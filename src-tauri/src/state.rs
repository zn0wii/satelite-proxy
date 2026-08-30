use crate::app_log;
use crate::core::manager::CoreState;
use crate::error::AppResult;
use crate::runtime::{ConnectionView, LiveConnectionBatch, ProxyStatus, RequestBatch, Runtime};
use crate::storage::{default_store_path, AppStore};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

const KERNEL_SELECTION_POLL_INTERVAL: Duration = Duration::from_secs(2);
const KERNEL_SELECTION_HTTP_TIMEOUT: Duration = Duration::from_millis(800);

#[derive(Default)]
struct KernelSelectionPoll {
    in_flight: bool,
    last_started: Option<Instant>,
}

#[derive(Default)]
struct QueryViewCache {
    query: String,
    limit: Option<usize>,
    rows: Vec<ConnectionView>,
    cursor: u64,
}

#[derive(Default)]
struct TrafficViewCache {
    live: Vec<ConnectionView>,
    live_revision: u64,
    live_order_revision: u64,
    requests: QueryViewCache,
    failures: QueryViewCache,
}

fn apply_selected_node(
    settings: &mut crate::domain::AppSettings,
    node_id: String,
    manual: bool,
) -> bool {
    let was_kernel = settings.auto_select.is_kernel();
    settings.current_node_id = Some(node_id);
    if manual {
        settings.auto_select = crate::domain::AutoSelectMode::Off;
        settings.smart_switch = false;
    }
    was_kernel
}

impl KernelSelectionPoll {
    fn try_start(&mut self, now: Instant) -> bool {
        if self.in_flight
            || self.last_started.is_some_and(|last| {
                now.saturating_duration_since(last) < KERNEL_SELECTION_POLL_INTERVAL
            })
        {
            return false;
        }
        self.in_flight = true;
        self.last_started = Some(now);
        true
    }

    fn finish(&mut self) {
        self.in_flight = false;
    }
}

