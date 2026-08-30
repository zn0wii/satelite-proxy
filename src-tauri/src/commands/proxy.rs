use crate::runtime::ProxyStatus;
use crate::smart_switch::{self, SmartSwitchNowResult};
use crate::state::AppState;
use tauri::{AppHandle, Manager, State};

#[tauri::command(async)]
pub fn get_proxy_status(app: AppHandle, state: State<'_, AppState>) -> Result<ProxyStatus, String> {
    let status = state.proxy_status().map_err(|e| e.to_string())?;
    AppState::schedule_kernel_selection_sync(app);
    Ok(status)
}

#[tauri::command]
pub async fn start_proxy(
    app: AppHandle,
    enable_system_proxy: Option<bool>,
) -> Result<ProxyStatus, String> {
    let resource_dir = app.path().resource_dir().ok();
    let worker_app = app.clone();
    // Default: start core only; system proxy controlled by independent switch.
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = worker_app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        state
            .start_proxy(
                resource_dir.as_deref(),
                enable_system_proxy.unwrap_or(false),
            )
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("start proxy task: {e}"))?;
    crate::tray::refresh_icon(&app);
    result
}

#[tauri::command]
pub async fn set_system_proxy(app: AppHandle, enabled: bool) -> Result<ProxyStatus, String> {
    let resource_dir = app.path().resource_dir().ok();
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = worker_app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        state
            .set_system_proxy(enabled, resource_dir.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("system proxy task: {e}"))?
}

/// Enable/disable TUN. Persists setting; restarts core if currently running so config applies.
#[tauri::command]
pub async fn set_tun_enabled(app: AppHandle, enabled: bool) -> Result<ProxyStatus, String> {
    let resource_dir = app.path().resource_dir().ok();
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = worker_app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        state
            .set_tun_enabled(enabled, resource_dir.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("TUN task: {e}"))?
}

/// Traffic capture: `off` | `system` | `tun` (mutually exclusive).
///
/// Async + spawn_blocking so the webview IPC is not stuck on a sync command
/// while TUN restart / service stop runs for seconds.
#[tauri::command]
pub async fn set_capture_mode(app: AppHandle, mode: String) -> Result<ProxyStatus, String> {
    let resource_dir = app.path().resource_dir().ok();
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        state
            .set_capture_mode(&mode, resource_dir.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("capture mode task: {e}"))?
}

/// Switch outbound mode: rule | global | direct. Restarts core when running.
#[tauri::command]
pub async fn set_outbound_mode(app: AppHandle, mode: String) -> Result<ProxyStatus, String> {
    let mode = crate::domain::OutboundMode::parse(&mode)
        .ok_or_else(|| "mode must be rule | global | direct".to_string())?;
    let resource_dir = app.path().resource_dir().ok();
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = worker_app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        state
            .set_outbound_mode(mode, resource_dir.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("outbound mode task: {e}"))?
}

#[tauri::command]
pub async fn stop_proxy(app: AppHandle) -> Result<ProxyStatus, String> {
    let worker_app = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = worker_app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        state.stop_proxy().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("stop proxy task: {e}"))?;
    crate::tray::refresh_icon(&app);
    result
}

#[tauri::command]
pub async fn restart_proxy(app: AppHandle) -> Result<ProxyStatus, String> {
    let resource_dir = app.path().resource_dir().ok();
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = worker_app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        state
            .restart_proxy(resource_dir.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("restart proxy task: {e}"))?
}

/// Enable-time bootstrap: probe candidates and switch to best node once.
#[tauri::command]
pub async fn smart_switch_now(state: State<'_, AppState>) -> Result<SmartSwitchNowResult, String> {
    smart_switch::select_best_now(&state).await
}

/// Set current node: persist + hot-switch via clash_api when running.
#[tauri::command]
pub async fn set_current_node_live(app: AppHandle, node_id: String) -> Result<ProxyStatus, String> {
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = worker_app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        let (_, was_kernel, _) = state
            .select_current_node_serialized(&node_id, true, true)
            .map_err(|e| e.to_string())?;
        if was_kernel {
            crate::rule_apply::request_restart(worker_app.clone(), Vec::new());
        }
        state.proxy_status().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("live node selection task: {e}"))?
}
