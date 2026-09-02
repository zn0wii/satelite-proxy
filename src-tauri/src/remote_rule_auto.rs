//! Download remote rule sets in the app, so sing-box only loads local files.

use crate::domain::RuleSet;
use crate::state::AppState;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const EVENT: &str = "remote-rule-set-status";
const MAX_BYTES: usize = 32 * 1024 * 1024;
const TICK_SECS: u64 = 60;
const AUTO_UPDATE_CONCURRENCY: usize = 3;

static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

struct ActiveDownload {
    id: String,
}

impl ActiveDownload {
    fn acquire(id: &str) -> Result<Self, String> {
        let mut active = ACTIVE
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .map_err(|_| "remote rule download lock poisoned".to_string())?;
        if !active.insert(id.to_string()) {
            return Err("该远程规则集正在下载".into());
        }
        Ok(Self { id: id.into() })
    }
}

impl Drop for ActiveDownload {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE.get_or_init(|| Mutex::new(HashSet::new())).lock() {
            active.remove(&self.id);
        }
    }
}

#[derive(Clone, Copy)]
enum RuleSetFileFormat {
    Source,
    Binary,
}

impl RuleSetFileFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Binary => "binary",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Source => "json",
            Self::Binary => "srs",
        }
    }
}

#[derive(Clone, Serialize)]
struct StatusEvent {
    id: String,
    status: String,
    error: Option<String>,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn emit(app: &AppHandle, id: &str, status: &str, error: Option<String>) {
    let _ = app.emit(
        EVENT,
        StatusEvent {
            id: id.to_string(),
            status: status.to_string(),
            error,
        },
    );
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        match cleanup_orphaned_cache(&app).await {
            Ok(removed) if removed > 0 => crate::app_log::info(
                "remote_rules",
                format!("removed {removed} orphaned cache file(s)"),
            ),
            Err(error) => crate::app_log::warn(
                "remote_rules",
                format!("orphaned cache cleanup failed: {error}"),
            ),
            _ => {}
        }
        loop {
            let due = due_ids(&app);
            let mut changed = false;
            let mut cleanup_after_apply = Vec::new();
            let mut pending = due.into_iter();
            let mut downloads = tokio::task::JoinSet::new();
            for _ in 0..AUTO_UPDATE_CONCURRENCY {
                if let Some(id) = pending.next() {
                    spawn_auto_download(&mut downloads, app.clone(), id);
                }
            }
            while let Some(joined) = downloads.join_next().await {
                match joined {
                    Ok((_, Ok(downloaded))) => {
                        changed = true;
                        cleanup_after_apply.extend(downloaded.cleanup_after_apply);
                    }
                    Ok((id, Err(error))) => crate::app_log::warn(
                        "remote_rules",
                        format!("refresh {id} failed: {error}"),
                    ),
                    Err(error) => crate::app_log::warn(
                        "remote_rules",
                        format!("refresh task failed: {error}"),
                    ),
                }
                if let Some(id) = pending.next() {
                    spawn_auto_download(&mut downloads, app.clone(), id);
                }
            }
            // Apply the entire due set with one restart instead of restarting
            // once after every sequential download.
            if changed {
                crate::rule_apply::request_restart(app.clone(), cleanup_after_apply);
            }
            tokio::time::sleep(Duration::from_secs(TICK_SECS)).await;
        }
    });
}

async fn cleanup_orphaned_cache(app: &AppHandle) -> Result<usize, String> {
    let cache_dir = crate::portable::resolve_app_data_dir(app)
        .map_err(|error| error.to_string())?
        .join("remote-rule-sets");
    let referenced = app
        .try_state::<AppState>()
        .ok_or_else(|| "app state unavailable".to_string())?
        .with_store(|store| {
            Ok(store
                .rule_sets
                .iter()
                .filter_map(|set| set.remote.as_ref()?.local_path.as_ref())
                .map(PathBuf::from)
                .collect::<HashSet<_>>())
        })
        .map_err(|error| error.to_string())?;

    tauri::async_runtime::spawn_blocking(move || cleanup_cache_dir(&cache_dir, &referenced))
        .await
        .map_err(|error| error.to_string())?
}

