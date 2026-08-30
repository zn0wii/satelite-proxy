use crate::core::{
    active_core_version, bundled_core_version, detect_platform, download_latest_core_with_progress,
    fetch_latest_app_tag, fetch_latest_app_tag_via_redirect, fetch_latest_release_with_proxy,
    inspect_core_bin, CoreDownloadResult, CoreKind, CoreSource,
};
use crate::error::AppError;
use crate::state::AppState;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

const CORE_DOWNLOAD_EVENT: &str = "core-download-progress";

fn parse_kind(raw: Option<String>) -> CoreKind {
    CoreKind::parse(raw.as_deref().unwrap_or("singbox"))
}

#[derive(Debug, Serialize)]
pub struct CoreInfo {
    /// `singbox` | `xray`
    pub kind: String,
    pub name: String,
    pub installed: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub platform: String,
    /// Filled only when check_update=true (network). Otherwise null for instant UI.
    pub latest_version: Option<String>,
    pub update_available: bool,
    /// `bundled` | `downloaded` | `missing`
    pub source: String,
    pub bundled_version: Option<String>,
}

/// Local core status only (no network). Prefer this for page load.
#[tauri::command(async)]
pub fn get_core_info(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: Option<String>,
) -> Result<CoreInfo, String> {
    let kind = parse_kind(kind);
    let platform = detect_platform().map_err(|e| e.to_string())?;
    let resource_dir = app.path().resource_dir().ok();
    let res = resource_dir.as_deref();

    let (path, source) = inspect_core_bin(&state.app_data_dir, res, kind);
    // Metadata-only inspection: do not stage/copy the bundled core during page load.
    let version = active_core_version(&state.app_data_dir, res, kind);
    let bundled_version = bundled_core_version(res, kind);

    Ok(CoreInfo {
        kind: kind.as_str().into(),
        name: kind.display_name().into(),
        installed: path.is_some(),
        version,
        path: path.map(|p| p.display().to_string()),
        platform: platform.asset_suffix_for(kind).to_string(),
        latest_version: None,
        update_available: false,
        source: match source {
            CoreSource::Bundled => "bundled".into(),
            CoreSource::Downloaded => "downloaded".into(),
            CoreSource::Missing => "missing".into(),
        },
        bundled_version,
    })
}

/// Remote latest version only (network). Call after local info is shown.
#[tauri::command]
pub async fn check_core_update(
    state: State<'_, AppState>,
    kind: Option<String>,
    local_version: Option<String>,
) -> Result<CoreUpdateInfo, String> {
    let kind = parse_kind(kind);
    let proxy_url = current_download_proxy(&state)?;
    let latest = fetch_latest_release_with_proxy(kind, proxy_url.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    let update_available = match &local_version {
        Some(local) => is_newer_version(&latest.version, local),
        None => true,
    };
    Ok(CoreUpdateInfo {
        kind: kind.as_str().into(),
        latest_version: latest.version,
        update_available,
        asset_name: latest.asset_name,
        size: latest.size,
    })
}

#[derive(Debug, Serialize)]
pub struct CoreUpdateInfo {
    pub kind: String,
    pub latest_version: String,
    pub update_available: bool,
    pub asset_name: String,
    pub size: u64,
}

#[derive(Debug, Serialize)]
pub struct AppUpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    /// True when served from the local cache without touching the network.
    pub cached: bool,
    /// Unix seconds of the underlying check (cached or fresh).
    pub checked_at: Option<u64>,
}

/// Side-car cache for app update checks. Auto checks must not hammer GitHub
/// (unauthenticated api.github.com allows 60 req/h per IP — trivially
/// exhausted behind shared NAT/proxy exits), so results are cached and
/// failures back off instead of retrying on every page open.
#[derive(Debug, Default, Clone, Serialize, serde::Deserialize)]
struct AppUpdateCache {
    latest_version: Option<String>,
    /// Unix seconds of the last successful network check.
    checked_at: Option<u64>,
    /// Earliest unix seconds the next auto check may run (failure backoff).
    next_try_at: u64,
}

