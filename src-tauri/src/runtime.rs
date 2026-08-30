//! Orchestrates core + system proxy.

use crate::api::{ClashApi, ConnectionInfo, RequestRecord, TrafficTotals, XrayMetrics};
use crate::config::{
    build_mihomo_config, build_singbox_config, build_xray_config, build_xray_sidecar_config,
    generate_api_secret, inspect_singbox_config, outbound_tag, write_active_config,
    write_active_yaml_config, write_custom_config, write_xray_sidecar_config, BuildOptions,
    SidecarPlan,
};
use crate::core::manager::{CoreManager, CoreState};
use crate::core::resolve_core_bin;
use crate::core::CoreKind;
use crate::domain::{ChainHop, Protocol, ProxyNode, RuntimeSource, SubscriptionSource};
use crate::error::{AppError, AppResult};
use crate::proxy::{create_system_proxy, SystemProxy, SystemProxySnapshot};
use crate::storage::AppStore;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct ProxyStatus {
    pub running: bool,
    pub core_state: CoreState,
    pub system_proxy: bool,
    /// Whether TUN is enabled in settings (applied on next start / restart).
    pub tun_enabled: bool,
    /// Persisted desired capture mode: off | system | tun.
    pub capture_mode: String,
    /// rule | global | direct
    pub outbound_mode: String,
    pub mixed_port: u16,
    pub api_port: u16,
    pub current_node_id: Option<String>,
    pub error: Option<String>,
    pub core_path: Option<String>,
    pub config_path: Option<String>,
    /// bytes/s uplink (approx)
    pub upload_speed: u64,
    /// bytes/s downlink (approx)
    pub download_speed: u64,
    pub upload_total: u64,
    pub download_total: u64,
    pub connections: u32,
    /// Smart auto node switch enabled (derived from auto_select == smart).
    #[serde(default)]
    pub smart_switch: bool,
    /// off | smart | kernel
    #[serde(default)]
    pub auto_select: String,
    /// Unix seconds when the core last entered running state (for uptime UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_started_at: Option<i64>,
    /// `generated` or `singbox`.
    #[serde(default)]
    pub runtime_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_profile_name: Option<String>,
    #[serde(default)]
    pub custom_has_clash_api: bool,
    #[serde(default)]
    pub custom_has_tun: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_inbound_port: Option<u16>,
    /// Resident memory (bytes) of the core process, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_memory_bytes: Option<u64>,
    /// Which core is active: `singbox` (default) | `xray`.
    #[serde(default)]
    pub core_type: String,
    /// True when the running core has elevated privileges (macOS:
    /// setuid-root; Windows: UAC). Surfaced so the UI can flag it — an
    /// elevated core outlives a normal user-privilege boundary, so it's
    /// worth calling out rather than leaving silent.
    #[serde(default)]
    pub core_elevated: bool,
    /// Companion Xray sidecar process is running (sing-box main mode,
    /// `settings.multi_core_*` delegation). Never true under other cores.
    #[serde(default)]
    pub sidecar_running: bool,
}

/// Cap history to limit RAM (UI only needs recent activity).
const MAX_REQUEST_HISTORY: usize = 3_000;
const MAX_LIVE_REMOVAL_HISTORY: usize = 10_000;

/// Passive connection-journal stats for one outbound tag (smart switch Level 0).
#[derive(Debug, Clone, Default)]
pub struct PassiveNodeStats {
    /// Closed connections in the lookback window on this node.
    pub total: u32,
    /// Short-lived low-byte closes (proxy path often died early).
    pub suspicious: u32,
    /// Distinct destinations among all samples.
    pub dests: u32,
    /// Distinct destinations among suspicious samples.
    pub sus_dests: u32,
    /// Trailing consecutive suspicious closes (most recent first).
    pub consecutive_recent_sus: u32,
}

impl PassiveNodeStats {
    pub fn fail_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.suspicious as f64 / self.total as f64
        }
    }

    /// Soft degrade: enough samples, high fail rate, ≥2 bad destinations.
    pub fn soft_degraded(&self, min_samples: u32, fail_rate: f64) -> bool {
        self.total >= min_samples && self.fail_rate() >= fail_rate && self.sus_dests >= 2
    }

    /// Stronger passive signal: consecutive bad closes (multi-dest or long streak).
    pub fn hard_degraded(&self) -> bool {
        (self.consecutive_recent_sus >= 3 && self.sus_dests >= 2)
            || self.consecutive_recent_sus >= 5
    }
}

pub struct Runtime {
    pub core: CoreManager,
    /// Companion Xray sidecar process (sing-box main mode + delegation
    /// settings). Independent `CoreManager` instance — the struct has no
    /// static state, so a second one just owns a second child process.
    pub sidecar: CoreManager,
    /// Ports the sidecar currently owns (per-node loopback inbounds).
    sidecar_ports: Vec<u16>,
    pub system_proxy_on: bool,
    pub proxy_snapshot: Option<SystemProxySnapshot>,
    pub api: Option<ClashApi>,
    /// Xray metrics client (no Clash API exists under Xray).
    pub xray_metrics: Option<XrayMetrics>,
    pub last_config_path: Option<PathBuf>,
    pub last_binary_path: Option<PathBuf>,
    system_proxy: Box<dyn SystemProxy>,
    traffic_prev: Option<(Instant, TrafficTotals)>,
    traffic_speed: (u64, u64),
    /// Live connections (last poll)
    live_connections: Vec<ConnectionInfo>,
    live_revision: u64,
    /// Bumped only when the live id SET changes (adds/removes), not on plain
    /// traffic-counter updates — lets `live_connection_batch` skip the O(N)
    /// `order_ids` payload for pure-update deltas.
    live_order_revision: u64,
    live_item_revisions: HashMap<String, u64>,
    live_removals: VecDeque<(u64, String)>,
    live_diff_floor: u64,
    /// History of requests keyed by connection id (or synthetic key).
    request_by_id: HashMap<String, RequestRecord>,
    /// Newest ids at the front.
    request_order: VecDeque<String>,
    /// When journal / sample last applied a snapshot.
    last_sample_at: Option<Instant>,
    /// Monotonic journal sequence (opens).
    journal_seq: u64,
    /// Wall-clock start of current core session (unix secs).
    core_started_at: Option<i64>,
    /// Listen port taken from a user sing-box file (never from settings).
    custom_inbound_port: Option<u16>,
    custom_has_clash_api: bool,
    custom_has_tun: bool,
    /// Cached (pid, rss_bytes, is_root, fetched-at) from the last successful
    /// `ps`/`tasklist` process read — throttled since it shells out to a
    /// subprocess on macOS. `is_root` rides the same read (macOS: same `ps`
    /// call already open for rss; other platforms: `None`, no elevation
    /// signal there — see `core::memory`).
    core_memory_cache: Option<(u32, u64, Option<bool>, Instant)>,
}

/// Whether the *next* `start_proxy`/`restart_core` call for `store`'s current
/// settings would need macOS setuid elevation, and on which binary.
///
/// Read-only mirror of the `elevated` branches inside `start_xray_proxy` /
/// `start_mihomo_proxy` / the inline sing-box path / `start_custom_proxy`.
/// Callers use this to run `macos_auth::ensure_core_setuid` (which can block
/// for seconds on a Touch ID / password prompt) *before* taking the
/// runtime/store locks, so a slow or stalled system auth dialog can't hold
/// those locks and freeze every other command that needs them.
///
/// Returns `None` when no elevation is needed, or it is already granted
/// (`macos_auth::core_has_setuid` — cheap `stat`, safe to call here too).
#[cfg(target_os = "macos")]
pub fn resolve_pending_elevation(
    app_data_dir: &Path,
    resource_dir: Option<&Path>,
    store: &AppStore,
) -> Option<PathBuf> {
    let (kind, tun_enabled) = match store.settings.runtime_source() {
        RuntimeSource::Singbox { id } => {
            let content = store
                .subscriptions
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| match &s.source {
                    SubscriptionSource::Singbox { content } => Some(content.clone()),
                    _ => None,
                })?;
            let insight = inspect_singbox_config(&content);
            (CoreKind::SingBox, insight.has_tun)
        }
        RuntimeSource::Generated => (
            CoreKind::parse(&store.settings.core_type),
            store.settings.tun_enabled,
        ),
    };
    if !tun_enabled {
        return None;
    }
    let (bin, _src) = resolve_core_bin(app_data_dir, resource_dir, kind);
    let bin = bin?;
    if crate::core::core_has_setuid(&bin) {
        return None;
    }
    Some(bin)
}

/// Resolve the Clash API secret to build the next sing-box/mihomo config
/// with, honoring `api_secret_enabled`. Disabled → clears any stored secret
/// and returns empty (both cores treat an empty `secret` as no auth).
/// Enabled → reuses the persisted secret, generating one only if missing
/// (old stores predating this toggle, or a freshly-enabled one).
fn resolve_clash_api_secret(store: &mut AppStore) -> String {
    if !store.settings.api_secret_enabled {
        store.settings.clash_api_secret = None;
        return String::new();
    }
    let secret = store
        .settings
        .clash_api_secret
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(generate_api_secret);
    store.settings.clash_api_secret = Some(secret.clone());
    secret
}

impl Runtime {
    pub fn new() -> Self {
        Self {
            core: CoreManager::default(),
            sidecar: CoreManager::default(),
            sidecar_ports: Vec::new(),
            system_proxy_on: false,
            proxy_snapshot: None,
            api: None,
            xray_metrics: None,
            last_config_path: None,
            last_binary_path: None,
            system_proxy: create_system_proxy(),
            traffic_prev: None,
            traffic_speed: (0, 0),
            live_connections: Vec::new(),
            live_revision: 0,
            live_order_revision: 0,
            live_item_revisions: HashMap::new(),
            live_removals: VecDeque::new(),
            live_diff_floor: 0,
            request_by_id: HashMap::new(),
            request_order: VecDeque::new(),
            last_sample_at: None,
            journal_seq: 0,
            core_started_at: None,
            custom_inbound_port: None,
            custom_has_clash_api: false,
            custom_has_tun: false,
            core_memory_cache: None,
        }
    }

    /// Clone of current Clash API client (for journal I/O outside the lock).
    pub fn api_clone(&self) -> Option<ClashApi> {
        self.api.clone()
    }

    /// Clone of the Xray metrics client for the journal poller.
    pub fn xray_metrics_clone(&self) -> Option<XrayMetrics> {
        self.xray_metrics.clone()
    }

    /// Tail of the log file for a specific core kind. Under multi-core mode
    /// the two cores write to separate hourly files — the sidecar manager
    /// owns the companion's file, the main manager everyone else's. Whichever
    /// manager actually ran (or is running) the requested kind answers.
    pub fn core_log_tail_for(
        &self,
        kind: CoreKind,
        limit: usize,
    ) -> Option<(PathBuf, Vec<String>)> {
        if self.sidecar.kind() == kind {
            if let Some(tail) = self.sidecar.core_log_tail(limit) {
                return Some(tail);
            }
        }
        if self.core.kind() == kind {
            return self.core.core_log_tail(limit);
        }
        None
    }