fn cleanup_cache_dir(cache_dir: &Path, referenced: &HashSet<PathBuf>) -> Result<usize, String> {
    let entries = match std::fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.to_string()),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_rule_cache = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "json" | "srs"));
        if !is_rule_cache || referenced.contains(&path) || !path.is_file() {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) => crate::app_log::warn(
                "remote_rules",
                format!(
                    "failed to remove orphaned cache {}: {error}",
                    path.display()
                ),
            ),
        }
    }
    Ok(removed)
}

fn spawn_auto_download(
    downloads: &mut tokio::task::JoinSet<(String, Result<DownloadedRule, String>)>,
    app: AppHandle,
    id: String,
) {
    downloads.spawn(async move {
        let result = refresh_download(app, id.clone()).await;
        (id, result)
    });
}

fn due_ids(app: &AppHandle) -> Vec<String> {
    let Some(state) = app.try_state::<AppState>() else {
        return Vec::new();
    };
    let now = now_secs();
    state
        .with_store(|store| {
            Ok(store
                .rule_sets
                .iter()
                .filter_map(|set| {
                    let remote = set.remote.as_ref()?;
                    let interval =
                        crate::domain::remote_update_interval_secs(&remote.update_interval);
                    let due = interval.is_some_and(|seconds| {
                        remote.download_status == "downloading"
                            || remote.local_path.is_none()
                            || now.saturating_sub(remote.last_attempt.unwrap_or(0)) >= seconds
                    });
                    due.then(|| set.id.clone())
                })
                .collect())
        })
        .unwrap_or_default()
}

pub async fn refresh(app: AppHandle, id: String) -> Result<RuleSet, String> {
    let downloaded = refresh_download(app.clone(), id).await?;
    crate::rule_apply::request_restart(app, downloaded.cleanup_after_apply);
    Ok(downloaded.set)
}

struct DownloadedRule {
    set: RuleSet,
    cleanup_after_apply: Vec<std::path::PathBuf>,
}

async fn refresh_download(app: AppHandle, id: String) -> Result<DownloadedRule, String> {
    let _active = ActiveDownload::acquire(&id)?;
    refresh_inner(&app, &id).await
}