const APP_UPDATE_CACHE_FILE: &str = "update_cache.json";
/// Fresh window for an auto (non-forced) check result.
const APP_UPDATE_TTL_SECS: u64 = 6 * 3600;
/// After a failed network check, auto checks back off this long.
const APP_UPDATE_FAILURE_BACKOFF_SECS: u64 = 10 * 60;

fn load_app_update_cache(state: &AppState) -> AppUpdateCache {
    std::fs::read(state.app_data_dir.join(APP_UPDATE_CACHE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn store_app_update_cache(state: &AppState, cache: &AppUpdateCache) {
    if let Ok(json) = serde_json::to_vec(cache) {
        let _ = std::fs::write(state.app_data_dir.join(APP_UPDATE_CACHE_FILE), json);
    }
}

/// Latest app release from GitHub, for the Settings version tab. Routing
/// matches the core check (running mixed-port proxy when the core is up),
/// but the tag itself is read from the `releases/latest` page redirect first
/// — website budget, no API quota — with the REST API as fallback.
///
/// `force` (manual "check for updates") bypasses the cache; auto checks on
/// tab open serve a fresh (< 6h) or backoff-held cached result instead.
#[tauri::command]
pub async fn check_app_update(
    app: AppHandle,
    state: State<'_, AppState>,
    force: Option<bool>,
) -> Result<AppUpdateInfo, String> {
    let current = app.package_info().version.to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cache = load_app_update_cache(&state);
    let force = force.unwrap_or(false);

    if !force {
        if let (Some(version), Some(checked_at)) = (&cache.latest_version, cache.checked_at) {
            let fresh = now.saturating_sub(checked_at) < APP_UPDATE_TTL_SECS;
            let backoff_hold = now < cache.next_try_at;
            if fresh || backoff_hold {
                return Ok(app_update_info(&current, version, Some(checked_at), true));
            }
        }
    }

    let proxy_url = current_download_proxy(&state)?;
    let fetched = match fetch_latest_app_tag_via_redirect(proxy_url.as_deref()).await {
        Ok(tag) => Ok(tag),
        Err(redirect_err) => fetch_latest_app_tag(proxy_url.as_deref())
            .await
            .map_err(|api_err| AppError::Core(format!("{redirect_err}; api fallback: {api_err}"))),
    };

    match fetched {
        Ok(version) => {
            store_app_update_cache(
                &state,
                &AppUpdateCache {
                    latest_version: Some(version.clone()),
                    checked_at: Some(now),
                    next_try_at: 0,
                },
            );
            Ok(app_update_info(&current, &version, Some(now), false))
        }
        Err(error) => {
            // Network failed: serve the stale cache when we have one and push
            // the next auto attempt out rather than retrying on every open.
            if let (Some(version), Some(checked_at)) = (&cache.latest_version, cache.checked_at) {
                store_app_update_cache(
                    &state,
                    &AppUpdateCache {
                        latest_version: Some(version.clone()),
                        checked_at: Some(checked_at),
                        next_try_at: now + APP_UPDATE_FAILURE_BACKOFF_SECS,
                    },
                );
                Ok(app_update_info(&current, version, Some(checked_at), true))
            } else {
                Err(error.to_string())
            }
        }
    }
}

fn app_update_info(
    current: &str,
    latest: &str,
    checked_at: Option<u64>,
    cached: bool,
) -> AppUpdateInfo {
    AppUpdateInfo {
        current_version: current.to_string(),
        // Tags are normalized with a `v` prefix internally; strip it so the
        // latest reads like the package version next to it (1.0.9, not v1.0.9).
        latest_version: latest.trim_start_matches('v').to_string(),
        update_available: is_newer_version(latest, current),
        cached,
        checked_at,
    }
}

#[tauri::command]
pub async fn download_core(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: Option<String>,
    tag: Option<String>,
) -> Result<CoreDownloadResult, String> {
    let kind = parse_kind(kind);
    let proxy_url = current_download_proxy(&state)?;
    let progress_app = app.clone();
    let result =
        download_latest_core_with_progress(kind, &state.app_data_dir, tag, proxy_url.clone(), move |progress| {
            let _ = progress_app.emit(CORE_DOWNLOAD_EVENT, progress);
        })
        .await
        .map_err(|e| e.to_string())?;

    // The binary is installed — eagerly fetch its runtime assets (geodata,
    // wintun) through the same download proxy, so the first start doesn't
    // discover-and-download them while holding the store/runtime locks
    // (AGENTS.md §9.22). Failures are warnings; startup ensure_* still run
    // as the fallback.
    let _ = app.emit(
        CORE_DOWNLOAD_EVENT,
        crate::core::CoreDownloadProgress {
            kind: kind.as_str().into(),
            stage: "assets",
            downloaded: 0,
            total: None,
            percent: None,
            via_proxy: proxy_url.is_some(),
        },
    );
    let data_dir = state.app_data_dir.clone();
    let resource_dir = app.path().resource_dir().ok();
    let prefetch_proxy = proxy_url.clone();
    let warnings = tauri::async_runtime::spawn_blocking(move || {
        crate::core::prefetch_runtime_assets(
            kind,
            &data_dir,
            resource_dir.as_deref(),
            prefetch_proxy.as_deref(),
        )
    })
    .await
    .unwrap_or_default();
    for warning in &warnings {
        crate::app_log::warn("core_assets", warning.clone());
    }

    Ok(result)
}

#[tauri::command]
pub async fn fetch_core_latest(
    state: State<'_, AppState>,
    kind: Option<String>,
) -> Result<crate::core::LatestReleaseInfo, String> {
    let kind = parse_kind(kind);
    let proxy_url = current_download_proxy(&state)?;
    fetch_latest_release_with_proxy(kind, proxy_url.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// Switch the active core (singbox | xray). Restarts a running core so the
/// new core takes over the mixed port immediately.
#[tauri::command]
pub async fn set_core_type(
    app: AppHandle,
    kind: String,
) -> Result<crate::domain::AppSettings, String> {
    let parsed = CoreKind::parse(&kind);
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = worker_app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        let running = state.is_core_running();
        let settings = state
            .with_store_mut(|store| {
                store.settings.core_type = parsed.as_str().to_string();
                // Multi-core mode hangs off the sing-box config: switching to
                // another core auto-disables it (the running sidecar stops
                // with the restart below). Protocol pins are kept, so
                // switching back only needs the switch re-flipped.
                store.settings.enforce_multi_core_scope();
                Ok(store.settings.clone())
            })
            .map_err(|e| e.to_string())?;
        if running {
            crate::rule_apply::request_restart(worker_app.clone(), Vec::new());
        }
        Ok(settings)
    })
    .await
    .map_err(|e| format!("core type switch task: {e}"))?
}

#[derive(Debug, Serialize)]
pub struct GeodataFileInfo {
    pub present: bool,
    pub bytes: u64,
    pub modified_at: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct GeodataInfo {
    pub geosite: GeodataFileInfo,
    pub geoip: GeodataFileInfo,
}

/// Geodata state for the kernel-mode Rules card. `kind` selects the pair:
/// `xray` (default — Loyalsoldier .dat in `bin/`) or `mihomo` (MetaCubeX
/// Country.mmdb + mrs geosite.dat in the mihomo home dir). `force` re-downloads
/// first (routed via the running proxy when the core is up — same policy as
/// core downloads).
#[tauri::command]
pub async fn refresh_geodata(
    state: State<'_, AppState>,
    force: Option<bool>,
    kind: Option<String>,
) -> Result<GeodataInfo, String> {
    let force = force.unwrap_or(false);
    let kind = crate::core::CoreKind::parse(kind.as_deref().unwrap_or("xray"));
    let proxy_url = if force {
        current_download_proxy(&state)?
    } else {
        None
    };
    let app_data_dir = state.app_data_dir.clone();
    if force {
        match kind {
            crate::core::CoreKind::Mihomo => {
                tauri::async_runtime::spawn_blocking(move || {
                    crate::core::download_missing_mihomo_geodata(
                        &app_data_dir,
                        proxy_url.as_deref(),
                        true,
                    )
                })
                .await
                .map_err(|e| format!("geodata task: {e}"))?
                .map_err(|e| e.to_string())?;
            }
            _ => {
                tauri::async_runtime::spawn_blocking(move || {
                    crate::core::download_missing_geodata(&app_data_dir, proxy_url.as_deref(), true)
                })
                .await
                .map_err(|e| format!("geodata task: {e}"))?
                .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(match kind {
        crate::core::CoreKind::Mihomo => mihomo_geodata_info(&state.app_data_dir),
        _ => geodata_info(&state.app_data_dir),
    })
}

fn geodata_info(app_data_dir: &std::path::Path) -> GeodataInfo {
    let states = crate::core::geodata_state(app_data_dir);
    let find = |name: &str| {
        states
            .iter()
            .find(|(file, _)| *file == name)
            .map(|(_, s)| GeodataFileInfo {
                present: s.present,
                bytes: s.bytes,
                modified_at: s.modified_at,
            })
            .unwrap_or(GeodataFileInfo {
                present: false,
                bytes: 0,
                modified_at: None,
            })
    };
    GeodataInfo {
        geosite: find("geosite.dat"),
        geoip: find("geoip.dat"),
    }
}

/// Same card shape for the mihomo pair (geosite.dat = .mrs, geoip =
/// Country.mmdb — file states are keyed by their on-disk names).
fn mihomo_geodata_info(app_data_dir: &std::path::Path) -> GeodataInfo {
    let states = crate::core::mihomo_geodata_state(app_data_dir);
    let find = |name: &str| {
        states
            .iter()
            .find(|(file, _)| *file == name)
            .map(|(_, s)| GeodataFileInfo {
                present: s.present,
                bytes: s.bytes,
                modified_at: s.modified_at,
            })
            .unwrap_or(GeodataFileInfo {
                present: false,
                bytes: 0,
                modified_at: None,
            })
    };
    GeodataInfo {
        geosite: find("geosite.dat"),
        geoip: find("Country.mmdb"),
    }
}

/// Absolute path of the running executable — the app's own install location,
/// shown on the version tab next to the kernel binary path.
#[tauri::command(async)]
pub fn get_app_install_path() -> Result<String, String> {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())
}

fn current_download_proxy(state: &AppState) -> Result<Option<String>, String> {
    if !state.is_core_running() {
        return Ok(None);
    }
    let mixed_port = state
        .with_store(|store| Ok(store.settings.mixed_port))
        .map_err(|error| error.to_string())?;
    Ok(Some(format!("http://127.0.0.1:{mixed_port}")))
}

fn normalize_cmp(v: &str) -> String {
    v.trim().trim_start_matches('v').to_string()
}

/// Numeric semver-ish comparison: true only if `latest` is strictly newer
/// than `local` (not merely different) — e.g. a bundled core ahead of the
/// latest published release should not be flagged as "update available".
fn is_newer_version(latest: &str, local: &str) -> bool {
    parse_version(latest) > parse_version(local)
}

fn parse_version(v: &str) -> Vec<u32> {
    normalize_cmp(v)
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .collect()
}

/// The machine's LAN IPv4 (of the default-route interface), for the
/// dashboard's listen card. The UDP "connect" trick only makes the OS pick
/// a route — no packet is sent — so it works offline as long as an
/// interface with a default route exists. `None` when there is no such
/// address (e.g. fully offline).
#[tauri::command]
pub fn get_lan_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v4) if !v4.is_loopback() => Some(v4.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::is_newer_version;

    #[test]
    fn bundled_ahead_of_latest_release_is_not_an_update() {
        // Regression: a bundled core (v1.13.18) can ship ahead of the latest
        // published GitHub release (v1.13.15) if releases lag the bundle.
        // String-diff comparison used to flag this as "update available"
        // even though downgrading would be wrong.
        assert!(!is_newer_version("v1.13.15", "v1.13.18"));
    }

    #[test]
    fn strictly_newer_release_is_an_update() {
        assert!(is_newer_version("v1.14.0", "v1.13.18"));
    }

    #[test]
    fn identical_versions_are_not_an_update() {
        assert!(!is_newer_version("v1.13.18", "v1.13.18"));
    }

    #[test]
    fn differing_segment_counts_compare_numerically() {
        assert!(is_newer_version("v1.13.2", "v1.13"));
        assert!(!is_newer_version("v1.13", "v1.13.2"));
    }
}