    /// Truncate the current-hour log file of the given core kind (same
    /// manager resolution as [`Self::core_log_tail_for`]). No-op when that
    /// core never ran in this app instance.
    pub fn core_log_clear_for(&self, kind: CoreKind) -> AppResult<()> {
        if self.sidecar.kind() == kind {
            if self.sidecar.latest_log_path().is_some() {
                return self.sidecar.clear_log();
            }
        }
        if self.core.kind() == kind {
            return self.core.clear_log();
        }
        Ok(())
    }

    pub fn status(&mut self, store: &AppStore) -> ProxyStatus {
        self.core.poll();
        self.sidecar.poll();
        // Core may have exited outside stop_proxy — keep uptime field consistent.
        if !self.core.is_running() {
            self.core_started_at = None;
        } else if self.core_started_at.is_none() {
            // Recover if we missed setting it (e.g. process still up after soft restart path).
            self.core_started_at = Some(now_unix_secs());
        }
        let core_memory_bytes = self.core_memory_bytes();
        let core_elevated = self.core_elevated();
        ProxyStatus {
            running: self.core.is_running(),
            core_state: self.core.state(),
            system_proxy: self.system_proxy_on,
            tun_enabled: store.settings.tun_enabled,
            capture_mode: store.settings.capture_mode.as_str().to_string(),
            outbound_mode: store.settings.outbound_mode.as_str().to_string(),
            mixed_port: self
                .custom_inbound_port
                .unwrap_or(store.settings.mixed_port),
            api_port: store.settings.api_port,
            current_node_id: store.settings.current_node_id.clone(),
            error: self.core.last_error().map(|s| s.to_string()),
            core_path: self
                .last_binary_path
                .as_ref()
                .map(|p| p.display().to_string()),
            config_path: self
                .last_config_path
                .as_ref()
                .map(|p| p.display().to_string()),
            upload_speed: self.traffic_speed.0,
            download_speed: self.traffic_speed.1,
            upload_total: self
                .traffic_prev
                .as_ref()
                .map(|(_, t)| t.upload_total)
                .unwrap_or(0),
            download_total: self
                .traffic_prev
                .as_ref()
                .map(|(_, t)| t.download_total)
                .unwrap_or(0),
            connections: self
                .traffic_prev
                .as_ref()
                .map(|(_, t)| t.connections)
                .unwrap_or(0),
            smart_switch: store.settings.auto_select.is_smart(),
            auto_select: store.settings.auto_select.as_str().to_string(),
            core_started_at: self.core_started_at,
            runtime_source: match store.settings.runtime_source() {
                crate::domain::RuntimeSource::Generated => "generated".into(),
                crate::domain::RuntimeSource::Singbox { .. } => "singbox".into(),
            },
            runtime_profile_id: store
                .settings
                .runtime_source()
                .singbox_id()
                .map(ToString::to_string),
            runtime_profile_name: store.settings.runtime_source().singbox_id().and_then(|id| {
                store
                    .subscriptions
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.name.clone())
            }),
            custom_has_clash_api: self.custom_has_clash_api,
            custom_has_tun: self.custom_has_tun,
            custom_inbound_port: self.custom_inbound_port,
            core_memory_bytes,
            // Report the ACTUAL running kind: custom sing-box profiles always
            // run the sing-box binary even when settings.core_type is xray.
            core_type: if self.core.is_running() {
                self.core.kind().as_str().to_string()
            } else {
                store.settings.core_type.clone()
            },
            core_elevated,
            // The sidecar is only meaningful while the main core runs; a
            // stopped main core always reports it down regardless of a
            // lingering process (stop paths tear it down first anyway).
            sidecar_running: self.core.is_running() && self.sidecar.is_running(),
        }
    }

    /// RSS of the core process, if running and a PID is known.
    ///
    /// Read through `core::memory` — the OS's own process-table surface per
    /// platform, so it works even when sing-box runs elevated for TUN and an
    /// unprivileged parent could not open a handle/task port for it.
    ///
    /// Throttled to once every 5s since a read is non-trivial work on some
    /// platforms; callers poll far more often than that, so we serve the
    /// cached value in between.
    fn core_memory_bytes(&mut self) -> Option<u64> {
        self.core_mem_info().and_then(|i| i.rss_bytes)
    }

    /// True when the running core is currently root/admin (elevated).
    ///
    /// macOS: read from the live process every poll (`core::memory`'s `ps`
    /// call, piggybacked on the RSS read below) rather than remembered from
    /// how this app started it — setuid is a bit persisted on the binary,
    /// so a root sing-box can predate this app session entirely (started
    /// under old code, still running across an app restart, started
    /// outside this app). Windows has no such history: `CoreManager`'s
    /// `ElevatedPid` run mode is set only right after this app's own
    /// `run_elevated` call succeeds, so remembering it is exact there.
    fn core_elevated(&mut self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.core_mem_info()
                .and_then(|i| i.is_root)
                .unwrap_or(false)
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.core.is_windows_elevated()
        }
    }

    /// Cached process read (rss + root-ness) for the running core's PID,
    /// throttled to once every 5s since it shells out to a subprocess on
    /// macOS. `None` when no core PID is currently known.
    fn core_mem_info(&mut self) -> Option<crate::core::ProcessMemInfo> {
        let pid = self.core.pid()?;
        if let Some((cached_pid, bytes, is_root, at)) = self.core_memory_cache {
            if cached_pid == pid && at.elapsed() < Duration::from_secs(5) {
                return Some(crate::core::ProcessMemInfo {
                    rss_bytes: Some(bytes),
                    is_root,
                });
            }
        }
        let info = crate::core::read_process_mem_info(pid);
        if let Some(bytes) = info.rss_bytes {
            self.core_memory_cache = Some((pid, bytes, info.is_root, Instant::now()));
        }
        Some(info)
    }

    /// Passive health for smart switch from connection journal (no MITM / no HTTP codes).
    ///
    /// Heuristic "suspicious": closed within 3s with almost no bytes — proxy path often
    /// dies before useful transfer. Multi-destination and consecutive tail reduce
    /// single-site false positives (docs/auto.md).
    pub fn passive_node_stats(&self, node_tag: &str, lookback_ms: i64) -> PassiveNodeStats {
        let now = now_unix_ms();
        let mut samples: Vec<(i64, bool, String)> = Vec::new(); // closed_at, sus, dest_key

        for rec in self.request_by_id.values() {
            if !rec.closed {
                continue;
            }
            let closed_at = rec.closed_at.unwrap_or(rec.last_seen);
            if now.saturating_sub(closed_at) > lookback_ms {
                continue;
            }
            let matches = rec.node == node_tag
                || rec.chains.iter().any(|c| c == node_tag)
                || (!node_tag.is_empty() && rec.node.contains(node_tag));
            if !matches {
                continue;
            }
            let dur = closed_at.saturating_sub(rec.first_seen);
            let sus = dur <= 3000 && rec.download < 1024 && rec.upload < 1024;
            let dest = if !rec.host.is_empty() {
                rec.host.clone()
            } else if !rec.destination.is_empty() && rec.destination != "—" {
                rec.destination.clone()
            } else {
                "unknown".into()
            };
            samples.push((closed_at, sus, dest));
        }

        samples.sort_by_key(|(t, _, _)| *t);

        let mut all_dests = HashSet::new();
        let mut sus_dests = HashSet::new();
        let mut suspicious = 0u32;
        for (_, sus, dest) in &samples {
            all_dests.insert(dest.clone());
            if *sus {
                suspicious = suspicious.saturating_add(1);
                sus_dests.insert(dest.clone());
            }
        }

        // Consecutive suspicious at the most recent end of the window.
        let mut consecutive_recent_sus = 0u32;
        for (_, sus, _) in samples.iter().rev() {
            if *sus {
                consecutive_recent_sus = consecutive_recent_sus.saturating_add(1);
            } else {
                break;
            }
        }

        PassiveNodeStats {
            total: samples.len() as u32,
            suspicious,
            dests: all_dests.len() as u32,
            sus_dests: sus_dests.len() as u32,
            consecutive_recent_sus,
        }
    }

    /// Apply a pre-fetched snapshot (journal / HTTP fallback). Prefer calling I/O outside the lock.
    pub fn apply_snapshot(&mut self, snap: crate::api::ConnectionsSnapshot) {
        let now = Instant::now();
        let totals = TrafficTotals {
            upload_total: snap.upload_total,
            download_total: snap.download_total,
            connections: snap.connections.len() as u32,
        };
        if let Some((prev_t, prev)) = &self.traffic_prev {
            let dt = now.duration_since(*prev_t).as_secs_f64();
            if dt > 0.05 {
                let up = totals.upload_total.saturating_sub(prev.upload_total);
                let down = totals.download_total.saturating_sub(prev.download_total);
                self.traffic_speed = ((up as f64 / dt) as u64, (down as f64 / dt) as u64);
            }
        }
        self.traffic_prev = Some((now, totals));
        self.ingest_connections(snap.connections);
        self.last_sample_at = Some(now);
    }

    /// Diff-based journal: upsert live, mark disappeared as closed.
    fn ingest_connections(&mut self, connections: Vec<ConnectionInfo>) {
        let now_ms = now_unix_ms();
        let mut seen: HashSet<String> = HashSet::with_capacity(connections.len());

        for c in &connections {
            let id = connection_history_key(c);
            seen.insert(id.clone());
            if let Some(rec) = self.request_by_id.get_mut(&id) {
                rec.last_seen = now_ms;
                rec.closed = false;
                rec.closed_at = None;
                rec.upload = c.upload.max(rec.upload);
                rec.download = c.download.max(rec.download);
                if !c.node.is_empty() && c.node != "—" {
                    rec.node = c.node.clone();
                }
                if !c.chains.is_empty() {
                    rec.chains = c.chains.clone();
                }
                if rec.destination == "—" && c.destination != "—" {
                    rec.destination = c.destination.clone();
                }
                if rec.host.is_empty() && !c.host.is_empty() {
                    rec.host = c.host.clone();
                }
                if rec.rule.is_empty() && !c.rule.is_empty() {
                    rec.rule = c.rule.clone();
                    rec.rule_payload = c.rule_payload.clone();
                }
                if rec.process.is_empty() && !c.process.is_empty() {
                    rec.process = c.process.clone();
                }
            } else {
                let mut rec = RequestRecord::from_connection(c, now_ms);
                rec.id = id.clone();
                self.request_by_id.insert(id.clone(), rec);
                self.request_order.push_front(id);
                while self.request_order.len() > MAX_REQUEST_HISTORY {
                    if let Some(old) = self.request_order.pop_back() {
                        self.request_by_id.remove(&old);
                    }
                }
            }
        }

        // Connections that left the live snapshot → Closed event in journal.
        for prev in &self.live_connections {
            let id = connection_history_key(prev);
            if !seen.contains(&id) {
                if let Some(rec) = self.request_by_id.get_mut(&id) {
                    if !rec.closed {
                        self.journal_seq = self.journal_seq.saturating_add(1);
                        rec.history_seq = self.journal_seq;
                        rec.closed = true;
                        rec.closed_at = Some(now_ms);
                        rec.last_seen = now_ms;
                    }
                }
            }
        }

        if self.live_connections != connections {
            self.live_revision = self.live_revision.saturating_add(1);
            let revision = self.live_revision;
            let previous: HashMap<String, &ConnectionInfo> = self
                .live_connections
                .iter()
                .map(|connection| (connection_history_key(connection), connection))
                .collect();
            let mut membership_changed = false;
            for connection in &connections {
                let id = connection_history_key(connection);
                let is_new = previous.get(&id).is_none();
                if previous.get(&id).is_none_or(|old| *old != connection) {
                    self.live_item_revisions.insert(id, revision);
                    if is_new {
                        membership_changed = true;
                    }
                }
            }
            for id in previous.keys() {
                if !seen.contains(id) {
                    membership_changed = true;
                    self.live_item_revisions.remove(id);
                    self.live_removals.push_back((revision, id.clone()));
                }
            }
            if membership_changed {
                self.live_order_revision = self.live_order_revision.saturating_add(1);
            }
            while self.live_removals.len() > MAX_LIVE_REMOVAL_HISTORY {
                if let Some((removed_revision, _)) = self.live_removals.pop_front() {
                    self.live_diff_floor = removed_revision;
                }
            }
            self.live_connections = connections;
        }
    }

    pub fn live_connections(&mut self, store: &AppStore) -> Vec<ConnectionView> {
        self.core.poll();
        let tag_info = node_tag_info_map(store);
        self.live_connections
            .iter()
            .map(|c| ConnectionView::from_info(c, &tag_info))
            .collect()
    }

    pub fn live_connection_batch(
        &mut self,
        store: &AppStore,
        since_revision: Option<u64>,
        last_order_revision: Option<u64>,
    ) -> LiveConnectionBatch {
        self.core.poll();
        if since_revision == Some(self.live_revision) {
            return LiveConnectionBatch {
                rows: Vec::new(),
                removed_ids: Vec::new(),
                order_ids: None,
                order_revision: self.live_order_revision,
                revision: self.live_revision,
                unchanged: true,
                full: false,
            };
        }
        let full = since_revision.is_none_or(|since| since < self.live_diff_floor);
        let since = since_revision.unwrap_or(0);
        let tag_info = node_tag_info_map(store);
        // Order payload is O(N); skip it when the client's order revision is
        // current — pure traffic-counter deltas then merge in place on the
        // client without rebuilding the whole array.
        let order_ids = if full || last_order_revision != Some(self.live_order_revision) {
            Some(
                self.live_connections
                    .iter()
                    .map(connection_history_key)
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };
        LiveConnectionBatch {
            rows: self
                .live_connections
                .iter()
                .filter(|connection| {
                    full || self
                        .live_item_revisions
                        .get(&connection_history_key(connection))
                        .is_some_and(|revision| *revision > since)
                })
                .map(|connection| ConnectionView::from_info(connection, &tag_info))
                .collect(),
            removed_ids: if full {
                Vec::new()
            } else {
                self.live_removals
                    .iter()
                    .filter(|(revision, _)| *revision > since)
                    .map(|(_, id)| id.clone())
                    .collect()
            },
            order_ids,
            order_revision: self.live_order_revision,
            revision: self.live_revision,
            unchanged: false,
            full,
        }
    }

    pub fn request_history(
        &mut self,
        store: &AppStore,
        query: Option<&str>,
        limit: Option<usize>,
        after_seq: Option<u64>,
    ) -> RequestBatch {
        self.request_batch(store, query, limit, after_seq, false)
    }

    /// Closed requests that look like failures / timeouts: short-lived (≤ 3s)
    /// with almost no bytes transferred — same heuristic used by the passive
    /// smart-switch health check (see `passive_node_stats`).
    pub fn request_failures(
        &mut self,
        store: &AppStore,
        query: Option<&str>,
        limit: Option<usize>,
        after_seq: Option<u64>,
    ) -> RequestBatch {
        self.request_batch(store, query, limit, after_seq, true)
    }

    fn request_batch(
        &mut self,
        store: &AppStore,
        query: Option<&str>,
        limit: Option<usize>,
        after_seq: Option<u64>,
        failures_only: bool,
    ) -> RequestBatch {
        self.core.poll();
        let tag_info = node_tag_info_map(store);
        let q = query.unwrap_or("").trim();
        let limit = limit.unwrap_or(800).min(MAX_REQUEST_HISTORY);
        let is_failure = |record: &RequestRecord| {
            if !failures_only {
                return true;
            }
            let closed_at = record.closed_at.unwrap_or(record.last_seen);
            let duration = closed_at.saturating_sub(record.first_seen);
            duration <= 3000 && record.download < 1024 && record.upload < 1024
        };

        if let Some(after_seq) = after_seq {
            let mut records: Vec<&RequestRecord> = self
                .request_by_id
                .values()
                .filter(|record| record.closed && record.history_seq > after_seq)
                .collect();
            records.sort_unstable_by_key(|record| record.history_seq);
            let mut entries = Vec::new();
            let mut cursor = after_seq;
            let mut hit_limit = false;
            for record in records {
                cursor = record.history_seq;
                if is_failure(record) && record.matches_query(q) {
                    entries.push(ConnectionView::from_record(record, &tag_info));
                    if entries.len() >= limit {
                        hit_limit = true;
                        break;
                    }
                }
            }
            if !hit_limit {
                cursor = self.journal_seq;
            }
            return RequestBatch { entries, cursor };
        }

        let entries = self
            .request_order
            .iter()
            .filter_map(|id| self.request_by_id.get(id))
            .filter(|record| record.closed)
            .filter(|record| is_failure(record))
            .filter(|record| record.matches_query(q))
            .take(limit)
            .map(|record| ConnectionView::from_record(record, &tag_info))
            .collect();
        RequestBatch {
            entries,
            cursor: self.journal_seq,
        }
    }

    pub fn clear_request_history(&mut self) {
        // Keep active records so they can still transition into the closed
        // list when they disappear from a later connection snapshot.
        self.request_by_id.retain(|_, record| !record.closed);
        let active_ids: HashSet<String> = self.request_by_id.keys().cloned().collect();
        self.request_order.retain(|id| active_ids.contains(id));
    }

    pub fn clash_api_clone(&self) -> Option<ClashApi> {
        self.api_clone()
    }

    /// Startup-failure diagnostics shared by all core start paths: prefer the
    /// manager's captured error tail, else the last 1200 chars of the core's
    /// own log file (NULs stripped). Empty when the core said nothing.
    fn core_startup_log_hint(&self) -> String {
        self.core
            .last_error()
            .map(|s| s.to_string())
            .or_else(|| {
                self.core
                    .log_path()
                    .and_then(|log| std::fs::read(log).ok())
                    .and_then(|b| {
                        let s = String::from_utf8_lossy(&b);
                        let tail: String = s
                            .chars()
                            .rev()
                            .take(1200)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect();
                        let cleaned = tail.replace('\0', "");
                        if cleaned.trim().is_empty() {
                            None
                        } else {
                            Some(cleaned)
                        }
                    })
            })
            .unwrap_or_default()
    }

    /// Generate config, start the active core, optionally enable system proxy.
    pub fn start_proxy(
        &mut self,
        app_data_dir: &Path,
        resource_dir: Option<&Path>,
        store: &mut AppStore,
        enable_system_proxy: bool,
    ) -> AppResult<ProxyStatus> {
        self.core.poll();
        if self.core.is_running() {
            return Ok(self.status(store));
        }

        match store.settings.runtime_source() {
            RuntimeSource::Singbox { id } => {
                return self.start_custom_proxy(
                    app_data_dir,
                    resource_dir,
                    store,
                    &id,
                    enable_system_proxy,
                );
            }
            RuntimeSource::Generated => {}
        }

        match CoreKind::parse(&store.settings.core_type) {
            CoreKind::Xray => {
                return self.start_xray_proxy(
                    app_data_dir,
                    resource_dir,
                    store,
                    enable_system_proxy,
                )
            }
            CoreKind::Mihomo => {
                return self.start_mihomo_proxy(
                    app_data_dir,
                    resource_dir,
                    store,
                    enable_system_proxy,
                )
            }
            CoreKind::SingBox => {}
        }

        self.custom_inbound_port = None;
        self.custom_has_clash_api = false;
        self.custom_has_tun = false;

        let nodes = store.enabled_nodes();
        if nodes.is_empty() {
            return Err(AppError::Core(
                "no nodes; import a subscription first".into(),
            ));
        }

        // Xray sidecar delegation (sing-box main mode only). Resolved before
        // any build work: the generated sing-box config embeds socks
        // outbounds pointing at the sidecar ports, so a missing Xray binary
        // must fail the start up front rather than half-start into
        // references to a process that will never exist.
        //
        // Port occupancy is NOT probed per port here — with hundreds of
        // delegated nodes that would spawn hundreds of lsof/netstat child
        // processes. Known ports (mixed/api/extra/diag) are already excluded
        // at plan time; anything else on a sidecar port is cleared by the
        // standard `ensure_ports_free` inside `start_with_ports`, and a real
        // bind conflict surfaces as the sidecar's own FATAL → full rollback.
        let sidecar_plan = compute_sidecar_plan(&store.settings, &store.chains, &nodes);
        if sidecar_plan.is_some() {
            let (xbin, _) = resolve_core_bin(app_data_dir, resource_dir, CoreKind::Xray);
            if xbin.is_none() {
                return Err(AppError::Core(
                    "Xray 副进程已启用但未找到 Xray 内核：请先在设置中下载 Xray，或关闭协议委托"
                        .into(),
                ));
            }
        }

        ensure_listen_port_available_on(
            store.settings.mixed_port,
            if store.settings.allow_lan {
                "0.0.0.0"
            } else {
                "127.0.0.1"
            },
            "Mixed",
        )?;
        ensure_listen_port_available(store.settings.api_port, "Clash API")?;
        for inb in &store.settings.extra_inbounds {
            ensure_listen_port_available_on(
                inb.port,
                if inb.allow_lan {
                    "0.0.0.0"
                } else {
                    "127.0.0.1"
                },
                "Inbound",
            )?;
        }

        let (bin, _src) = resolve_core_bin(app_data_dir, resource_dir, CoreKind::SingBox);
        let bin = bin.ok_or_else(|| AppError::Core("sing-box binary not found".into()))?;

        // Reuse the persisted clash_api secret so it survives restarts, or
        // clear it when the user has the secret toggle off (see
        // resolve_clash_api_secret / api_secret_enabled).
        let secret = resolve_clash_api_secret(store);
        let mut opts = build_options(store, secret.clone());
        opts.sidecar = sidecar_plan.clone();
        let built = build_singbox_config(&nodes, &opts)?;
        let config_path = write_active_config(app_data_dir, &built)?;
        if store.settings.current_node_id.is_none() {
            if let Some(first) = nodes.first() {
                store.settings.current_node_id = Some(first.id.clone());
            }
        }

        let log_dir = app_data_dir.join("logs");
        // TUN creates utun + routes → macOS setuid sing-box / Windows UAC.
        let elevated = store.settings.tun_enabled;
        self.core.start_with_ports(
            CoreKind::SingBox,
            &bin,
            &config_path,
            &log_dir,
            store.settings.mixed_port,
            Some(store.settings.api_port),
            &store
                .settings
                .extra_inbounds
                .iter()
                .map(|inb| inb.port)
                .collect::<Vec<_>>(),
            elevated,
            resource_dir,
        )?;
        self.last_config_path = Some(config_path.clone());
        self.last_binary_path = Some(bin.clone());

        let api = ClashApi::new("127.0.0.1", store.settings.api_port, &secret);
        // TUN start can take a few seconds (utun + routes). Health uses a short
        // per-try timeout so we do not block the runtime lock for minutes.
        let max_wait = if elevated {
            Duration::from_secs(10)
        } else {
            Duration::from_secs(6)
        };
        let wait_started = Instant::now();
        let mut ok = false;
        while wait_started.elapsed() < max_wait {
            if api.health_ok() {
                ok = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            self.core.poll();
            if !self.core.is_running() {
                break;
            }
        }
        if !ok {
            let log_hint = self.core_startup_log_hint();
            let _ = self.core.stop();
            let detail = if log_hint.is_empty() {
                format!(
                    "sing-box started but clash_api not responding at 127.0.0.1:{}",
                    store.settings.api_port
                )
            } else {
                format!(
                    "sing-box started but clash_api not responding at 127.0.0.1:{}\n--- log ---\n{log_hint}",
                    store.settings.api_port
                )
            };
            return Err(AppError::Core(detail));
        }
        self.api = Some(api);
        self.core_started_at = Some(now_unix_secs());

        // Main core healthy — now bring up the companion Xray sidecar (if
        // delegated). Failure fails the whole start and rolls the main core
        // back: its outbounds already point at the sidecar ports, so
        // leaving it running would black-hole delegated nodes.
        if let Some(plan) = &sidecar_plan {
            if let Err(e) = self.start_xray_sidecar(app_data_dir, resource_dir, &nodes, plan) {
                let _ = self.core.stop();
                self.core_started_at = None;
                self.api = None;
                return Err(e);
            }
        }

        // System proxy is independent — optional on start; prefer UI switch after running.
        if enable_system_proxy {
            if let Err(e) = self.set_system_proxy(store, true) {
                // Core stays up; surface error to caller as soft fail via Err still?
                // Keep core running: return Ok and leave system_proxy off.
                let _ = e;
            }
        }

        Ok(self.status(store))
    }

    /// Build, write and start the companion Xray sidecar process for the
    /// delegation plan. Called only after the main sing-box core is healthy.
    ///
    /// `start_with_ports` itself waits until the first sidecar port is
    /// listening (and errors if the process dies), so a successful return
    /// means every delegated node's inbound is live. Config validation runs
    /// via Xray's own `-test` (`CoreKind::check_command_args`); the sidecar
    /// config deliberately contains no geodata references, so the test never
    /// needs the geosite/geoip assets.
    fn start_xray_sidecar(
        &mut self,
        app_data_dir: &Path,
        resource_dir: Option<&Path>,
        nodes: &[ProxyNode],
        plan: &SidecarPlan,
    ) -> AppResult<()> {
        let entries: Vec<(ProxyNode, u16)> = plan
            .ports
            .iter()
            .filter_map(|(id, port)| {
                nodes
                    .iter()
                    .find(|n| &n.id == id)
                    .map(|n| (n.clone(), *port))
            })
            .collect();
        let built = build_xray_sidecar_config(&entries)?;
        let config_path = write_xray_sidecar_config(app_data_dir, &built)?;
        let (bin, _src) = resolve_core_bin(app_data_dir, resource_dir, CoreKind::Xray);
        let bin = bin.ok_or_else(|| AppError::Core("Xray sidecar binary not found".into()))?;
        let log_dir = app_data_dir.join("logs");

        let mut ports = plan.port_list();
        let first = ports.remove(0);
        // No elevated path: the sidecar only binds loopback socks ports.
        if let Err(e) = self.sidecar.start_with_ports(
            CoreKind::Xray,
            &bin,
            &config_path,
            &log_dir,
            first,
            None,
            &ports,
            false,
            resource_dir,
        ) {
            let _ = self.sidecar.stop();
            let hint = self.sidecar.last_error().unwrap_or_default();
            return Err(AppError::Core(format!(
                "Xray 副进程启动失败：{e}{}",
                if hint.is_empty() {
                    String::new()
                } else {
                    format!("\n{hint}")
                }
            )));
        }
        self.sidecar_ports = plan.port_list();
        crate::app_log::info(
            "xray_sidecar",
            format!(
                "Xray 副进程已启动：{} 个委托节点，端口 {}..={}",
                plan.ports.len(),
                self.sidecar_ports.first().copied().unwrap_or(0),
                self.sidecar_ports.last().copied().unwrap_or(0),
            ),
        );
        Ok(())
    }

    /// Generate an Xray config and start the Xray core. Mirrors the sing-box
    /// generated path; readiness is process-alive + mixed port (no Clash API
    /// to health-check), and traffic monitoring switches to `XrayMetrics`.
    fn start_xray_proxy(
        &mut self,
        app_data_dir: &Path,
        resource_dir: Option<&Path>,
        store: &mut AppStore,
        enable_system_proxy: bool,
    ) -> AppResult<ProxyStatus> {
        self.custom_inbound_port = None;
        self.custom_has_clash_api = false;
        self.custom_has_tun = false;

        let nodes = store.enabled_nodes();
        if nodes.is_empty() {
            return Err(AppError::Core(
                "no nodes; import a subscription first".into(),
            ));
        }
        // Xray cannot serve every protocol. Mixed subscriptions are fine —
        // build_xray_config skips incompatible nodes with a warning — but a
        // subscription with zero compatible nodes cannot run at all.
        let supported_count = nodes
            .iter()
            .filter(|n| CoreKind::Xray.supports(n.protocol))
            .count();
        if supported_count == 0 {
            return Err(AppError::Config(
                "当前启用的节点均不被 Xray 内核支持（仅支持 vmess/vless/shadowsocks/trojan/hysteria2(无 obfs)/socks5/http/wireguard）。请导入兼容订阅或切回 sing-box 内核".into(),
            ));
        }
        let unsupported: Vec<&str> = {
            let mut kinds: Vec<&str> = nodes
                .iter()
                .filter(|n| !CoreKind::Xray.supports(n.protocol))
                .map(|n| n.protocol.as_str())
                .collect();
            kinds.sort_unstable();
            kinds.dedup();
            kinds
        };
        if !unsupported.is_empty() {
            let skipped = nodes.len() - supported_count;
            crate::app_log::warn(
                "xray_config",
                format!(
                    "Xray 内核跳过 {skipped} 个不支持协议（{}）的节点",
                    unsupported.join("/")
                ),
            );
        }

        ensure_listen_port_available_on(
            store.settings.mixed_port,
            if store.settings.allow_lan {
                "0.0.0.0"
            } else {
                "127.0.0.1"
            },
            "Mixed",
        )?;
        ensure_listen_port_available(store.settings.api_port, "Metrics")?;
        for inb in &store.settings.extra_inbounds {
            ensure_listen_port_available_on(
                inb.port,
                if inb.allow_lan {
                    "0.0.0.0"
                } else {
                    "127.0.0.1"
                },
                "Inbound",
            )?;
        }

        let (bin, _src) = resolve_core_bin(app_data_dir, resource_dir, CoreKind::Xray);
        let bin = bin.ok_or_else(|| {
            AppError::Core("Xray binary not found; download it on the Settings core tab".into())
        })?;

        // geosite:/geoip: matchers (and the tun adapter on Windows) resolve
        // through asset files next to the binary.
        crate::core::ensure_geodata(app_data_dir, resource_dir, None)?;
        #[cfg(target_os = "windows")]
        if store.settings.tun_enabled {
            crate::core::ensure_wintun(app_data_dir, resource_dir, None)?;
        }

        // `mut` is only consumed by the macOS utun pick below.
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
        let mut xray_opts = build_options(store, String::new());
        #[cfg(target_os = "macos")]
        if store.settings.tun_enabled {
            xray_opts.tun_interface_name = Some(pick_free_darwin_utun_name());
        }
        let built = build_xray_config(&nodes, &xray_opts)?;
        let config_path = write_active_config(app_data_dir, &built)?;
        // The generator falls back to the first supported node when the
        // persisted pick is incompatible (or absent) — mirror that here so
        // the UI's current node matches what the config actually routes
        // through, and node switching stays on a usable node.
        let needs_pick = store
            .settings
            .current_node_id
            .as_deref()
            .map(|id| {
                nodes
                    .iter()
                    .find(|n| n.id == id)
                    .is_some_and(|n| !CoreKind::Xray.supports(n.protocol))
            })
            .unwrap_or(true);
        if needs_pick {
            if let Some(first) = nodes.iter().find(|n| CoreKind::Xray.supports(n.protocol)) {
                store.settings.current_node_id = Some(first.id.clone());
            }
        }

        let log_dir = app_data_dir.join("logs");
        let elevated = store.settings.tun_enabled;
        self.core.start_with_ports(
            CoreKind::Xray,
            &bin,
            &config_path,
            &log_dir,
            store.settings.mixed_port,
            Some(store.settings.api_port),
            &store
                .settings
                .extra_inbounds
                .iter()
                .map(|inb| inb.port)
                .collect::<Vec<_>>(),
            elevated,
            resource_dir,
        )?;
        self.last_config_path = Some(config_path.clone());
        self.last_binary_path = Some(bin.clone());
        self.api = None;
        let metrics = XrayMetrics::new("127.0.0.1", store.settings.api_port);
        // Confirm the metrics module is serving; a miss doesn't fail the
        // start (proxying works without stats) but gets logged for diagnosis.
        let wait_started = Instant::now();
        let mut metrics_ok = false;
        let mut died_during_wait = false;
        while wait_started.elapsed() < Duration::from_secs(3) {
            if metrics.health_ok() {
                metrics_ok = true;
                break;
            }
            self.core.poll();
            if !self.core.is_running() {
                died_during_wait = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        if died_during_wait {
            // Mirrors the sing-box/mihomo health-wait failure path: the core
            // died after `wait_until_ready` accepted it — surface its log tail
            // instead of reporting a successful start the watchdog must undo.
            let log_hint = self.core_startup_log_hint();
            let _ = self.core.stop();
            let detail = if log_hint.is_empty() {
                "xray exited during startup".to_string()
            } else {
                format!("xray exited during startup\n--- log ---\n{log_hint}")
            };
            return Err(AppError::Core(detail));
        }
        if !metrics_ok {
            crate::app_log::warn(
                "xray_metrics",
                format!(
                    "metrics not responding at 127.0.0.1:{} (traffic stats may be blank)",
                    store.settings.api_port
                ),
            );
        }
        self.xray_metrics = Some(metrics);
        self.core_started_at = Some(now_unix_secs());

        if enable_system_proxy {
            let _ = self.set_system_proxy(store, true);
        }

        Ok(self.status(store))
    }

    /// Generate a Clash YAML config and start the mihomo core. Mirrors the
    /// sing-box generated path — mihomo serves a Clash-compatible API, so the
    /// same ClashApi health-check / hot-switch / conn-journal machinery is
    /// reused unchanged.
    fn start_mihomo_proxy(
        &mut self,
        app_data_dir: &Path,
        resource_dir: Option<&Path>,
        store: &mut AppStore,
        enable_system_proxy: bool,
    ) -> AppResult<ProxyStatus> {
        self.custom_inbound_port = None;
        self.custom_has_clash_api = false;
        self.custom_has_tun = false;

        let nodes = store.enabled_nodes();
        if nodes.is_empty() {
            return Err(AppError::Core(
                "no nodes; import a subscription first".into(),
            ));
        }
        // A core may not serve every protocol. Mixed subscriptions are fine —
        // build_mihomo_config skips incompatible nodes (and vmess non-tcp/ws
        // transports) with a warning — but zero compatible nodes cannot run.
        let supported_count = nodes
            .iter()
            .filter(|n| CoreKind::Mihomo.supports_node(n))
            .count();
        if supported_count == 0 {
            return Err(AppError::Config(
                "当前启用的节点均不被 mihomo 内核支持（支持 ss/vmess/vless/trojan/hysteria(2)/tuic/wireguard/anytls/snell/socks5/http/ssh）。请导入兼容订阅或切换内核".into(),
            ));
        }
        let unsupported: Vec<&str> = {
            let mut kinds: Vec<&str> = nodes
                .iter()
                .filter(|n| !CoreKind::Mihomo.supports_node(n))
                .map(|n| n.protocol.as_str())
                .collect();
            kinds.sort_unstable();
            kinds.dedup();
            kinds
        };
        if !unsupported.is_empty() {
            let skipped = nodes.len() - supported_count;
            crate::app_log::warn(
                "mihomo_config",
                format!(
                    "mihomo 内核跳过 {skipped} 个不支持协议（{}）的节点",
                    unsupported.join("/")
                ),
            );
        }

        ensure_listen_port_available_on(
            store.settings.mixed_port,
            if store.settings.allow_lan {
                "0.0.0.0"
            } else {
                "127.0.0.1"
            },
            "Mixed",
        )?;
        ensure_listen_port_available(store.settings.api_port, "Clash API")?;
        for inb in &store.settings.extra_inbounds {
            ensure_listen_port_available_on(
                inb.port,
                if inb.allow_lan {
                    "0.0.0.0"
                } else {
                    "127.0.0.1"
                },
                "Inbound",
            )?;
        }

        let (bin, _src) = resolve_core_bin(app_data_dir, resource_dir, CoreKind::Mihomo);
        let bin = bin.ok_or_else(|| {
            AppError::Core("mihomo binary not found; download it on the Settings core tab".into())
        })?;

        // GEOSITE/GEOIP rules hard-fail the core when the geodata files are
        // missing (mihomo's own auto-download dials through the not-yet-started
        // proxies) — ensure them first: staged → bundled → direct download.
        crate::core::ensure_mihomo_geodata(app_data_dir, resource_dir, None)?;

        // Reuse the persisted clash_api secret (same policy as sing-box).
        let secret = resolve_clash_api_secret(store);
        let built = build_mihomo_config(&nodes, &build_options(store, secret.clone()))?;
        let config_path = write_active_yaml_config(app_data_dir, &built.yaml)?;
        // Mirror the selected node onto the store when the persisted pick is
        // absent or incompatible (the generator falls back likewise), so the
        // UI and config agree and switching stays on a usable node.
        let needs_pick = store
            .settings
            .current_node_id
            .as_deref()
            .map(|id| {
                nodes
                    .iter()
                    .find(|n| n.id == id)
                    .is_some_and(|n| !CoreKind::Mihomo.supports_node(n))
            })
            .unwrap_or(true);
        if needs_pick {
            if let Some(first) = nodes.iter().find(|n| CoreKind::Mihomo.supports_node(n)) {
                store.settings.current_node_id = Some(first.id.clone());
            }
        }

        let log_dir = app_data_dir.join("logs");
        let elevated = store.settings.tun_enabled;
        self.core.start_with_ports(
            CoreKind::Mihomo,
            &bin,
            &config_path,
            &log_dir,
            store.settings.mixed_port,
            Some(store.settings.api_port),
            &store
                .settings
                .extra_inbounds
                .iter()
                .map(|inb| inb.port)
                .collect::<Vec<_>>(),
            elevated,
            resource_dir,
        )?;
        self.last_config_path = Some(config_path.clone());
        self.last_binary_path = Some(bin.clone());

        let api = ClashApi::new("127.0.0.1", store.settings.api_port, &secret);
        let max_wait = if elevated {
            Duration::from_secs(10)
        } else {
            Duration::from_secs(6)
        };
        let wait_started = Instant::now();
        let mut ok = false;
        while wait_started.elapsed() < max_wait {
            if api.health_ok() {
                ok = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            self.core.poll();
            if !self.core.is_running() {
                break;
            }
        }
        if !ok {
            let log_hint = self.core_startup_log_hint();
            let _ = self.core.stop();
            let detail = if log_hint.is_empty() {
                format!(
                    "mihomo started but clash api not responding at 127.0.0.1:{}",
                    store.settings.api_port
                )
            } else {
                format!(
                    "mihomo started but clash api not responding at 127.0.0.1:{}\n--- log ---\n{log_hint}",
                    store.settings.api_port
                )
            };
            return Err(AppError::Core(detail));
        }
        self.api = Some(api);
        self.xray_metrics = None;
        self.core_started_at = Some(now_unix_secs());

        if enable_system_proxy {
            let _ = self.set_system_proxy(store, true);
        }

        Ok(self.status(store))
    }

    fn start_custom_proxy(
        &mut self,
        app_data_dir: &Path,
        resource_dir: Option<&Path>,
        store: &mut AppStore,
        profile_id: &str,
        enable_system_proxy: bool,
    ) -> AppResult<ProxyStatus> {
        let (name, content) = store
            .subscriptions
            .iter()
            .find(|s| s.id == profile_id)
            .and_then(|s| match &s.source {
                SubscriptionSource::Singbox { content } => Some((s.name.clone(), content.clone())),
                _ => None,
            })
            .ok_or_else(|| AppError::Core("selected sing-box profile was not found".into()))?;
        let _ = name;

        crate::subscription::validate_complete_singbox_config(&content)?;
        let insight = inspect_singbox_config(&content);
        let config_path = write_custom_config(app_data_dir, profile_id, &content)?;

        if let Some(port) = insight.inbound_port {
            ensure_listen_port_available(port, "Inbound")?;
        }
        if let Some(port) = insight.clash_api_port {
            ensure_listen_port_available(port, "Clash API")?;
        }

        let (bin, _src) = resolve_core_bin(app_data_dir, resource_dir, CoreKind::SingBox);
        let bin = bin.ok_or_else(|| AppError::Core("sing-box binary not found".into()))?;

        let log_dir = app_data_dir.join("logs");
        let elevated = insight.has_tun;
        self.core.start_with_ports(
            CoreKind::SingBox,
            &bin,
            &config_path,
            &log_dir,
            insight.inbound_port.unwrap_or(0),
            insight.clash_api_port,
            &[],
            elevated,
            resource_dir,
        )?;
        self.last_config_path = Some(config_path.clone());
        self.last_binary_path = Some(bin.clone());
        self.custom_inbound_port = insight.inbound_port;
        self.custom_has_clash_api = insight.has_clash_api();
        self.custom_has_tun = insight.has_tun;

        if insight.has_clash_api() {
            let host = insight.clash_api_host.as_deref().unwrap_or("127.0.0.1");
            let port = insight.clash_api_port.unwrap_or(9090);
            let secret = insight.clash_api_secret.clone().unwrap_or_default();
            let api = ClashApi::new(host, port, &secret);
            let max_wait = if elevated {
                Duration::from_secs(10)
            } else {
                Duration::from_secs(6)
            };
            let wait_started = Instant::now();
            let mut ok = false;
            while wait_started.elapsed() < max_wait {
                if api.health_ok() {
                    ok = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
                self.core.poll();
                if !self.core.is_running() {
                    break;
                }
            }
            if !ok {
                let log_hint = self.core_startup_log_hint();
                let _ = self.core.stop();
                let detail = if log_hint.is_empty() {
                    format!("sing-box started but clash_api not responding at {host}:{port}")
                } else {
                    format!(
                        "sing-box started but clash_api not responding at {host}:{port}\n--- log ---\n{log_hint}"
                    )
                };
                return Err(AppError::Core(detail));
            }
            self.api = Some(api);
            store.settings.clash_api_secret = if secret.is_empty() {
                None
            } else {
                Some(secret)
            };
        } else {
            self.api = None;
            let wait_started = Instant::now();
            let mut ok = false;
            while wait_started.elapsed() < Duration::from_secs(4) {
                self.core.poll();
                if self.core.is_running() {
                    ok = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if !ok {
                let log_hint = self.core_startup_log_hint();
                return Err(AppError::Core(format!(
                    "sing-box failed to stay running{hint}",
                    hint = if log_hint.is_empty() {
                        String::new()
                    } else {
                        format!(": {log_hint}")
                    }
                )));
            }
        }
        self.core_started_at = Some(now_unix_secs());

        if enable_system_proxy {
            if insight.inbound_port.is_some() {
                let _ = self.set_system_proxy(store, true);
            }
        }

        Ok(self.status(store))
    }

    /// Toggle system HTTP(S)/SOCKS proxy independently of core running state.
    pub fn set_system_proxy(&mut self, store: &AppStore, enabled: bool) -> AppResult<ProxyStatus> {
        self.core.poll();
        if enabled == self.system_proxy_on {
            return Ok(self.status(store));
        }
        if enabled {
            let port = if store.settings.runtime_source().is_custom() {
                self.custom_inbound_port.ok_or_else(|| {
                    AppError::Core(
                        "当前自写配置没有 mixed/http/socks inbound，无法开启系统代理".into(),
                    )
                })?
            } else {
                store.settings.mixed_port
            };
            let snap = self.system_proxy.enable("127.0.0.1", port)?;
            self.proxy_snapshot = Some(snap);
            self.system_proxy_on = true;
        } else {
            // Only clear the in-memory state after the operating-system proxy
            // was actually restored. Otherwise the UI would report success
            // while the machine can still be pointing at our local port.
            self.system_proxy.disable(self.proxy_snapshot.as_ref())?;
            self.system_proxy_on = false;
            self.proxy_snapshot = None;
        }
        Ok(self.status(store))
    }

    /// Stop only the managed sing-box process.
    ///
    fn clear_live_connections(&mut self) {
        if self.live_connections.is_empty() {
            return;
        }
        self.live_revision = self.live_revision.saturating_add(1);
        let revision = self.live_revision;
        for connection in &self.live_connections {
            let id = connection_history_key(connection);
            self.live_item_revisions.remove(&id);
            self.live_removals.push_back((revision, id));
        }
        self.live_connections.clear();
        while self.live_removals.len() > MAX_LIVE_REMOVAL_HISTORY {
            if let Some((removed_revision, _)) = self.live_removals.pop_front() {
                self.live_diff_floor = removed_revision;
            }
        }
    }

    /// Internal restarts deliberately use this path so the saved/effective
    /// system-proxy state survives the short process replacement. A user
    /// initiated stop must use `stop_proxy`, which restores the OS first.
    fn stop_core(&mut self, _store: &AppStore) -> AppResult<()> {
        if let Some(api) = self.api.take() {
            api.deactivate();
        }
        if let Some(metrics) = self.xray_metrics.take() {
            metrics.deactivate();
        }
        // Sidecar first: the main core's delegated outbounds point at its
        // ports, so tear the dependent process down before the ingress.
        // Soft-fail — a sticky sidecar must not block stopping the proxy;
        // the next start's begin-with-stop cleans it up again.
        if self.sidecar.is_running() || self.sidecar.state() != CoreState::Stopped {
            if let Err(e) = self.sidecar.stop() {
                crate::app_log::warn("xray_sidecar", format!("sidecar stop: {e}"));
            }
            self.sidecar.await_owned_ports_released();
        }
        self.sidecar_ports.clear();
        self.core.stop()?;
        // `CoreManager::stop` waits for the process we actually own. Never
        // force-kill arbitrary listeners here: an empty/test runtime has no
        // ownership proof and could otherwise terminate another running app
        // instance (or an unrelated process using the configured ports).
        //
        // Process-exited and socket-released are not the same instant, so
        // also wait for the ports it held to actually clear. Without this,
        // `restart_core` immediately re-launching the (possibly different)
        // core can lose a bind race against the outgoing process's still-
        // draining listener — the new core then fails with "address already
        // in use" even though the old one just stopped cleanly.
        self.core.await_owned_ports_released();
        self.core_started_at = None;
        self.clear_live_connections();
        Ok(())
    }

    pub fn stop_proxy(&mut self, store: &AppStore) -> AppResult<ProxyStatus> {
        // Restore the OS proxy before releasing the local listener. If this
        // fails, keep the core alive: stopping it would strand the machine on
        // a dead 127.0.0.1 proxy and appear as a system-wide network outage.
        if self.system_proxy_on {
            self.system_proxy.disable(self.proxy_snapshot.as_ref())?;
            self.system_proxy_on = false;
            self.proxy_snapshot = None;
        }
        self.stop_core(store)?;
        // keep request_history across stop so user can review
        Ok(self.status(store))
    }

    pub fn restart_core(
        &mut self,
        app_data_dir: &Path,
        resource_dir: Option<&Path>,
        store: &mut AppStore,
    ) -> AppResult<ProxyStatus> {
        let sys = self.system_proxy_on;
        self.stop_core(store)?;
        self.start_proxy(app_data_dir, resource_dir, store, sys)
    }

    /// Full cleanup on app exit: system proxy off and stop the managed core.
    /// Returns true when no owned OS proxy remains and the ownership marker
    /// can be cleared safely.
    pub fn shutdown(&mut self) -> bool {
        let proxy_cleared = if self.system_proxy_on {
            match self.system_proxy.disable(self.proxy_snapshot.as_ref()) {
                Ok(()) => {
                    self.system_proxy_on = false;
                    self.proxy_snapshot = None;
                    true
                }
                Err(error) => {
                    crate::app_log::error(
                        "system_proxy",
                        format!("shutdown restore failed: {error}"),
                    );
                    false
                }
            }
        } else {
            true
        };
        if let Some(api) = self.api.take() {
            api.deactivate();
        }
        if let Some(metrics) = self.xray_metrics.take() {
            metrics.deactivate();
        }
        self.core.force_shutdown();
        self.sidecar.force_shutdown();
        self.sidecar_ports.clear();
        self.clear_live_connections();
        self.traffic_prev = None;
        self.traffic_speed = (0, 0);
        proxy_cleared
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

fn ensure_listen_port_available(port: u16, label: &str) -> AppResult<()> {
    ensure_listen_port_available_on(port, "127.0.0.1", label)
}

/// Pre-flight check for a listen port. Only rejects when another process is
/// visibly LISTENing (lsof/netstat). Deliberately does NOT probe-bind the
/// address itself: under TUN the core runs setuid-root, and a root-owned
/// LISTEN socket is invisible to our unprivileged lsof while making our own
/// probe bind fail with EADDRINUSE — so the bind probe misfires on exactly
/// the app's own leftover core session (a long-standing false "port in use"
/// block). Let the core spawn instead; if the port is genuinely taken, the
/// core's own FATAL output surfaces through the startup error / app log.
fn ensure_listen_port_available_on(port: u16, host: &str, label: &str) -> AppResult<()> {
    if CoreManager::has_port_listener(port) {
        return Err(AppError::Core(format!(
            "{label} 端口 {host}:{port} 已被其他程序占用，请关闭冲突程序或修改端口"
        )));
    }
    Ok(())
}

/// Shared BuildOptions for both generators (sing-box and Xray). The api
/// secret is only consumed by the sing-box clash_api; Xray ignores it.
fn build_options(store: &AppStore, api_secret: String) -> BuildOptions {
    BuildOptions {
        mixed_port: store.settings.mixed_port,
        allow_lan: store.settings.allow_lan,
        api_port: store.settings.api_port,
        extra_inbounds: store.settings.extra_inbounds.clone(),
        api_secret,
        current_node_id: store.settings.current_node_id.clone(),
        log_level: "info".into(),
        rules: store.enabled_rules_sorted(),
        rule_sets: store.enabled_rule_sets(),
        pools: store.pools.clone(),
        chains: store.chains.clone(),
        tun_enabled: store.settings.tun_enabled,
        tun_stack: store.settings.tun_stack.clone(),
        dns: store.dns.clone(),
        outbound_mode: store.settings.outbound_mode,
        route_final: store.settings.route_final.clone(),
        auto_select: store.settings.auto_select,
        probe_url: store.settings.probe_url.clone(),
        find_process: store.settings.find_process,
        tun_ipv6: store.settings.tun_ipv6_enabled,
        block_quic: store.settings.block_quic,
        bypass_lan: store.settings.bypass_lan,
        tun_interface_name: None,
        sidecar: None,
    }
}

/// Delegated nodes per sidecar plan cap this high; the rest stay native.
/// Not a functional limit — each delegated node costs one sing-box socks
/// outbound, one Xray inbound/outbound pair and one loopback port, all cheap
/// — it only guards against pathological stores (five-figure subscriptions)
/// where config size and startup time would degrade for everyone.
const SIDECAR_MAX_NODES: usize = 1024;

/// Compute which enabled nodes delegate to the companion Xray sidecar and
/// which loopback port each one gets. `None` = no sidecar (fully native
/// sing-box config):
/// - sidecar disabled in settings, or core_type/runtime_source is not the
///   generated sing-box path (mihomo/Xray main modes never delegate);
/// - no enabled node qualifies (protocol not in the delegation set, Xray
///   can't speak the exact protocol/transport combination, pinned by a
///   chain hop, WireGuard endpoint, or above [`SIDECAR_MAX_NODES`]).
///
/// Nodes dropped from the plan silently fall back to their native sing-box
/// outbound (they keep the same tag either way), with a warn log each.
pub(crate) fn compute_sidecar_plan(
    settings: &crate::domain::AppSettings,
    chains: &[crate::domain::ProxyChain],
    nodes: &[ProxyNode],
) -> Option<SidecarPlan> {
    if !settings.multi_core_enabled {
        return None;
    }
    if CoreKind::parse(&settings.core_type) != CoreKind::SingBox
        || settings.runtime_source().is_custom()
    {
        return None;
    }
    // Only entries pinned to a non-main core delegate. v1 supports exactly
    // one sidecar target (Xray); future cores slot in here as additional
    // sidecar processes.
    let wanted: std::collections::HashSet<&str> = settings
        .protocol_cores
        .iter()
        .filter(|e| e.core == CoreKind::Xray.as_str())
        .map(|e| e.protocol.as_str())
        .collect();
    if wanted.is_empty() {
        return None;
    }
    // Chain hop pins keep native semantics (v1): a detour chain that routed
    // through a loopback socks hop would technically work, but the hop dial
    // direction gets confusing to reason about and to diagnose.
    let chain_node_ids: std::collections::HashSet<&str> = chains
        .iter()
        .flat_map(|c| c.hops.iter())
        .filter_map(|h| match h {
            ChainHop::Node { node_id } => Some(node_id.as_str()),
            ChainHop::Pool { .. } => None,
        })
        .collect();

    let base = settings.sidecar_port;
    // Ports the main config (or the app) already owns — claiming one would
    // only surface as a sidecar bind FATAL after the main core is up, so
    // such nodes stay native instead.
    let mut reserved: std::collections::HashSet<u16> = std::collections::HashSet::new();
    reserved.insert(settings.mixed_port);
    reserved.insert(settings.api_port);
    reserved.insert(crate::config::DIAG_INBOUND_PORT);
    for inb in &settings.extra_inbounds {
        reserved.insert(inb.port);
    }

    let mut ports: Vec<(String, u16)> = Vec::new();
    // Sidecar ports are base + i over *candidate* nodes, not delegated ones:
    // skipping a reserved port must not stall (or shift) the range.
    let mut next_index: u32 = 0;
    for node in nodes {
        // Delegation follows ONLY the user's per-protocol pinning — no
        // per-transport special cases. Nodes the main core can't serve
        // natively (e.g. xhttp under sing-box) are filtered at config
        // generation with a logged reason unless their protocol is pinned
        // to the sidecar here.
        if !wanted.contains(node.protocol.as_str()) {
            continue;
        }
        if node.protocol == Protocol::WireGuard {
            continue;
        }
        if chain_node_ids.contains(node.id.as_str()) {
            continue;
        }
        if !CoreKind::Xray.supports_node(node) {
            crate::app_log::warn(
                "xray_sidecar",
                format!("节点「{}」的协议组合 Xray 不支持，保持原生出站", node.name),
            );
            continue;
        }
        let Some(delta) = u16::try_from(next_index).ok() else {
            crate::app_log::warn("xray_sidecar", "委托端口超出 u16 范围，提前截断委托计划");
            break;
        };
        let Some(port) = base.checked_add(delta) else {
            crate::app_log::warn("xray_sidecar", "委托端口超出 u16 范围，提前截断委托计划");
            break;
        };
        next_index += 1;
        if reserved.contains(&port) {
            crate::app_log::warn(
                "xray_sidecar",
                format!("候选端口 {port} 与主配置监听端口冲突，该节点保持原生出站"),
            );
            continue;
        }
        ports.push((node.id.clone(), port));
        if ports.len() >= SIDECAR_MAX_NODES {
            crate::app_log::warn(
                "xray_sidecar",
                format!("委托节点超过 {SIDECAR_MAX_NODES} 个上限，其余保持 sing-box 原生出站"),
            );
            break;
        }
    }
    if ports.is_empty() {
        None
    } else {
        Some(SidecarPlan { ports })
    }
}

/// macOS-only: find a `utunN` index with no existing interface, for Xray's
/// TUN inbound (see `BuildOptions::tun_interface_name`). Falls back to
/// `utun9` (matching the error message's own example) if `ifconfig` itself
/// is unavailable — Xray still fails clearly if that happens to collide.
#[cfg(target_os = "macos")]
fn pick_free_darwin_utun_name() -> String {
    let existing: std::collections::HashSet<u32> = std::process::Command::new("ifconfig")
        .arg("-l")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|name| name.strip_prefix("utun"))
        .filter_map(|n| n.parse().ok())
        .collect();
    (0..1000)
        .find(|n| !existing.contains(n))
        .map(|n| format!("utun{n}"))
        .unwrap_or_else(|| "utun9".into())
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Stable history key: prefer clash id; fall back so empty ids still accumulate.
fn connection_history_key(c: &ConnectionInfo) -> String {
    if !c.id.trim().is_empty() {
        return c.id.clone();
    }
    format!(
        "{}|{}|{}|{}|{}",
        c.network, c.destination, c.source, c.process, c.start
    )
}

/// Resolved node display info for a connection: human node name + owning
/// subscription name (e.g. "新加坡01" / "机场A").
struct NodeInfo {
    name: String,
    subscription: String,
}

/// Map outbound tag → resolved display info, using only enabled subscriptions.
fn node_tag_info_map(store: &AppStore) -> HashMap<String, NodeInfo> {
    let enabled: std::collections::HashSet<_> = store
        .subscriptions
        .iter()
        .filter(|s| s.enabled)
        .map(|s| s.id.as_str())
        .collect();
    // subscription id → name
    let sub_name: HashMap<&str, &str> = store
        .subscriptions
        .iter()
        .map(|s| (s.id.as_str(), s.name.as_str()))
        .collect();
    store
        .nodes
        .iter()
        .filter(|n| enabled.contains(n.subscription_id.as_str()))
        .map(|n| {
            let info = NodeInfo {
                name: n.node.name.clone(),
                subscription: sub_name
                    .get(n.subscription_id.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
            };
            (outbound_tag(&n.node), info)
        })
        .collect()
}

/// UI-facing connection / request row.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionView {
    pub id: String,
    pub destination: String,
    pub host: String,
    pub network: String,
    pub conn_type: String,
    /// Raw tag or chain leaf
    pub node_tag: String,
    /// Human node name when known
    pub node_name: String,
    /// Owning subscription name (for tooltip: 订阅配置名 + 节点名称)
    #[serde(default)]
    pub subscription_name: String,
    pub chains: Vec<String>,
    pub chains_display: String,
    pub rule: String,
    pub rule_payload: String,
    pub process: String,
    pub source: String,
    pub upload: u64,
    pub download: u64,
    pub start: String,
    pub first_seen: Option<i64>,
    pub last_seen: Option<i64>,
    pub closed: bool,
    pub closed_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LiveConnectionBatch {
    pub rows: Vec<ConnectionView>,
    pub removed_ids: Vec<String>,
    /// Full id order. Omitted (`None`) when the id set is unchanged since the
    /// client's `order_revision` — clients then merge `rows` in place.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_ids: Option<Vec<String>>,
    /// Bumps only on membership changes; pass it back on the next poll.
    pub order_revision: u64,
    pub revision: u64,
    pub unchanged: bool,
    pub full: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RequestBatch {
    pub entries: Vec<ConnectionView>,
    pub cursor: u64,
}

impl ConnectionView {
    fn from_info(c: &ConnectionInfo, tag_info: &HashMap<String, NodeInfo>) -> Self {
        let info = tag_info.get(&c.node);
        let node_name = info
            .map(|i| i.name.clone())
            .unwrap_or_else(|| c.node.clone());
        let subscription_name = info.map(|i| i.subscription.clone()).unwrap_or_default();
        let chains_display = c.chains.join(" → ");
        Self {
            id: connection_history_key(c),
            destination: c.destination.clone(),
            host: c.host.clone(),
            network: c.network.clone(),
            conn_type: c.conn_type.clone(),
            node_tag: c.node.clone(),
            node_name,
            subscription_name,
            chains: c.chains.clone(),
            chains_display,
            rule: format_rule(&c.rule, &c.rule_payload),
            rule_payload: c.rule_payload.clone(),
            process: c.process.clone(),
            source: c.source.clone(),
            upload: c.upload,
            download: c.download,
            start: c.start.clone(),
            first_seen: None,
            last_seen: None,
            closed: false,
            closed_at: None,
        }
    }

    fn from_record(r: &RequestRecord, tag_info: &HashMap<String, NodeInfo>) -> Self {
        let info = tag_info.get(&r.node);
        let node_name = info
            .map(|i| i.name.clone())
            .unwrap_or_else(|| r.node.clone());
        let subscription_name = info.map(|i| i.subscription.clone()).unwrap_or_default();
        Self {
            id: r.id.clone(),
            destination: r.destination.clone(),
            host: r.host.clone(),
            network: r.network.clone(),
            conn_type: r.conn_type.clone(),
            node_tag: r.node.clone(),
            node_name,
            subscription_name,
            chains: r.chains.clone(),
            chains_display: r.chains.join(" → "),
            rule: format_rule(&r.rule, &r.rule_payload),
            rule_payload: r.rule_payload.clone(),
            process: r.process.clone(),
            source: r.source.clone(),
            upload: r.upload,
            download: r.download,
            start: String::new(),
            first_seen: Some(r.first_seen),
            last_seen: Some(r.last_seen),
            closed: r.closed,
            closed_at: r.closed_at,
        }
    }
}

fn format_rule(rule: &str, payload: &str) -> String {
    if rule.is_empty() && payload.is_empty() {
        return "—".into();
    }
    if payload.is_empty() {
        return rule.to_string();
    }
    if rule.is_empty() {
        return payload.to_string();
    }
    format!("{rule}({payload})")
}

#[cfg(test)]
mod clash_api_secret_tests {
    use super::*;

    #[test]
    fn disabled_toggle_clears_any_stored_secret_and_returns_empty() {
        // Turning the toggle off must actually wipe the persisted secret —
        // otherwise the UI could still display/copy a "dead" key that no
        // longer matches what's in the running config.
        let mut store = AppStore::default();
        store.settings.api_secret_enabled = false;
        store.settings.clash_api_secret = Some("leftover".into());
        let secret = resolve_clash_api_secret(&mut store);
        assert_eq!(secret, "");
        assert_eq!(store.settings.clash_api_secret, None);
    }

    #[test]
    fn enabled_toggle_generates_a_secret_when_none_is_stored() {
        let mut store = AppStore::default();
        store.settings.api_secret_enabled = true;
        store.settings.clash_api_secret = None;
        let secret = resolve_clash_api_secret(&mut store);
        assert!(!secret.is_empty());
        assert_eq!(store.settings.clash_api_secret, Some(secret));
    }

    #[test]
    fn enabled_toggle_reuses_the_persisted_secret_across_restarts() {
        // Regenerating on every restart would silently break any external
        // tool that saved the previous secret.
        let mut store = AppStore::default();
        store.settings.api_secret_enabled = true;
        store.settings.clash_api_secret = Some("kept-secret".into());
        let secret = resolve_clash_api_secret(&mut store);
        assert_eq!(secret, "kept-secret");
    }
}

#[cfg(test)]
mod sidecar_plan_tests {
    use super::*;
    use crate::domain::{ProtocolConfig, ProxyChain, TlsConfig, Transport};

    fn node(id: &str, protocol: Protocol) -> ProxyNode {
        let config = match protocol {
            Protocol::Vless => ProtocolConfig::Vless {
                uuid: "uuid-1".into(),
                flow: None,
                packet_encoding: "xudp".into(),
            },
            _ => ProtocolConfig::Shadowsocks {
                method: "aes-256-gcm".into(),
                password: "pw".into(),
                plugin: None,
                plugin_opts: None,
                shadow_tls: None,
            },
        };
        ProxyNode {
            id: id.into(),
            name: format!("n-{id}"),
            protocol,
            server: "example.com".into(),
            port: 443,
            tls: None,
            transport: None,
            udp: Some(true),
            config,
            source: None,
            latency_ms: None,
            latency_at: None,
        }
    }

    fn sidecar_store() -> AppStore {
        let mut store = AppStore::default();
        store.settings.multi_core_enabled = true;
        store.settings.protocol_cores = vec![crate::domain::ProtocolCoreItem {
            protocol: "vless".into(),
            core: "xray".into(),
        }];
        store.settings.sidecar_port = 20890;
        store
    }

    #[test]
    fn disabled_or_non_singbox_core_yields_no_plan() {
        let mut store = sidecar_store();
        let nodes = vec![node("n1", Protocol::Vless)];
        store.settings.multi_core_enabled = false;
        assert!(compute_sidecar_plan(&store.settings, &store.chains, &nodes).is_none());

        store.settings.multi_core_enabled = true;
        store.settings.core_type = "xray".into();
        assert!(compute_sidecar_plan(&store.settings, &store.chains, &nodes).is_none());
    }

    #[test]
    fn only_delegated_protocols_get_ports_in_input_order() {
        let store = sidecar_store();
        let nodes = vec![
            node("ss1", Protocol::Shadowsocks),
            node("vl1", Protocol::Vless),
            node("vl2", Protocol::Vless),
        ];
        let plan = compute_sidecar_plan(&store.settings, &store.chains, &nodes).expect("plan");
        assert_eq!(
            plan.ports,
            vec![("vl1".into(), 20890), ("vl2".into(), 20891)]
        );
    }

    #[test]
    fn xray_unsupported_combo_falls_back_to_native() {
        // REALITY over ws is exactly the combination CoreKind::Xray rejects —
        // the plan must drop it (native sing-box outbound) rather than emit a
        // sidecar inbound the Xray config can't map.
        let store = sidecar_store();
        let mut n = node("vl", Protocol::Vless);
        n.tls = Some(TlsConfig {
            enabled: true,
            server_name: Some("sni.example.com".into()),
            insecure: None,
            alpn: None,
            utls_fingerprint: None,
            reality_public_key: Some("pbk".into()),
            reality_short_id: Some("abcd".into()),
        });
        n.transport = Some(Transport::Ws {
            path: None,
            headers: None,
            max_early_data: None,
        });
        assert!(compute_sidecar_plan(&store.settings, &store.chains, &[n]).is_none());
    }

    #[test]
    fn chain_pinned_nodes_stay_native() {
        let mut store = sidecar_store();
        store.chains = vec![ProxyChain::new(
            "链",
            vec![
                ChainHop::Node {
                    node_id: "vl1".into(),
                },
                ChainHop::Node {
                    node_id: "ss1".into(),
                },
            ],
        )];
        let nodes = vec![
            node("ss1", Protocol::Shadowsocks),
            node("vl1", Protocol::Vless),
            node("vl2", Protocol::Vless),
        ];
        let plan = compute_sidecar_plan(&store.settings, &store.chains, &nodes).expect("plan");
        assert_eq!(plan.ports, vec![("vl2".into(), 20890)]);
    }

    #[test]
    fn plan_caps_at_max_nodes() {
        let store = sidecar_store();
        let nodes: Vec<ProxyNode> = (0..(SIDECAR_MAX_NODES + 5))
            .map(|i| node(&format!("n{i}"), Protocol::Vless))
            .collect();
        let plan = compute_sidecar_plan(&store.settings, &store.chains, &nodes).expect("plan");
        assert_eq!(plan.ports.len(), SIDECAR_MAX_NODES);
    }

    #[test]
    fn xhttp_nodes_follow_the_protocol_pin_only() {
        // No per-transport special cases: an xhttp node delegates exactly
        // when its PROTOCOL is pinned to Xray. vless is pinned here (goes to
        // the sidecar); shadowsocks is not (stays native — and since
        // sing-box can't speak xhttp, generation filters that node out).
        let store = sidecar_store();
        let mut vl_xhttp = node("vl1", Protocol::Vless);
        vl_xhttp.transport = Some(crate::domain::Transport::Xhttp {
            path: None,
            host: None,
            mode: None,
        });
        let mut ss_xhttp = node("ss1", Protocol::Shadowsocks);
        ss_xhttp.transport = Some(crate::domain::Transport::Xhttp {
            path: None,
            host: None,
            mode: None,
        });
        let nodes = vec![vl_xhttp, ss_xhttp];
        let plan = compute_sidecar_plan(&store.settings, &store.chains, &nodes).expect("plan");
        assert_eq!(plan.ports, vec![("vl1".into(), 20890)]);
    }

    #[test]
    fn reserved_ports_are_skipped_without_stalling_the_range() {
        // The second candidate port (base+1) collides with the Clash API
        // port: that node stays native, later nodes keep their own indexes —
        // the range must not shift, stall, or reuse the reserved port.
        let mut store = sidecar_store(); // base 20890
        store.settings.api_port = 20891;
        store.settings.extra_inbounds = vec![crate::domain::ExtraInbound {
            id: "x".into(),
            kind: "mixed".into(),
            port: 20894,
            allow_lan: false,
        }];
        let nodes = vec![
            node("vl1", Protocol::Vless),
            node("vl2", Protocol::Vless),
            node("vl3", Protocol::Vless),
            node("vl4", Protocol::Vless),
            node("vl5", Protocol::Vless),
        ];
        let plan = compute_sidecar_plan(&store.settings, &store.chains, &nodes).expect("plan");
        assert_eq!(
            plan.ports,
            vec![
                ("vl1".into(), 20890),
                // 20891 = api port → vl2 native
                ("vl3".into(), 20892),
                ("vl4".into(), 20893),
                // 20894 = extra inbound → vl5 native
            ]
        );
    }
}

#[cfg(test)]
mod stop_behavior_tests {
    use super::*;
    use crate::proxy::{SystemProxy, SystemProxySnapshot};
    use std::sync::{Arc, Mutex};

    struct RecordingSystemProxy {
        disabled: Arc<Mutex<usize>>,
        fail_disable: bool,
    }

    impl SystemProxy for RecordingSystemProxy {
        fn enable(&self, _host: &str, _port: u16) -> AppResult<SystemProxySnapshot> {
            Ok(SystemProxySnapshot {
                detail: "test".into(),
            })
        }

        fn disable(&self, _snapshot: Option<&SystemProxySnapshot>) -> AppResult<()> {
            *self.disabled.lock().expect("disabled counter") += 1;
            if self.fail_disable {
                Err(AppError::Core("restore failed".into()))
            } else {
                Ok(())
            }
        }

        fn detect_owned(&self, _host: &str, _port: u16) -> AppResult<Option<SystemProxySnapshot>> {
            Ok(None)
        }
    }

    fn runtime_with_system_proxy(fail_disable: bool) -> (Runtime, Arc<Mutex<usize>>) {
        let disabled = Arc::new(Mutex::new(0));
        let mut runtime = Runtime::new();
        runtime.system_proxy = Box::new(RecordingSystemProxy {
            disabled: Arc::clone(&disabled),
            fail_disable,
        });
        runtime.system_proxy_on = true;
        runtime.proxy_snapshot = Some(SystemProxySnapshot {
            detail: "previous system proxy".into(),
        });
        (runtime, disabled)
    }

    fn closed_record(id: &str, history_seq: u64, host: &str) -> RequestRecord {
        RequestRecord {
            id: id.into(),
            history_seq,
            destination: format!("{host}:443"),
            host: host.into(),
            network: "tcp".into(),
            conn_type: "http".into(),
            node: "proxy".into(),
            chains: Vec::new(),
            rule: String::new(),
            rule_payload: String::new(),
            process: String::new(),
            source: String::new(),
            upload: 10,
            download: 10,
            first_seen: 1_000,
            last_seen: 1_100,
            closed: true,
            closed_at: Some(1_100),
        }
    }

    #[test]
    fn request_incremental_cursor_pages_without_skipping() {
        let store = AppStore::default();
        let mut runtime = Runtime::new();
        runtime.journal_seq = 3;
        for (id, seq, host) in [
            ("one", 1, "match.example"),
            ("two", 2, "other.example"),
            ("three", 3, "match.example"),
        ] {
            runtime
                .request_by_id
                .insert(id.into(), closed_record(id, seq, host));
        }

        let first = runtime.request_history(&store, None, Some(1), Some(0));
        assert_eq!(first.entries.len(), 1);
        assert_eq!(first.cursor, 1);
        let second = runtime.request_history(&store, None, Some(1), Some(first.cursor));
        assert_eq!(second.entries.len(), 1);
        assert_eq!(second.cursor, 2);

        let filtered = runtime.request_history(&store, Some("match"), Some(10), Some(1));
        assert_eq!(filtered.entries.len(), 1);
        assert_eq!(filtered.entries[0].id, "three");
        assert_eq!(filtered.cursor, 3);
    }

    #[test]
    fn user_stop_restores_system_proxy_before_reporting_stopped() {
        let store = AppStore::default();
        let (mut runtime, disabled) = runtime_with_system_proxy(false);

        let status = runtime.stop_proxy(&store).expect("stop proxy");

        assert_eq!(*disabled.lock().expect("disabled counter"), 1);
        assert!(!runtime.system_proxy_on);
        assert!(runtime.proxy_snapshot.is_none());
        assert!(!status.system_proxy);
        assert!(!status.running);
    }

    #[test]
    fn user_stop_does_not_clear_state_when_system_proxy_restore_fails() {
        let store = AppStore::default();
        let (mut runtime, disabled) = runtime_with_system_proxy(true);

        let error = runtime.stop_proxy(&store).expect_err("restore must fail");

        assert!(error.to_string().contains("restore failed"));
        assert_eq!(*disabled.lock().expect("disabled counter"), 1);
        assert!(runtime.system_proxy_on);
        assert!(runtime.proxy_snapshot.is_some());
    }

    #[test]
    fn shutdown_reports_whether_system_proxy_was_cleared() {
        let (mut successful, _) = runtime_with_system_proxy(false);
        assert!(successful.shutdown());
        assert!(!successful.system_proxy_on);

        let (mut failed, _) = runtime_with_system_proxy(true);
        assert!(!failed.shutdown());
        assert!(failed.system_proxy_on);
        assert!(failed.proxy_snapshot.is_some());
    }

    #[test]
    fn status_never_waits_for_clash_api_network_io() {
        let store = AppStore::default();
        let mut runtime = Runtime::new();
        runtime.api = Some(ClashApi::new("192.0.2.1", 9, "unreachable"));

        let started = Instant::now();
        let _ = runtime.status(&store);

        assert!(
            started.elapsed() < Duration::from_millis(100),
            "status must remain a memory-only operation"
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn stop_then_start_accepts_the_same_ports() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut holder = std::process::Command::new("/usr/bin/nc")
            .args(["-l", "127.0.0.1", &port.to_string()])
            .spawn()
            .unwrap();
        for _ in 0..20 {
            if CoreManager::has_port_listener(port) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(ensure_listen_port_available(port, "test").is_err());

        let mut store = AppStore::default();
        store.settings.mixed_port = port;
        store.settings.api_port = 0;
        let mut runtime = Runtime::new();
        let api = ClashApi::new("127.0.0.1", port, "test");
        runtime.api = Some(api.clone());
        runtime.stop_proxy(&store).unwrap();
        let restart_allowed = ensure_listen_port_available(port, "test").is_ok();
        let _ = holder.kill();
        let _ = holder.wait();

        assert!(restart_allowed, "stop must allow an immediate restart");
        assert!(!api.is_active(), "stop must cancel Clash API clients");
    }
}

/// Cross-platform protocol tests (the `tests` module above is macOS-only
/// because of the /usr/bin/nc listener).
#[cfg(test)]
mod live_batch_tests {
    use super::*;

    #[test]
    fn live_batch_skips_order_ids_until_membership_changes() {
        let conn = |id: &str, up: u64| ConnectionInfo {
            id: id.into(),
            destination: format!("{id}.example:443"),
            host: format!("{id}.example"),
            destination_ip: "1.2.3.4".into(),
            destination_port: "443".into(),
            network: "tcp".into(),
            conn_type: String::new(),
            source: "127.0.0.1:1".into(),
            process: String::new(),
            chains: vec![],
            node: String::new(),
            rule: String::new(),
            rule_payload: String::new(),
            upload: up,
            download: 0,
            start: String::new(),
        };

        let mut runtime = Runtime::new();
        let store = AppStore::default();
        runtime.ingest_connections(vec![conn("a", 1), conn("b", 1)]);

        let first = runtime.live_connection_batch(&store, None, None);
        assert!(first.full && first.order_ids.is_some());
        assert_eq!(first.order_ids.as_ref().map(Vec::len), Some(2));

        // Pure counter update — membership unchanged: order_ids omitted.
        runtime.ingest_connections(vec![conn("a", 5), conn("b", 5)]);
        let delta =
            runtime.live_connection_batch(&store, Some(first.revision), Some(first.order_revision));
        assert!(!delta.unchanged && !delta.full);
        assert!(
            delta.order_ids.is_none(),
            "no membership change → skip order_ids"
        );
        assert_eq!(delta.rows.len(), 2);

        // New id → membership changed → order_ids return and revision bumps.
        runtime.ingest_connections(vec![conn("a", 5), conn("b", 5), conn("c", 1)]);
        let delta2 =
            runtime.live_connection_batch(&store, Some(delta.revision), Some(delta.order_revision));
        assert!(delta2.order_ids.is_some());
        assert_eq!(delta2.order_ids.as_ref().map(Vec::len), Some(3));
        assert!(delta2.order_revision > delta.order_revision);

        // Removal also counts as a membership change.
        runtime.ingest_connections(vec![conn("a", 5), conn("b", 5)]);
        let delta3 = runtime.live_connection_batch(
            &store,
            Some(delta2.revision),
            Some(delta2.order_revision),
        );
        assert!(delta3.order_ids.is_some());
        assert_eq!(delta3.order_ids.as_ref().map(Vec::len), Some(2));
        assert!(!delta3.removed_ids.is_empty());
    }
}