async fn refresh_inner(app: &AppHandle, id: &str) -> Result<DownloadedRule, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "app state unavailable".to_string())?;
    let attempt = now_secs();
    let use_proxy = state.is_core_running();
    let (url, mixed_port) = state
        .with_store_mut(|store| {
            let mixed_port = store.settings.mixed_port;
            let set = store
                .rule_sets
                .iter_mut()
                .find(|set| set.id == id)
                .ok_or_else(|| crate::error::AppError::NotFound(id.to_string()))?;
            let remote = set
                .remote
                .as_mut()
                .ok_or_else(|| crate::error::AppError::Config("该规则集不是远程规则集".into()))?;
            remote.download_status = "downloading".into();
            remote.download_error = None;
            remote.last_attempt = Some(attempt);
            Ok((remote.url.clone(), mixed_port))
        })
        .map_err(|error| error.to_string())?;
    emit(app, id, "downloading", None);

    let bytes = match download(&url, use_proxy.then_some(mixed_port)).await {
        Ok(bytes) => Ok(bytes),
        Err(first) if use_proxy => download(&url, None)
            .await
            .map_err(|second| format!("代理下载失败: {first}; 直连下载失败: {second}")),
        Err(error) => Err(error),
    };

    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) => return fail(app, id, error),
    };
    let (format, source_scan, binary_scan) = match validate_source(&bytes) {
        Ok((count, contains_ip)) => (RuleSetFileFormat::Source, Some((count, contains_ip)), None),
        Err(_) if bytes.starts_with(b"SRS") => {
            // Structural parse validates the binary container without the
            // core, and is the only validation possible for AdGuard
            // rule-sets, which `sing-box rule-set decompile` refuses.
            match crate::srs::parse(&bytes) {
                Ok(parsed) => (RuleSetFileFormat::Binary, None, Some(parsed)),
                Err(error) => return fail(app, id, format!("SRS 校验失败: {error}")),
            }
        }
        Err(error) => return fail(app, id, error),
    };

    let cache_dir = match crate::portable::resolve_app_data_dir(app) {
        Ok(path) => path.join("remote-rule-sets"),
        Err(error) => return fail(app, id, error.to_string()),
    };
    let safe_id: String = id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let path = cache_dir.join(format!("{safe_id}-{attempt}.{}", format.extension()));
    let write_path = path.clone();
    let write_dir = cache_dir.clone();
    let write_result = tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        std::fs::create_dir_all(&write_dir).map_err(|error| error.to_string())?;
        std::fs::write(&write_path, bytes).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|result| result);
    if let Err(error) = write_result {
        return fail(app, id, error);
    }

    let (rule_count, contains_ip) = match source_scan {
        Some(scan) => scan,
        None => {
            let parsed = binary_scan.expect("binary sets are scanned before writing");
            if parsed.has_adguard {
                // AdGuard rule-sets cannot be decompiled by sing-box; the
                // structural scan above already validated the file. Their
                // content is domain-only (`ad_guard_domain` lines), never IP.
                (parsed.display_count, false)
            } else {
                let resource_dir = app.path().resource_dir().ok();
                let (core, _) = crate::core::resolve_core_bin(
                    &state.app_data_dir,
                    resource_dir.as_deref(),
                    crate::core::CoreKind::SingBox,
                );
                match core {
                    // Regular binary rule-set: decompile with the core for
                    // an against-the-core check and an exact JSON count.
                    Some(core) => {
                        let input = path.clone();
                        let result = tauri::async_runtime::spawn_blocking(move || {
                            let source = decompile_srs(&core, &input)?;
                            validate_source(&source)
                        })
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result);
                        match result {
                            Ok(scan) => scan,
                            Err(error) => {
                                let _ = std::fs::remove_file(&path);
                                return fail(app, id, error);
                            }
                        }
                    }
                    // No core available to decompile with: the structural
                    // scan already verified the file, accept it as-is.
                    // Content type is unknown; keep the DNS-side reference
                    // (assume domain-only) rather than pessimistically
                    // dropping it.
                    None => (parsed.display_count, false),
                }
            }
        }
    };

    let path_text = path.to_string_lossy().to_string();
    let updated = state
        .with_store_mut(|store| {
            let set = store
                .rule_sets
                .iter_mut()
                .find(|set| set.id == id)
                .ok_or_else(|| crate::error::AppError::NotFound(id.to_string()))?;
            let remote = set
                .remote
                .as_mut()
                .ok_or_else(|| crate::error::AppError::Config("该规则集不是远程规则集".into()))?;
            remote.format = format.as_str().to_string();
            let old_path = remote.local_path.replace(path_text);
            remote.download_status = "ready".into();
            remote.download_error = None;
            remote.last_update = Some(attempt);
            remote.rule_count = Some(rule_count);
            remote.contains_ip = Some(contains_ip);
            Ok((set.clone(), old_path))
        })
        .map_err(|error| error.to_string());
    let (set, old_path) = match updated {
        Ok(updated) => updated,
        Err(error) => return fail(app, id, error),
    };

    // The cache is ready even if applying it to a currently running core later
    // fails. Tell the UI to stop spinning and surface restart failure separately.
    emit(app, id, "ready", None);

    let mut cleanup_after_apply = Vec::new();
    if let Some(old_path) = old_path.filter(|old| old != &path.to_string_lossy()) {
        let old = std::path::PathBuf::from(old_path);
        if old.parent() == Some(cache_dir.as_path()) {
            cleanup_after_apply.push(old);
        }
    }
    Ok(DownloadedRule {
        set,
        cleanup_after_apply,
    })
}

async fn download(url: &str, proxy_port: Option<u16>) -> Result<Vec<u8>, String> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .user_agent("Satelite/1 remote-rule-set");
    if let Some(port) = proxy_port {
        builder = builder.proxy(
            reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).map_err(|e| e.to_string())?,
        );
    }
    let response = builder
        .build()
        .map_err(|error| error.to_string())?
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    crate::services::http_body::read_limited(response, MAX_BYTES, "远程规则集超过 32 MB".into())
        .await
}