#[cfg(test)]
mod kernel_selection_poll_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::sync::Arc;

    #[test]
    fn suppresses_concurrent_and_recent_status_polls() {
        let mut poll = KernelSelectionPoll::default();
        let start = Instant::now();
        assert!(poll.try_start(start));
        assert!(!poll.try_start(start + Duration::from_secs(10)));

        poll.finish();
        assert!(!poll.try_start(start + Duration::from_millis(500)));
        assert!(poll.try_start(start + KERNEL_SELECTION_POLL_INTERVAL));
    }

    #[test]
    fn manual_node_selection_disables_every_auto_select_mode() {
        for mode in [
            crate::domain::AutoSelectMode::Smart,
            crate::domain::AutoSelectMode::Kernel,
        ] {
            let mut settings = crate::domain::AppSettings {
                auto_select: mode,
                smart_switch: true,
                ..crate::domain::AppSettings::default()
            };
            let was_kernel = apply_selected_node(&mut settings, "manual-node".into(), true);
            assert_eq!(settings.auto_select, crate::domain::AutoSelectMode::Off);
            assert!(!settings.smart_switch);
            assert_eq!(settings.current_node_id.as_deref(), Some("manual-node"));
            assert_eq!(was_kernel, mode.is_kernel());
        }
    }

    #[test]
    fn kernel_auto_manual_select_skips_live_put_and_flips_mode() {
        let test_dir = std::env::temp_dir().join(format!(
            "satelite-kernel-manual-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let state = AppState::load(test_dir.clone(), None).expect("load test state");
        state
            .with_store_mut(|store| {
                store.upsert_subscription(
                    crate::domain::Subscription {
                        id: "sub".into(),
                        name: "sub".into(),
                        source: crate::domain::SubscriptionSource::Url {
                            url: "https://example.com/sub".into(),
                        },
                        last_update: 1,
                        node_count: 0,
                        enabled: true,
                        format: None,
                        skipped_count: 0,
                        via_proxy: false,
                        auto_update: false,
                        auto_update_interval_min: 1440,
                        traffic: None,
                    },
                    vec![crate::domain::ProxyNode {
                        id: "node-a".into(),
                        name: "node-a".into(),
                        protocol: crate::domain::Protocol::Trojan,
                        server: "example.com".into(),
                        port: 443,
                        tls: None,
                        transport: None,
                        udp: None,
                        config: crate::domain::ProtocolConfig::Trojan {
                            password: "x".into(),
                        },
                        source: None,
                        latency_ms: None,
                        latency_at: None,
                    }],
                )?;
                store.settings.auto_select = crate::domain::AutoSelectMode::Kernel;
                Ok(())
            })
            .expect("seed kernel mode");

        // Nothing listens here: any live PUT would fail, proving it is skipped.
        state.lock_runtime().api = Some(crate::api::ClashApi::new("127.0.0.1", 1, "test"));

        let (settings, was_kernel, selected_live) = state
            .select_current_node_serialized("node-a", true, true)
            .expect("kernel-mode manual select must not touch the urltest group");
        assert!(!selected_live);
        assert!(was_kernel);
        assert_eq!(settings.auto_select, crate::domain::AutoSelectMode::Off);
        assert_eq!(settings.current_node_id.as_deref(), Some("node-a"));

        let _ = std::fs::remove_dir_all(test_dir);
    }

    #[test]
    fn status_uses_cache_instead_of_waiting_for_runtime_during_transition() {
        let test_dir = std::env::temp_dir().join(format!(
            "satelite-status-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let state = Arc::new(AppState::load(test_dir.clone(), None).expect("load test state"));
        state.core_transitioning.store(true, Ordering::Release);
        state.mark_cached_core_state(CoreState::Starting);

        let runtime = state.lock_runtime();
        let (tx, rx) = mpsc::channel();
        let query_state = Arc::clone(&state);
        let query = std::thread::spawn(move || {
            tx.send(query_state.proxy_status()).expect("send status");
        });

        let result = rx.recv_timeout(Duration::from_millis(200));
        drop(runtime);
        query.join().expect("status query thread");
        let status = result
            .expect("status query must not wait for the runtime lock")
            .expect("status query");
        assert_eq!(status.core_state, CoreState::Starting);

        let _ = std::fs::remove_dir_all(test_dir);
    }

    #[test]
    fn traffic_views_use_cache_instead_of_waiting_during_transition() {
        let test_dir = std::env::temp_dir().join(format!(
            "satelite-traffic-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let state = Arc::new(AppState::load(test_dir.clone(), None).expect("load test state"));
        state.core_transitioning.store(true, Ordering::Release);

        let runtime = state.lock_runtime();
        let (tx, rx) = mpsc::channel();
        let query_state = Arc::clone(&state);
        let query = std::thread::spawn(move || {
            let live = query_state.live_connection_views();
            let requests = query_state.request_views(None, Some(800), false, None);
            let failures = query_state.request_views(None, Some(800), true, None);
            tx.send((live, requests, failures))
                .expect("send traffic views");
        });

        let result = rx.recv_timeout(Duration::from_millis(200));
        drop(runtime);
        query.join().expect("traffic query thread");
        let (live, requests, failures) =
            result.expect("traffic queries must not wait for the runtime lock");
        assert!(live.is_empty());
        assert!(requests.entries.is_empty());
        assert!(failures.entries.is_empty());

        let _ = std::fs::remove_dir_all(test_dir);
    }

    #[test]
    fn journal_never_waits_for_runtime_and_rejects_stale_sessions() {
        let test_dir = std::env::temp_dir().join(format!(
            "satelite-journal-session-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let state = Arc::new(AppState::load(test_dir.clone(), None).expect("load test state"));
        let current = crate::api::ClashApi::new("127.0.0.1", 19090, "current");
        state.lock_runtime().api = Some(current.clone());

        let runtime = state.lock_runtime();
        let (tx, rx) = mpsc::channel();
        let journal_state = Arc::clone(&state);
        let query = std::thread::spawn(move || {
            tx.send(journal_state.try_clash_api_clone())
                .expect("send journal API");
        });
        let busy_result = rx.recv_timeout(Duration::from_millis(200));
        drop(runtime);
        query.join().expect("journal query thread");
        assert!(busy_result.expect("journal query must not wait").is_none());

        let stale = crate::api::ClashApi::new("127.0.0.1", 19090, "stale");
        let snapshot = |upload_total| crate::api::ConnectionsSnapshot {
            upload_total,
            download_total: 0,
            connections: Vec::new(),
        };
        assert!(!state.try_apply_connection_snapshot(&stale, snapshot(3)));
        assert!(state.try_apply_connection_snapshot(&current, snapshot(7)));
        assert_eq!(state.proxy_status().expect("status").upload_total, 7);

        let _ = std::fs::remove_dir_all(test_dir);
    }

    #[test]
    fn live_selection_does_not_hold_runtime_lock_during_http() {
        let test_dir = std::env::temp_dir().join(format!(
            "satelite-live-select-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let state = Arc::new(AppState::load(test_dir.clone(), None).expect("load test state"));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake clash api");
        let port = listener.local_addr().expect("fake api address").port();
        state.lock_runtime().api = Some(crate::api::ClashApi::new("127.0.0.1", port, "test"));

        let (seen_tx, seen_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept selector request");
            let mut request = [0u8; 4096];
            socket.read(&mut request).expect("read selector request");
            seen_tx.send(()).expect("signal request received");
            release_rx.recv().expect("release fake response");
            socket
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .expect("write selector response");
        });

        let operation_state = Arc::clone(&state);
        let operation = std::thread::spawn(move || {
            operation_state.select_group_live_serialized("proxy", "node-a", false)
        });
        seen_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("selector request must reach fake api");
        let runtime_was_available = state.runtime.try_lock().is_ok();
        release_tx.send(()).expect("release selector response");
        server.join().expect("fake api server");
        assert!(operation
            .join()
            .expect("selector thread")
            .expect("selection"));
        assert!(
            runtime_was_available,
            "runtime lock must be released before selector HTTP waits"
        );

        let _ = std::fs::remove_dir_all(test_dir);
    }

    #[test]
    fn core_running_check_never_waits_for_runtime_lock() {
        let test_dir = std::env::temp_dir().join(format!(
            "satelite-running-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let state = Arc::new(AppState::load(test_dir.clone(), None).expect("load test state"));
        {
            let mut cached = recover_lock(&state.status_cache, "status_cache");
            cached.running = true;
            cached.core_state = CoreState::Running;
        }

        let runtime = state.lock_runtime();
        let (tx, rx) = mpsc::channel();
        let query_state = Arc::clone(&state);
        let cached_query = std::thread::spawn(move || {
            tx.send(query_state.is_core_running())
                .expect("send cached running state");
        });
        let cached_running = rx.recv_timeout(Duration::from_millis(200));
        drop(runtime);
        cached_query.join().expect("cached running query");

        state.core_transitioning.store(true, Ordering::Release);
        let runtime = state.lock_runtime();
        let (tx, rx) = mpsc::channel();
        let query_state = Arc::clone(&state);
        let transition_query = std::thread::spawn(move || {
            tx.send(query_state.is_core_running())
                .expect("send transition running state");
        });
        let transition_running = rx.recv_timeout(Duration::from_millis(200));
        drop(runtime);
        transition_query.join().expect("transition running query");

        assert!(cached_running.expect("lock contention must use cached state"));
        assert!(!transition_running.expect("transition check must not wait"));
        let _ = std::fs::remove_dir_all(test_dir);
    }
}

pub struct AppState {
    pub app_data_dir: PathBuf,
    /// Tauri resource dir (bundled assets); used to scan `resources/rules/`.
    pub resource_dir: Option<PathBuf>,
    pub store_path: PathBuf,
    pub store: Mutex<AppStore>,
    /// Serializes store mutations through their durable write. The store mutex
    /// itself can be released before disk I/O so read-only UI calls stay responsive.
    store_persistence: Mutex<()>,
    pub runtime: Mutex<Runtime>,
    /// Last complete status snapshot. Status IPC reads this while a long core
    /// transition owns `runtime`, so the WebView never queues behind startup.
    status_cache: Mutex<ProxyStatus>,
    /// Last rendered traffic rows. Traffic pages keep showing these while a
    /// core transition owns `runtime` instead of queuing another IPC request.
    traffic_view_cache: Mutex<TrafficViewCache>,
    /// Main WebView is visible (affects journal sampling rate).
    pub ui_visible: AtomicBool,
    /// Only true when user explicitly quits (tray Quit / close without tray).
    /// Destroying the last WebView would otherwise kill tray + sing-box.
    pub exit_allowed: AtomicBool,
    /// True while the managed core is being started, stopped, or replaced.
    /// Background samplers must not contend for Runtime during this window.
    core_transitioning: AtomicBool,
    /// One-click subscribe deep links waiting for the add-subscription UI.
    /// Cleared when the user closes the modal (not sticky across intentional dismiss).
    pending_import_urls: Mutex<Option<Vec<String>>>,
    /// One global debounced apply queue for toggles and remote-rule updates.
    rule_apply_queue: Mutex<crate::rule_apply::RuleApplyQueue>,
    kernel_selection_poll: Mutex<KernelSelectionPoll>,
}

struct CoreTransitionGuard<'a> {
    flag: &'a AtomicBool,
}

impl Drop for CoreTransitionGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// Recover from a poisoned mutex so one panic cannot brick the whole app.
fn recover_lock<'a, T>(mutex: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            app_log::error(
                "lock",
                format!(
                    "{name} lock was poisoned — recovering (previous panic left the mutex tainted)"
                ),
            );
            poisoned.into_inner()
        }
    }
}

impl AppState {
    pub fn load(app_data_dir: PathBuf, resource_dir: Option<PathBuf>) -> AppResult<Self> {
        let store_path = default_store_path(&app_data_dir);
        let store = AppStore::load(&store_path, resource_dir.as_deref())?;
        let mut runtime = Runtime::new();
        let status_cache = runtime.status(&store);
        Ok(Self {
            app_data_dir,
            resource_dir,
            store_path,
            store: Mutex::new(store),
            store_persistence: Mutex::new(()),
            runtime: Mutex::new(runtime),
            status_cache: Mutex::new(status_cache),
            traffic_view_cache: Mutex::new(TrafficViewCache::default()),
            ui_visible: AtomicBool::new(true),
            exit_allowed: AtomicBool::new(false),
            core_transitioning: AtomicBool::new(false),
            pending_import_urls: Mutex::new(None),
            rule_apply_queue: Mutex::new(crate::rule_apply::RuleApplyQueue::default()),
            kernel_selection_poll: Mutex::new(KernelSelectionPoll::default()),
        })
    }

    /// Queue deep-link URLs for the frontend add-subscription form.
    pub fn set_pending_import_urls(&self, urls: Vec<String>) {
        *recover_lock(&self.pending_import_urls, "pending_import") = Some(urls);
    }

    /// Read pending import URLs without clearing (UI may remount before user closes).
    pub fn peek_pending_import_urls(&self) -> Option<Vec<String>> {
        recover_lock(&self.pending_import_urls, "pending_import").clone()
    }

    /// Drop pending import after user closes / finishes the add form.
    pub fn clear_pending_import_urls(&self) {
        *recover_lock(&self.pending_import_urls, "pending_import") = None;
    }

    /// Lock order rule: **never** hold `store` while acquiring `runtime`.
    /// Prefer `runtime` then `store` when both are needed.
    pub fn lock_runtime(&self) -> MutexGuard<'_, Runtime> {
        recover_lock(&self.runtime, "runtime")
    }

    pub fn lock_store(&self) -> MutexGuard<'_, AppStore> {
        recover_lock(&self.store, "store")
    }

    fn lock_store_persistence(&self) -> MutexGuard<'_, ()> {
        recover_lock(&self.store_persistence, "store_persistence")
    }

    /// Short-lived bookkeeping lock; unrelated to the runtime/store lock order.
    pub(crate) fn lock_rule_apply_queue(
        &self,
    ) -> MutexGuard<'_, crate::rule_apply::RuleApplyQueue> {
        recover_lock(&self.rule_apply_queue, "rule_apply_queue")
    }

    pub fn set_ui_visible(&self, visible: bool) {
        self.ui_visible.store(visible, Ordering::Relaxed);
    }

    pub fn is_ui_visible(&self) -> bool {
        self.ui_visible.load(Ordering::Relaxed)
    }

    pub fn allow_exit(&self) {
        self.exit_allowed.store(true, Ordering::SeqCst);
    }

    pub fn is_exit_allowed(&self) -> bool {
        self.exit_allowed.load(Ordering::SeqCst)
    }

    pub fn is_core_transitioning(&self) -> bool {
        self.core_transitioning.load(Ordering::Acquire)
    }

    /// The connection journal is best-effort and high-frequency. It must never
    /// queue behind a core transition; another snapshot will arrive shortly.
    pub fn try_clash_api_clone(&self) -> Option<crate::api::ClashApi> {
        if self.is_core_transitioning() {
            return None;
        }
        match self.runtime.try_lock() {
            Ok(runtime) => runtime.clash_api_clone(),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Poisoned(poisoned)) => {
                app_log::error("lock", "runtime lock was poisoned — recovering");
                poisoned.into_inner().clash_api_clone()
            }
        }
    }

    /// Xray-mode counterpart of `try_clash_api_clone` for the journal poller.
    pub fn try_xray_metrics_clone(&self) -> Option<crate::api::XrayMetrics> {
        if self.is_core_transitioning() {
            return None;
        }
        match self.runtime.try_lock() {
            Ok(runtime) => runtime.xray_metrics_clone(),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Poisoned(poisoned)) => {
                app_log::error("lock", "runtime lock was poisoned — recovering");
                poisoned.into_inner().xray_metrics_clone()
            }
        }
    }

    /// Apply an Xray metrics sample from the currently active core session.
    /// Traffic totals only — no per-connection data exists under Xray.
    pub fn try_apply_metrics_snapshot(
        &self,
        metrics: &crate::api::XrayMetrics,
        totals: crate::api::TrafficTotals,
    ) -> bool {
        if self.is_core_transitioning() || !metrics.is_active() {
            return false;
        }
        let mut runtime = match self.runtime.try_lock() {
            Ok(runtime) => runtime,
            Err(TryLockError::WouldBlock) => return false,
            Err(TryLockError::Poisoned(poisoned)) => {
                app_log::error("lock", "runtime lock was poisoned — recovering");
                poisoned.into_inner()
            }
        };
        if self.is_core_transitioning()
            || !metrics.is_active()
            || !runtime
                .xray_metrics_clone()
                .is_some_and(|current| current.same_session(metrics))
        {
            return false;
        }
        runtime.apply_snapshot(crate::api::ConnectionsSnapshot {
            upload_total: totals.upload_total,
            download_total: totals.download_total,
            connections: Vec::new(),
        });
        true
    }

    /// Apply only a snapshot from the currently active core session. If the
    /// runtime is busy, dropping one frame is safer than delaying a restart or
    /// applying stale data after it completes.
    pub fn try_apply_connection_snapshot(
        &self,
        api: &crate::api::ClashApi,
        mut snapshot: crate::api::ConnectionsSnapshot,
    ) -> bool {
        if self.is_core_transitioning() || !api.is_active() {
            return false;
        }
        let mut runtime = match self.runtime.try_lock() {
            Ok(runtime) => runtime,
            Err(TryLockError::WouldBlock) => return false,
            Err(TryLockError::Poisoned(poisoned)) => {
                app_log::error("lock", "runtime lock was poisoned — recovering");
                poisoned.into_inner()
            }
        };
        if self.is_core_transitioning()
            || !api.is_active()
            || !runtime
                .clash_api_clone()
                .is_some_and(|current| current.same_session(api))
        {
            return false;
        }
        // Some Clash derivatives report only the matched target in `chains` (["proxy"], not
        // ["node-x", "proxy"] like sing-box/mihomo), so every main-group
        // connection would display as "proxy". Resolve it to the persisted
        // current node — accurate in manual mode (the select's pick) and in
        // kernel mode (the url-test `now` synced by
        // schedule_kernel_selection_sync). Custom sing-box profiles keep
        // whatever their own config reports.
        if let Some(name) = self.generated_current_node_name() {
            for conn in &mut snapshot.connections {
                if conn.node == "proxy" {
                    conn.node = name.clone();
                }
            }
        }
        runtime.apply_snapshot(snapshot);
        true
    }

    /// Display name (alias applied) of the persisted current node for a
    /// generated runtime; `None` for custom sing-box profiles.
    fn generated_current_node_name(&self) -> Option<String> {
        let store = match self.store.try_lock() {
            Ok(store) => store,
            Err(TryLockError::WouldBlock) => return None,
            Err(TryLockError::Poisoned(poisoned)) => {
                app_log::error("lock", "store lock was poisoned — recovering");
                poisoned.into_inner()
            }
        };
        if store.settings.runtime_source().is_custom() {
            return None;
        }
        store
            .settings
            .current_node_id
            .as_deref()
            .and_then(|id| store.find_node(id))
            .map(|n| n.name.clone())
    }

    fn begin_core_transition(&self) -> AppResult<CoreTransitionGuard<'_>> {
        self.core_transitioning
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| crate::error::AppError::Core("内核正在切换，请稍候".into()))?;
        Ok(CoreTransitionGuard {
            flag: &self.core_transitioning,
        })
    }

    /// Run the (possibly slow, interactive) macOS setuid authorization
    /// *before* `start_proxy`/`restart_proxy` take the runtime/store locks.
    ///
    /// `ensure_core_setuid` can block for seconds waiting on a Touch ID /
    /// password prompt — or much longer if that system dialog never surfaces
    /// (headless/remote session). Doing that inside the runtime+store
    /// critical section used to hold both locks for the entire wait, which
    /// froze every other command that needed them (status polls fall back to
    /// a cache and survive, but node switches, rule toggles, and settings
    /// changes all queue on `lock_store`/`lock_runtime` and appear to hang
    /// the whole UI). Doing it here means only this one call is exposed to
    /// that latency; the store snapshot used to decide is read-and-released
    /// up front, so it never contends with the slow part.
    #[cfg(target_os = "macos")]
    fn pre_authorize_setuid_if_needed(&self, resource_dir: Option<&Path>) -> AppResult<()> {
        let pending = {
            let store = self.lock_store();
            crate::runtime::resolve_pending_elevation(&self.app_data_dir, resource_dir, &store)
        };
        if let Some(bin) = pending {
            crate::core::ensure_core_setuid(&bin)?;
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    fn pre_authorize_setuid_if_needed(&self, _resource_dir: Option<&Path>) -> AppResult<()> {
        Ok(())
    }

    fn cache_status(&self, status: &ProxyStatus) {
        *recover_lock(&self.status_cache, "status_cache") = status.clone();
    }

    fn cached_status(&self) -> ProxyStatus {
        recover_lock(&self.status_cache, "status_cache").clone()
    }

    fn mark_cached_core_state(&self, core_state: CoreState) {
        let mut status = recover_lock(&self.status_cache, "status_cache");
        status.core_state = core_state;
    }

    pub fn unload_ui_on_tray(&self) -> bool {
        self.with_store(|s| Ok(s.settings.unload_ui_on_tray))
            .unwrap_or(false)
    }

    pub fn with_store_mut<F, T>(&self, f: F) -> AppResult<T>
    where
        F: FnOnce(&mut AppStore) -> AppResult<T>,
    {
        let _persistence = self.lock_store_persistence();
        let (result, snapshot) = {
            let mut guard = self.lock_store();
            let result = f(&mut guard)?;
            (result, guard.clone())
        };
        snapshot.save(&self.store_path)?;
        Ok(result)
    }

    pub fn with_store<F, T>(&self, f: F) -> AppResult<T>
    where
        F: FnOnce(&AppStore) -> AppResult<T>,
    {
        let guard = self.lock_store();
        f(&guard)
    }

    pub fn start_proxy(
        &self,
        resource_dir: Option<&Path>,
        enable_system_proxy: bool,
    ) -> AppResult<ProxyStatus> {
        let _transition = self.begin_core_transition()?;
        self.mark_cached_core_state(CoreState::Starting);
        self.pre_authorize_setuid_if_needed(resource_dir)?;
        let mut runtime = self.lock_runtime();
        let _persistence = self.lock_store_persistence();
        let mut store = self.lock_store();
        let stored_capture = store.settings.capture_mode;
        let enable_system_proxy = match stored_capture {
            crate::domain::CaptureMode::System => true,
            crate::domain::CaptureMode::Tun => false,
            crate::domain::CaptureMode::Off => enable_system_proxy,
        };
        // Preserve compatibility with callers that explicitly request system
        // proxy on first start, while never overriding a saved TUN preference.
        if enable_system_proxy && stored_capture == crate::domain::CaptureMode::Off {
            store.settings.capture_mode = crate::domain::CaptureMode::System;
            store.settings.tun_enabled = false;
        }
        let mut status = runtime.start_proxy(
            &self.app_data_dir,
            resource_dir,
            &mut store,
            enable_system_proxy,
        )?;
        if runtime.system_proxy_on != enable_system_proxy {
            status = runtime.set_system_proxy(&store, enable_system_proxy)?;
        }
        if status.system_proxy {
            self.record_proxy_ownership(store.settings.mixed_port);
        } else if crate::proxy::cleanup_stale_owned_proxy(
            &self.app_data_dir,
            store.settings.mixed_port,
        )? {
            app_log::warn(
                "system_proxy",
                "cleared stale owned proxy while starting core",
            );
        }
        store.save(&self.store_path)?;
        self.cache_status(&status);
        Ok(status)
    }

    pub fn stop_proxy(&self) -> AppResult<ProxyStatus> {
        let _transition = self.begin_core_transition()?;
        self.mark_cached_core_state(CoreState::Stopping);
        let mut runtime = self.lock_runtime();
        let store = self.lock_store();
        let status = runtime.stop_proxy(&store)?;
        if crate::proxy::cleanup_stale_owned_proxy(&self.app_data_dir, store.settings.mixed_port)? {
            app_log::warn(
                "system_proxy",
                "cleared stale owned proxy while stopping core",
            );
        }
        self.cache_status(&status);
        Ok(status)
    }

    pub fn restart_proxy(&self, resource_dir: Option<&Path>) -> AppResult<ProxyStatus> {
        let _transition = self.begin_core_transition()?;
        self.mark_cached_core_state(CoreState::Starting);
        self.pre_authorize_setuid_if_needed(resource_dir)?;
        let mut runtime = self.lock_runtime();
        let _persistence = self.lock_store_persistence();
        let mut store = self.lock_store();
        let want_system = store.settings.capture_mode == crate::domain::CaptureMode::System;
        let mut status = runtime.restart_core(&self.app_data_dir, resource_dir, &mut store)?;
        if runtime.system_proxy_on != want_system {
            status = runtime.set_system_proxy(&store, want_system)?;
        }
        if status.system_proxy {
            self.record_proxy_ownership(store.settings.mixed_port);
        } else if crate::proxy::cleanup_stale_owned_proxy(
            &self.app_data_dir,
            store.settings.mixed_port,
        )? {
            app_log::warn(
                "system_proxy",
                "cleared stale owned proxy while restarting core",
            );
        }
        store.save(&self.store_path)?;
        self.cache_status(&status);
        Ok(status)
    }

    /// If core is running, regenerate config and restart so settings take effect.
    pub fn restart_if_running(
        &self,
        resource_dir: Option<&Path>,
    ) -> AppResult<Option<crate::runtime::ProxyStatus>> {
        if self.is_core_transitioning() {
            return Err(crate::error::AppError::Core("内核正在切换，请稍候".into()));
        }
        if !self.is_core_running() {
            return Ok(None);
        }
        let custom = self
            .with_store(|store| Ok(store.settings.runtime_source().is_custom()))
            .unwrap_or(false);
        if custom {
            // Never rebuild active.json or rewrite the user file.
            return Ok(None);
        }
        Ok(Some(self.restart_proxy(resource_dir)?))
    }

    pub fn proxy_status(&self) -> AppResult<ProxyStatus> {
        if self.is_core_transitioning() {
            return Ok(self.cached_status());
        }

        let mut runtime = match self.runtime.try_lock() {
            Ok(runtime) => runtime,
            Err(TryLockError::WouldBlock) => return Ok(self.cached_status()),
            Err(TryLockError::Poisoned(poisoned)) => {
                app_log::error("lock", "runtime lock was poisoned — recovering");
                poisoned.into_inner()
            }
        };
        let store = match self.store.try_lock() {
            Ok(store) => store,
            Err(TryLockError::WouldBlock) => return Ok(self.cached_status()),
            Err(TryLockError::Poisoned(poisoned)) => {
                app_log::error("lock", "store lock was poisoned — recovering");
                poisoned.into_inner()
            }
        };
        let status = runtime.status(&store);
        self.cache_status(&status);
        Ok(status)
    }

    pub fn live_connection_views(&self) -> Vec<ConnectionView> {
        if self.is_core_transitioning() {
            return recover_lock(&self.traffic_view_cache, "traffic_view_cache")
                .live
                .clone();
        }
        let mut runtime = match self.runtime.try_lock() {
            Ok(runtime) => runtime,
            Err(TryLockError::WouldBlock) => {
                return recover_lock(&self.traffic_view_cache, "traffic_view_cache")
                    .live
                    .clone()
            }
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        let store = match self.store.try_lock() {
            Ok(store) => store,
            Err(TryLockError::WouldBlock) => {
                return recover_lock(&self.traffic_view_cache, "traffic_view_cache")
                    .live
                    .clone()
            }
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        let rows = runtime.live_connections(&store);
        recover_lock(&self.traffic_view_cache, "traffic_view_cache").live = rows.clone();
        rows
    }

    pub fn live_connection_batch(
        &self,
        since_revision: Option<u64>,
        last_order_revision: Option<u64>,
    ) -> LiveConnectionBatch {
        let cached = || {
            let cache = recover_lock(&self.traffic_view_cache, "traffic_view_cache");
            if since_revision == Some(cache.live_revision) {
                LiveConnectionBatch {
                    rows: Vec::new(),
                    removed_ids: Vec::new(),
                    order_ids: None,
                    order_revision: cache.live_order_revision,
                    revision: cache.live_revision,
                    unchanged: true,
                    full: false,
                }
            } else {
                LiveConnectionBatch {
                    rows: cache.live.clone(),
                    removed_ids: Vec::new(),
                    order_ids: Some(cache.live.iter().map(|row| row.id.clone()).collect()),
                    order_revision: cache.live_order_revision,
                    revision: cache.live_revision,
                    unchanged: false,
                    full: true,
                }
            }
        };
        if self.is_core_transitioning() {
            return cached();
        }
        let mut runtime = match self.runtime.try_lock() {
            Ok(runtime) => runtime,
            Err(TryLockError::WouldBlock) => return cached(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        let store = match self.store.try_lock() {
            Ok(store) => store,
            Err(TryLockError::WouldBlock) => return cached(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        let batch = runtime.live_connection_batch(&store, since_revision, last_order_revision);
        if !batch.unchanged {
            let mut cache = recover_lock(&self.traffic_view_cache, "traffic_view_cache");
            if batch.full {
                cache.live = batch.rows.clone();
            } else {
                let removed: std::collections::HashSet<&str> =
                    batch.removed_ids.iter().map(String::as_str).collect();
                match &batch.order_ids {
                    Some(order) => {
                        let mut by_id: std::collections::HashMap<String, ConnectionView> = cache
                            .live
                            .drain(..)
                            .filter(|row| !removed.contains(row.id.as_str()))
                            .map(|row| (row.id.clone(), row))
                            .collect();
                        for row in &batch.rows {
                            by_id.insert(row.id.clone(), row.clone());
                        }
                        cache.live = order.iter().filter_map(|id| by_id.remove(id)).collect();
                    }
                    None => {
                        // Membership unchanged — overlay updates in place.
                        let updates: std::collections::HashMap<String, &ConnectionView> =
                            batch.rows.iter().map(|row| (row.id.clone(), row)).collect();
                        cache.live = cache
                            .live
                            .drain(..)
                            .filter(|row| !removed.contains(row.id.as_str()))
                            .map(|row| match updates.get(&row.id) {
                                Some(updated) => (*updated).clone(),
                                None => row,
                            })
                            .collect();
                    }
                }
            }
            cache.live_revision = batch.revision;
            cache.live_order_revision = batch.order_revision;
        }
        batch
    }

    pub fn request_views(
        &self,
        query: Option<&str>,
        limit: Option<usize>,
        failures_only: bool,
        after_seq: Option<u64>,
    ) -> RequestBatch {
        let query = query.unwrap_or("").trim().to_string();
        let cached = || {
            if let Some(cursor) = after_seq {
                return RequestBatch {
                    entries: Vec::new(),
                    cursor,
                };
            }
            let cache = recover_lock(&self.traffic_view_cache, "traffic_view_cache");
            let entry = if failures_only {
                &cache.failures
            } else {
                &cache.requests
            };
            if entry.query == query && entry.limit == limit {
                RequestBatch {
                    entries: entry.rows.clone(),
                    cursor: entry.cursor,
                }
            } else {
                RequestBatch::default()
            }
        };
        if self.is_core_transitioning() {
            return cached();
        }
        let mut runtime = match self.runtime.try_lock() {
            Ok(runtime) => runtime,
            Err(TryLockError::WouldBlock) => return cached(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        let store = match self.store.try_lock() {
            Ok(store) => store,
            Err(TryLockError::WouldBlock) => return cached(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        let rows = if failures_only {
            runtime.request_failures(&store, Some(&query), limit, after_seq)
        } else {
            runtime.request_history(&store, Some(&query), limit, after_seq)
        };
        if after_seq.is_some() {
            return rows;
        }
        let mut cache = recover_lock(&self.traffic_view_cache, "traffic_view_cache");
        let entry = if failures_only {
            &mut cache.failures
        } else {
            &mut cache.requests
        };
        entry.query = query;
        entry.limit = limit;
        entry.rows = rows.entries.clone();
        entry.cursor = rows.cursor;
        rows
    }

    pub fn clear_request_history_nonblocking(&self) -> AppResult<()> {
        if self.is_core_transitioning() {
            return Err(crate::error::AppError::Core("内核正在切换，请稍候".into()));
        }
        let mut runtime = match self.runtime.try_lock() {
            Ok(runtime) => runtime,
            Err(TryLockError::WouldBlock) => {
                return Err(crate::error::AppError::Core("内核正忙，请稍候".into()))
            }
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        runtime.clear_request_history();
        let mut cache = recover_lock(&self.traffic_view_cache, "traffic_view_cache");
        cache.requests.rows.clear();
        cache.failures.rows.clear();
        Ok(())
    }

    /// Run a Clash selector update without holding `runtime` across HTTP I/O.
    /// The transition guard prevents a core restart from replacing the API
    /// endpoint between cloning the handle and applying the selection.
    pub fn select_group_live_serialized(
        &self,
        group: &str,
        node_tag: &str,
        close_connections: bool,
    ) -> AppResult<bool> {
        let _operation = self.begin_core_transition()?;
        let api = {
            let runtime = self.lock_runtime();
            runtime.clash_api_clone()
        };
        let Some(api) = api else {
            return Ok(false);
        };

        api.select_proxy(group, node_tag)?;
        if close_connections {
            let _ = api.close_all_connections();
        }
        Ok(true)
    }

    /// Select the main proxy node and persist it under the same operation
    /// guard, so a manual click and a smart-switch apply cannot overwrite one
    /// another mid-flight.
    ///
    /// Returns `(settings, restart_needed, switched_live)`. Under Xray there
    /// is no live selection API — the pick is persisted and the caller must
    /// restart the core (`restart_needed = true`).
    pub fn select_current_node_serialized(
        &self,
        node_id: &str,
        manual: bool,
        close_if_enabled: bool,
    ) -> AppResult<(crate::domain::AppSettings, bool, bool)> {
        let _operation = self.begin_core_transition()?;
        let core_kind = {
            let kind = crate::core::CoreKind::parse(
                self.with_store(|store| Ok(store.settings.core_type.clone()))?
                    .as_str(),
            );
            kind
        };
        let (tag, should_close, kernel_auto) = self.with_store(|store| {
            if store.settings.runtime_source().is_custom() {
                return Err(crate::error::AppError::Core(
                    "自写配置模式下无法切换节点".into(),
                ));
            }
            if !manual && !store.settings.auto_select.is_smart() {
                return Err(crate::error::AppError::Core("智能切换已关闭".into()));
            }
            let node = store
                .find_node(node_id)
                .ok_or_else(|| crate::error::AppError::NotFound(node_id.to_string()))?;
            // Node-level compatibility (protocol / per-core node-shape
            // transport limits) lives in CoreKind::supports_node — the same
            // predicate that filters listings and generation, so a pick can
            // never desync from what the config actually contains.
            if !core_kind.supports_node(node) {
                return Err(crate::error::AppError::Core(format!(
                    "{} 内核不支持该节点（协议/传输/REALITY 限制），请切换内核或选择其他节点",
                    core_kind.display_name()
                )));
            }
            Ok((
                crate::config::outbound_tag(node),
                close_if_enabled && store.settings.close_connections_on_switch,
                manual && store.settings.auto_select.is_kernel(),
            ))
        })?;
        let api = {
            let runtime = self.lock_runtime();
            runtime.clash_api_clone()
        };
        // Kernel-auto main group is urltest: PUT /proxies would 400. Persist the
        // manual pick; the caller rebuilds a selector group via core restart.
        // Xray has no selection API at all — same restart path. mihomo (like
        // sing-box) selects live through its Clash-compatible API.
        let selected_live = if core_kind == crate::core::CoreKind::Xray || kernel_auto {
            false
        } else if let Some(api) = api {
            api.select_proxy("proxy", &tag)?;
            if should_close {
                let _ = api.close_all_connections();
            }
            true
        } else {
            false
        };

        let node_id = node_id.to_string();
        let (settings, was_kernel) = self.with_store_mut(|store| {
            let was_kernel = apply_selected_node(&mut store.settings, node_id, manual);
            Ok((store.settings.clone(), was_kernel))
        })?;
        let restart_needed =
            was_kernel || (core_kind == crate::core::CoreKind::Xray && self.is_core_running());
        Ok((settings, restart_needed, selected_live))
    }

    /// When auto_select=kernel, read Clash API group `now` and persist as current_node_id.
    pub fn schedule_kernel_selection_sync(app: tauri::AppHandle) {
        use tauri::Manager;

        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let kernel_mode = state
            .with_store(|store| {
                Ok(store.settings.auto_select == crate::domain::AutoSelectMode::Kernel)
            })
            .unwrap_or(false);
        if !kernel_mode
            || state.is_core_transitioning()
            || !recover_lock(&state.kernel_selection_poll, "kernel_selection_poll")
                .try_start(Instant::now())
        {
            return;
        }

        tauri::async_runtime::spawn_blocking(move || {
            if let Some(state) = app.try_state::<AppState>() {
                state.sync_kernel_selection_outside_runtime_lock();
                recover_lock(&state.kernel_selection_poll, "kernel_selection_poll").finish();
            }
        });
    }

    /// Mirror the kernel urltest selection without holding Runtime during HTTP.
    fn sync_kernel_selection_outside_runtime_lock(&self) {
        use crate::config::outbound_tag;
        use crate::domain::AutoSelectMode;

        let mode = match self.with_store(|s| Ok(s.settings.auto_select)) {
            Ok(m) => m,
            Err(_) => return,
        };
        if mode != AutoSelectMode::Kernel {
            return;
        }

        let (api, metrics) = {
            let mut runtime = self.lock_runtime();
            runtime.core.poll();
            if !runtime.core.is_running() {
                return;
            }
            (runtime.api_clone(), runtime.xray_metrics_clone())
        };
        // sing-box / mihomo: read the urltest group's `now` directly.
        // Xray: no selection API exists — infer the balancer's live pick from
        // the per-outbound stats counters (the picked outbound is the one
        // whose counters grow between polls; idle polls keep the last pick).
        // XrayMetrics only exists in Xray mode, so its presence is the check.
        let now_tag = if let Some(api) = api {
            match api.proxy_group_now_with_timeout("proxy", KERNEL_SELECTION_HTTP_TIMEOUT) {
                Ok(tag) => tag,
                Err(_) => return,
            }
        } else if let Some(metrics) = metrics {
            metrics.dominant_outbound_tag()
        } else {
            return;
        };
        let Some(tag) = now_tag else {
            return;
        };

        let node_id = match self.with_store(|store| {
            Ok(store
                .nodes
                .iter()
                .find(|n| outbound_tag(&n.node) == tag)
                .map(|n| n.node.id.clone()))
        }) {
            Ok(id) => id,
            Err(_) => return,
        };
        let Some(node_id) = node_id else {
            return;
        };

        let changed = self
            .with_store(|s| Ok(s.settings.current_node_id.as_deref() != Some(node_id.as_str())))
            .unwrap_or(false);
        if !changed {
            return;
        }

        if let Err(e) = self.with_store_mut(|store| {
            store.settings.current_node_id = Some(node_id.clone());
            Ok(())
        }) {
            app_log::warn(
                "auto_select",
                format!("persist kernel selection failed: {e}"),
            );
            return;
        }
        app_log::info(
            "auto_select",
            format!("kernel urltest now → node {node_id} ({tag})"),
        );
    }

    pub fn shutdown_runtime(&self) {
        let mut runtime = self.lock_runtime();
        if runtime.shutdown() {
            drop(runtime);
            if let Err(error) = self.cleanup_stale_system_proxy() {
                app_log::warn(
                    "system_proxy",
                    format!("shutdown ownership cleanup failed: {error}"),
                );
            }
        }
    }

    pub fn cleanup_stale_system_proxy(&self) -> AppResult<bool> {
        let mixed_port = self.with_store(|store| Ok(store.settings.mixed_port))?;
        crate::proxy::cleanup_stale_owned_proxy(&self.app_data_dir, mixed_port)
    }

    fn record_proxy_ownership(&self, port: u16) {
        if let Err(error) = crate::proxy::record_ownership(&self.app_data_dir, "127.0.0.1", port) {
            app_log::error("system_proxy", format!("record ownership failed: {error}"));
        }
    }

    fn clear_proxy_ownership(&self) {
        if let Err(error) = crate::proxy::clear_ownership(&self.app_data_dir) {
            app_log::warn("system_proxy", format!("clear ownership failed: {error}"));
        }
    }

    pub fn is_core_running(&self) -> bool {
        // Background schedulers must skip work while the endpoint is being
        // replaced; waiting here can occupy an async worker for 6–10 seconds.
        if self.is_core_transitioning() {
            return false;
        }

        let mut runtime = match self.runtime.try_lock() {
            Ok(runtime) => runtime,
            Err(TryLockError::WouldBlock) => return self.cached_status().running,
            Err(TryLockError::Poisoned(poisoned)) => {
                app_log::error("lock", "runtime lock was poisoned — recovering");
                poisoned.into_inner()
            }
        };
        runtime.core.poll();
        let running = runtime.core.is_running();
        let core_state = runtime.core.state();
        drop(runtime);

        let mut cached = recover_lock(&self.status_cache, "status_cache");
        cached.running = running;
        cached.core_state = core_state;
        running
    }

    pub fn set_system_proxy(
        &self,
        enabled: bool,
        resource_dir: Option<&Path>,
    ) -> AppResult<ProxyStatus> {
        self.set_capture_mode(if enabled { "system" } else { "off" }, resource_dir)
    }

    /// Toggle TUN mode. When core is running, regenerate config and restart.
    pub fn set_tun_enabled(
        &self,
        enabled: bool,
        resource_dir: Option<&Path>,
    ) -> AppResult<ProxyStatus> {
        self.set_capture_mode(if enabled { "tun" } else { "off" }, resource_dir)
    }

    /// Traffic capture mode (mutually exclusive): `off` | `system` | `tun`.
    ///
    /// - off: system proxy off, TUN off  
    /// - system: TUN off, system proxy on  
    /// - tun: system proxy off, TUN on  
    pub fn set_capture_mode(
        &self,
        mode: &str,
        resource_dir: Option<&Path>,
    ) -> AppResult<ProxyStatus> {
        let _transition = self.begin_core_transition()?;
        let mode = crate::domain::CaptureMode::parse(mode).ok_or_else(|| {
            crate::error::AppError::Core("capture mode must be off | system | tun".into())
        })?;
        let mut runtime = self.lock_runtime();
        let _persistence = self.lock_store_persistence();
        let mut store = self.lock_store();
        runtime.core.poll();

        if store.settings.runtime_source().is_custom() && mode == crate::domain::CaptureMode::Tun {
            return Err(crate::error::AppError::Core(
                "自写配置模式下无法改写 TUN，请在配置文件里自行声明".into(),
            ));
        }
        let want_tun = mode == crate::domain::CaptureMode::Tun;
        let want_sys = mode == crate::domain::CaptureMode::System;
        let tun_now = store.settings.tun_enabled;
        let sys_now = runtime.system_proxy_on;

        // Runtime state is process-local. After an unclean exit it may say
        // "off" while the OS still points at our dead loopback endpoint.
        if !want_sys && !sys_now {
            if crate::proxy::cleanup_stale_owned_proxy(
                &self.app_data_dir,
                store.settings.mixed_port,
            )? {
                app_log::warn(
                    "system_proxy",
                    "cleared stale owned proxy while switching off",
                );
            }
        }

        if tun_now == want_tun && sys_now == want_sys && store.settings.capture_mode == mode {
            let status = runtime.status(&store);
            self.cache_status(&status);
            return Ok(status);
        }

        store.settings.capture_mode = mode;

        // 1) TUN setting / restart first (heavier).
        if tun_now != want_tun {
            store.settings.tun_enabled = want_tun;
            store.save(&self.store_path)?;
            if runtime.core.is_running() {
                self.mark_cached_core_state(CoreState::Starting);
                runtime.restart_core(&self.app_data_dir, resource_dir, &mut store)?;
                store.save(&self.store_path)?;
            }
        }

        // 2) System proxy: always align with mode (TUN implies proxy off).
        if runtime.system_proxy_on != want_sys {
            runtime.set_system_proxy(&store, want_sys)?;
        }

        if want_sys && runtime.system_proxy_on {
            self.record_proxy_ownership(store.settings.mixed_port);
        } else if !want_sys {
            self.clear_proxy_ownership();
        }

        store.save(&self.store_path)?;

        let status = runtime.status(&store);
        self.cache_status(&status);
        Ok(status)
    }

    /// Clash-style rule / global / direct. Restarts core when running.
    pub fn set_outbound_mode(
        &self,
        mode: crate::domain::OutboundMode,
        resource_dir: Option<&Path>,
    ) -> AppResult<ProxyStatus> {
        let _transition = self.begin_core_transition()?;
        let mut runtime = self.lock_runtime();
        let _persistence = self.lock_store_persistence();
        let mut store = self.lock_store();

        if store.settings.runtime_source().is_custom() {
            return Err(crate::error::AppError::Core(
                "自写配置模式下无法切换规则 / 全局 / 直连".into(),
            ));
        }
        if store.settings.outbound_mode == mode {
            let status = runtime.status(&store);
            self.cache_status(&status);
            return Ok(status);
        }
        store.settings.outbound_mode = mode;
        store.save(&self.store_path)?;

        runtime.core.poll();
        if runtime.core.is_running() {
            self.mark_cached_core_state(CoreState::Starting);
            let status = runtime.restart_core(&self.app_data_dir, resource_dir, &mut store)?;
            store.save(&self.store_path)?;
            self.cache_status(&status);
            Ok(status)
        } else {
            let status = runtime.status(&store);
            self.cache_status(&status);
            Ok(status)
        }
    }

    /// User-triggered clash_api secret rotation. Persists a new secret and
    /// restarts a running core so the regenerated config picks it up.
    pub fn regenerate_api_secret(
        &self,
        resource_dir: Option<&Path>,
    ) -> AppResult<crate::domain::AppSettings> {
        let _transition = self.begin_core_transition()?;
        let mut runtime = self.lock_runtime();
        let _persistence = self.lock_store_persistence();
        let mut store = self.lock_store();

        if store.settings.runtime_source().is_custom() {
            return Err(crate::error::AppError::Core(
                "自写配置模式下无法重新生成密钥，请在配置文件里自行修改".into(),
            ));
        }
        if !store.settings.api_secret_enabled {
            return Err(crate::error::AppError::Core(
                "密钥已关闭，请先开启密钥保护后再重新生成".into(),
            ));
        }

        store.settings.clash_api_secret = Some(crate::config::generate_api_secret());
        store.save(&self.store_path)?;

        runtime.core.poll();
        if runtime.core.is_running() {
            self.mark_cached_core_state(CoreState::Starting);
            runtime.restart_core(&self.app_data_dir, resource_dir, &mut store)?;
            store.save(&self.store_path)?;
        }
        let status = runtime.status(&store);
        self.cache_status(&status);

        Ok(store.settings.clone())
    }

    /// Last observed core state from the status cache; `is_core_running`
    /// refreshes it as a side effect (and reaps a dead child first).
    pub fn cached_core_state(&self) -> CoreState {
        self.cached_status().core_state
    }

    /// Poll the companion Xray sidecar (reap a dead child, read liveness),
    /// best-effort: `None` when the runtime lock is contended — the watchdog
    /// just skips that tick instead of blocking an async worker.
    pub fn poll_sidecar(&self) -> Option<(bool, CoreState)> {
        let mut runtime = match self.runtime.try_lock() {
            Ok(runtime) => runtime,
            Err(TryLockError::WouldBlock) => return None,
            Err(TryLockError::Poisoned(poisoned)) => {
                app_log::error("lock", "runtime lock was poisoned — recovering");
                poisoned.into_inner()
            }
        };
        runtime.sidecar.poll();
        Some((runtime.sidecar.is_running(), runtime.sidecar.state()))
    }
}

// —— core watchdog ——————————————————————————————————————————————

/// A core that flips running → error without a user-initiated stop is
/// auto-restarted through the regular debounced apply-and-restart path.
/// Motivated by a field incident of a core exiting silently (code 1,
/// no log line) minutes after start; generic across cores. An attempt
/// budget inside a rolling window keeps a config-error death-loop from
/// spinning forever — after the budget is spent the core stays down and
/// the error surfaces in the UI as before.
const WATCHDOG_POLL_MS: u64 = 2000;
const WATCHDOG_MAX_ATTEMPTS: usize = 3;
const WATCHDOG_WINDOW: Duration = Duration::from_secs(600);

/// Pure decision core (unit-tested): restart only on the running→not-running
/// edge, only for the `Error` state (a deliberate stop lands on `Stopped`),
/// never during a core transition, and only within the attempt budget.
fn watchdog_should_restart(
    was_running: bool,
    now_running: bool,
    transitioning: bool,
    core_state: CoreState,
    attempts_in_window: usize,
) -> bool {
    !now_running
        && was_running
        && !transitioning
        && core_state == CoreState::Error
        && attempts_in_window < WATCHDOG_MAX_ATTEMPTS
}

pub fn spawn_core_watchdog(app: tauri::AppHandle) {
    use tauri::Manager;
    std::thread::Builder::new()
        .name("core-watchdog".into())
        .spawn(move || {
            let mut was_running = false;
            let mut attempts: Vec<Instant> = Vec::new();
            // Companion Xray sidecar gets its own edge/budget tracking: it
            // crashes independently of the main core (which stays Running),
            // so the main-core inputs alone never see the failure.
            let mut sidecar_was_running = false;
            let mut sidecar_attempts: Vec<Instant> = Vec::new();
            loop {
                std::thread::sleep(Duration::from_millis(WATCHDOG_POLL_MS));
                let Some(state) = app.try_state::<AppState>() else {
                    continue;
                };
                // is_core_running also reaps a dead child, flipping the
                // cached state to error before we read it.
                let now_running = state.is_core_running();
                let transitioning = state.is_core_transitioning();
                let core_state = state.cached_core_state();
                let now = Instant::now();
                attempts.retain(|t| now.duration_since(*t) < WATCHDOG_WINDOW);
                if watchdog_should_restart(
                    was_running,
                    now_running,
                    transitioning,
                    core_state,
                    attempts.len(),
                ) {
                    attempts.push(now);
                    app_log::warn(
                        "core",
                        format!(
                            "core died unexpectedly (state {core_state:?}) — auto-restarting (attempt {}/{WATCHDOG_MAX_ATTEMPTS} in {}s)",
                            attempts.len(),
                            WATCHDOG_WINDOW.as_secs()
                        ),
                    );
                    crate::rule_apply::request_restart(app.clone(), Vec::new());
                }
                was_running = now_running;

                // Sidecar watchdog: only meaningful while the main core is
                // up (stop paths tear the sidecar down with it, landing on
                // Stopped, which the edge check rejects).
                let Some((sidecar_running, sidecar_state)) = state.poll_sidecar() else {
                    continue;
                };
                sidecar_attempts.retain(|t| now.duration_since(*t) < WATCHDOG_WINDOW);
                if watchdog_should_restart(
                    sidecar_was_running,
                    sidecar_running,
                    transitioning,
                    sidecar_state,
                    sidecar_attempts.len(),
                ) && now_running
                {
                    sidecar_attempts.push(now);
                    app_log::warn(
                        "core",
                        format!(
                            "xray sidecar died unexpectedly (state {sidecar_state:?}) — auto-restarting (attempt {}/{WATCHDOG_MAX_ATTEMPTS} in {}s)",
                            sidecar_attempts.len(),
                            WATCHDOG_WINDOW.as_secs()
                        ),
                    );
                    crate::rule_apply::request_restart(app.clone(), Vec::new());
                }
                sidecar_was_running = sidecar_running;
            }
        })
        .ok();
}

#[cfg(test)]
mod watchdog_tests {
    use super::*;

    #[test]
    fn restarts_only_on_error_edge() {
        // running → error, no transition, budget left → restart.
        assert!(watchdog_should_restart(
            true,
            false,
            false,
            CoreState::Error,
            0
        ));
        // Deliberate stop lands on "stopped" → never auto-restart.
        assert!(!watchdog_should_restart(
            true,
            false,
            false,
            CoreState::Stopped,
            0
        ));
        // No edge (was already down) or mid-transition → skip.
        assert!(!watchdog_should_restart(
            false,
            false,
            false,
            CoreState::Error,
            0
        ));
        assert!(!watchdog_should_restart(
            true,
            false,
            true,
            CoreState::Error,
            0
        ));
        // Budget exhausted inside the window → skip.
        assert!(!watchdog_should_restart(
            true,
            false,
            false,
            CoreState::Error,
            WATCHDOG_MAX_ATTEMPTS
        ));
        assert!(!watchdog_should_restart(
            true,
            false,
            false,
            CoreState::Error,
            WATCHDOG_MAX_ATTEMPTS + 2
        ));
        // Still running → nothing to do.
        assert!(!watchdog_should_restart(
            true,
            true,
            false,
            CoreState::Running,
            0
        ));
    }
}
