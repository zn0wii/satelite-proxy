use crate::runtime::{ConnectionView, LiveConnectionBatch, RequestBatch};
use crate::state::AppState;
use tauri::{AppHandle, Manager, State};

#[tauri::command]
pub async fn list_connections(app: AppHandle) -> Result<Vec<ConnectionView>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        Ok(state.live_connection_views())
    })
    .await
    .map_err(|e| format!("list connections task: {e}"))?
}

#[tauri::command]
pub async fn list_connection_changes(
    app: AppHandle,
    since_revision: Option<u64>,
    last_order_revision: Option<u64>,
) -> Result<LiveConnectionBatch, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        Ok(state.live_connection_batch(since_revision, last_order_revision))
    })
    .await
    .map_err(|e| format!("list connection changes task: {e}"))?
}

#[tauri::command]
pub async fn list_requests(
    app: AppHandle,
    query: Option<String>,
    limit: Option<usize>,
    after_seq: Option<u64>,
) -> Result<RequestBatch, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        Ok(state.request_views(query.as_deref(), limit, false, after_seq))
    })
    .await
    .map_err(|e| format!("list requests task: {e}"))?
}

#[tauri::command]
pub async fn list_request_failures(
    app: AppHandle,
    query: Option<String>,
    limit: Option<usize>,
    after_seq: Option<u64>,
) -> Result<RequestBatch, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        Ok(state.request_views(query.as_deref(), limit, true, after_seq))
    })
    .await
    .map_err(|e| format!("list request failures task: {e}"))?
}

#[tauri::command(async)]
pub fn clear_request_history(state: State<'_, AppState>) -> Result<(), String> {
    state
        .clear_request_history_nonblocking()
        .map_err(|error| error.to_string())
}