/// Validates a source-format rule-set and returns its display count plus
/// whether any rule (including nested logical ones) carries an `ip_cidr`
/// condition. `contains_ip` feeds the config builder's decision to skip a
/// DNS-side `rule_set` reference for new-enough sing-box cores — see
/// `rules_contain_ip_cidr`.
fn validate_source(bytes: &[u8]) -> Result<(u32, bool), String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("远程规则集不是有效的 sing-box source JSON: {error}"))?;
    let rules = value
        .get("rules")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "远程规则集缺少 rules 数组".to_string())?;
    if rules.is_empty() {
        return Err("远程规则集 rules 为空".into());
    }
    let count = rules
        .iter()
        .try_fold(0usize, |total, rule| {
            total.checked_add(crate::domain::remote_rule_display_count(rule))
        })
        .ok_or_else(|| "远程规则集条目数量过多".to_string())?;
    let count = u32::try_from(count).map_err(|_| "远程规则集条目数量过多".to_string())?;
    Ok((count, crate::domain::rules_contain_ip_cidr(rules)))
}

/// IP scan for a cached source-format rule-set: `Some(verdict)` when the
/// file is valid source JSON with a non-empty rules array, `None` otherwise
/// (unknown content keeps the conservative "assume domain-only" default).
fn source_cache_contains_ip(bytes: &[u8]) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let rules = value.get("rules")?.as_array()?;
    if rules.is_empty() {
        return None;
    }
    Some(crate::domain::rules_contain_ip_cidr(rules))
}

/// One-shot startup heal for rule sets cached by builds that predate the
/// `contains_ip` metadata: those entries carry `None`, which the config
/// builder reads as "assume domain-only" — wrong for IP-only sets, where
/// sing-box 1.14+ FATALs on the DNS-side `rule_set` reference once a fakeip
/// rule exists (Legacy Address Filter Fields). Re-scans each cached file
/// once and records the verdict; read/parse failures leave `None` untouched
/// so download-path semantics stay conservative. Runs after seeding so
/// freshly seeded entries (already scanned with the real rules) are skipped.
pub(crate) fn heal_contains_ip(store: &mut crate::storage::AppStore) {
    for set in store.rule_sets.iter_mut() {
        let Some(remote) = set.remote.as_mut() else {
            continue;
        };
        if remote.contains_ip.is_some() {
            continue;
        }
        let Some(path) = remote
            .local_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        let path = PathBuf::from(path);
        if !path.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let verdict = if remote.format == "binary" {
            // `parse_with_rules`: the IP scan reads the collected rules —
            // plain `parse` never fills them in (see `parsed_srs_contains_ip`).
            crate::srs::parse_with_rules(&bytes)
                .ok()
                .map(|parsed| crate::builtin_remote_rules::parsed_srs_contains_ip(&parsed))
        } else {
            source_cache_contains_ip(&bytes)
        };
        if let Some(contains_ip) = verdict {
            remote.contains_ip = Some(contains_ip);
        }
    }
}

/// Decompile and validate a binary `.srs` with the active sing-box core.
/// The temporary JSON is created beside the input and always removed.
pub(crate) fn decompile_srs(core: &Path, input: &Path) -> Result<Vec<u8>, String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    let output = input.with_extension(format!("decompiled-{}-{stamp}.json", std::process::id()));
    let mut command = Command::new(core);
    command
        .arg("rule-set")
        .arg("decompile")
        .arg(input)
        .arg("-o")
        .arg(&output);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let result = command
        .output()
        .map_err(|error| format!("无法运行 sing-box 校验 SRS: {error}"));
    let bytes = match result {
        Ok(result) if result.status.success() => {
            std::fs::read(&output).map_err(|error| format!("无法读取 SRS 反编译结果: {error}"))
        }
        Ok(result) => Err(format!(
            "SRS 校验失败: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )),
        Err(error) => Err(error),
    };
    let _ = std::fs::remove_file(output);
    bytes
}

fn fail<T>(app: &AppHandle, id: &str, error: String) -> Result<T, String> {
    if let Some(state) = app.try_state::<AppState>() {
        let _ = state.with_store_mut(|store| {
            if let Some(remote) = store
                .rule_sets
                .iter_mut()
                .find(|set| set.id == id)
                .and_then(|set| set.remote.as_mut())
            {
                remote.download_status = "error".into();
                remote.download_error = Some(error.clone());
            }
            Ok(())
        });
    }
    emit(app, id, "error", Some(error.clone()));
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_unreferenced_rule_cache_files() {
        let cache_dir = std::env::temp_dir().join(format!(
            "satelite-rule-cache-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&cache_dir).unwrap();
        let referenced_path = cache_dir.join("referenced.json");
        let orphaned_path = cache_dir.join("orphaned.srs");
        let unrelated_path = cache_dir.join("keep.txt");
        std::fs::write(&referenced_path, b"{}").unwrap();
        std::fs::write(&orphaned_path, b"SRS").unwrap();
        std::fs::write(&unrelated_path, b"keep").unwrap();

        let referenced = HashSet::from([referenced_path.clone()]);
        assert_eq!(cleanup_cache_dir(&cache_dir, &referenced), Ok(1));
        assert!(referenced_path.exists());
        assert!(!orphaned_path.exists());
        assert!(unrelated_path.exists());

        std::fs::remove_dir_all(cache_dir).unwrap();
    }

    #[test]
    fn active_download_guard_releases_id_when_dropped() {
        let id = format!(
            "remote-download-guard-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let guard = ActiveDownload::acquire(&id).expect("first download acquires id");
        assert!(ActiveDownload::acquire(&id).is_err());
        drop(guard);
        assert!(ActiveDownload::acquire(&id).is_ok());
    }

    /// `sing-box rule-set compile` output of
    /// `{"version":3,"rules":[{"ip_cidr":["1.0.1.0/24","1.0.2.0/23"]}]}`.
    const IP_ONLY_SRS: &[u8] = &[
        0x53, 0x52, 0x53, 0x02, 0x78, 0xda, 0x62, 0x64, 0x60, 0x63, 0x64, 0x80, 0x00, 0x46, 0x16,
        0x46, 0x06, 0x46, 0x06, 0x16, 0x46, 0x06, 0xe6, 0xff, 0xff, 0x19, 0x00, 0x01, 0x00, 0x00,
        0xff, 0xff, 0x06, 0x43, 0x02, 0x16,
    ];

    #[test]
    fn heal_contains_ip_backfills_stale_remote_sets() {
        // Regression (sing-box 1.14, 2026-09): sets downloaded before the
        // `contains_ip` metadata existed carry `None`; the builder then
        // assumes domain-only and emits a DNS-side `rule_set` reference that
        // sing-box 1.14 rejects as Legacy Address Filter Fields once a
        // fakeip rule exists. The heal re-scans each cache once.
        let dir = std::env::temp_dir().join(format!(
            "satelite-heal-contains-ip-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let ip_srs = dir.join("geoip.srs");
        std::fs::write(&ip_srs, IP_ONLY_SRS).unwrap();
        let domain_json = dir.join("block.json");
        std::fs::write(
            &domain_json,
            br#"{"version":3,"rules":[{"domain_suffix":["ads.example"]}]}"#,
        )
        .unwrap();
        let mixed_json = dir.join("mixed.json");
        std::fs::write(
            &mixed_json,
            br#"{"version":3,"rules":[{"domain_suffix":["a.com"]},{"ip_cidr":["1.0.1.0/24"]}]}"#,
        )
        .unwrap();
        let junk_srs = dir.join("junk.srs");
        std::fs::write(&junk_srs, b"<html>not srs</html>").unwrap();

        let mut binary_ip = RuleSet::new_remote(
            "Binary IP",
            "https://e.com/cn.srs",
            crate::domain::RuleTarget::Direct,
        );
        {
            let remote = binary_ip.remote.as_mut().unwrap();
            remote.format = "binary".into();
            remote.local_path = Some(ip_srs.to_string_lossy().to_string());
            remote.contains_ip = None;
        }
        let mut source_domain = RuleSet::new_remote(
            "Source Domain",
            "https://e.com/b.json",
            crate::domain::RuleTarget::Proxy,
        );
        {
            let remote = source_domain.remote.as_mut().unwrap();
            remote.format = "source".into();
            remote.local_path = Some(domain_json.to_string_lossy().to_string());
            remote.contains_ip = None;
        }
        let mut source_mixed = RuleSet::new_remote(
            "Source Mixed",
            "https://e.com/m.json",
            crate::domain::RuleTarget::Proxy,
        );
        {
            let remote = source_mixed.remote.as_mut().unwrap();
            remote.format = "source".into();
            remote.local_path = Some(mixed_json.to_string_lossy().to_string());
            remote.contains_ip = None;
        }
        let mut already_labeled = RuleSet::new_remote(
            "Labeled",
            "https://e.com/l.json",
            crate::domain::RuleTarget::Proxy,
        );
        {
            let remote = already_labeled.remote.as_mut().unwrap();
            remote.format = "source".into();
            remote.local_path = Some(domain_json.to_string_lossy().to_string());
            remote.contains_ip = Some(false);
        }
        let mut junk = RuleSet::new_remote(
            "Junk",
            "https://e.com/j.srs",
            crate::domain::RuleTarget::Proxy,
        );
        {
            let remote = junk.remote.as_mut().unwrap();
            remote.format = "binary".into();
            remote.local_path = Some(junk_srs.to_string_lossy().to_string());
            remote.contains_ip = None;
        }
        let mut missing_file = RuleSet::new_remote(
            "Missing",
            "https://e.com/x.json",
            crate::domain::RuleTarget::Proxy,
        );
        {
            let remote = missing_file.remote.as_mut().unwrap();
            remote.format = "source".into();
            remote.local_path = Some(
                dir.join("does-not-exist.json")
                    .to_string_lossy()
                    .to_string(),
            );
            remote.contains_ip = None;
        }

        let mut store = crate::storage::AppStore::default();
        store.rule_sets = vec![
            binary_ip,
            source_domain,
            source_mixed,
            already_labeled,
            junk,
            missing_file,
        ];
        heal_contains_ip(&mut store);

        let verdict = |name: &str| {
            store
                .rule_sets
                .iter()
                .find(|set| set.name == name)
                .and_then(|set| set.remote.as_ref())
                .and_then(|remote| remote.contains_ip)
        };
        assert_eq!(verdict("Binary IP"), Some(true));
        assert_eq!(verdict("Source Domain"), Some(false));
        assert_eq!(verdict("Source Mixed"), Some(true));
        // Already-labeled entries are not re-scanned; unreadable caches and
        // missing files keep `None` (conservative domain-only assumption).
        assert_eq!(verdict("Labeled"), Some(false));
        assert_eq!(verdict("Junk"), None);
        assert_eq!(verdict("Missing"), None);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn accepts_sing_box_source_json() {
        assert_eq!(
            validate_source(br#"{"version":3,"rules":[{"domain_suffix":["example.com"]}]}"#),
            Ok((1, false))
        );
    }

    #[test]
    fn counts_expanded_matcher_values() {
        assert_eq!(
            validate_source(
                br#"{"version":3,"rules":[{"domain_suffix":["a.com","b.com"],"ip_cidr":["10.0.0.0/8"]}]}"#
            ),
            Ok((3, true))
        );
    }

    #[test]
    fn rejects_html_and_empty_rules() {
        assert!(validate_source(b"<html>not a rule set</html>").is_err());
        assert!(validate_source(br#"{"version":3,"rules":[]}"#).is_err());
    }

    #[test]
    fn decompiles_binary_srs_with_bundled_core_when_available() {
        let Some(core) = crate::core::find_bundled_core(None, crate::core::CoreKind::SingBox)
        else {
            return;
        };
        const SRS: &[u8] = &[
            0x53, 0x52, 0x53, 0x02, 0x78, 0xda, 0x62, 0x64, 0x60, 0x62, 0x60, 0x64, 0x00, 0x03,
            0x01, 0x08, 0x83, 0x71, 0xd5, 0xaa, 0x55, 0x3c, 0xb9, 0xf9, 0xc9, 0x7a, 0xa9, 0x39,
            0x05, 0xb9, 0x89, 0x15, 0xa9, 0x5c, 0xff, 0x19, 0x00, 0x01, 0x00, 0x00, 0xff, 0xff,
            0x4d, 0xcc, 0x07, 0x83,
        ];
        let path = std::env::temp_dir().join(format!(
            "satelite-test-{}-{}.srs",
            std::process::id(),
            now_secs()
        ));
        std::fs::write(&path, SRS).unwrap();
        let result = decompile_srs(&core, &path).and_then(|bytes| validate_source(&bytes));
        let _ = std::fs::remove_file(path);
        assert_eq!(result, Ok((1, false)));
    }
}
