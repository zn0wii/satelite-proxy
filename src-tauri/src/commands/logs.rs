use crate::app_log::{self, LogBatch, LogLevel};
use crate::core::CoreKind;
use crate::state::AppState;
use serde::Serialize;
use tauri::State;

#[tauri::command]
pub async fn list_app_logs(
    min_level: Option<String>,
    limit: Option<usize>,
    query: Option<String>,
    after_id: Option<u64>,
) -> Result<LogBatch, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let level = min_level
            .as_deref()
            .and_then(LogLevel::parse)
            .unwrap_or(LogLevel::Info);
        let limit = limit.unwrap_or(500).clamp(1, 2_000);
        Ok(app_log::list(level, limit, query.as_deref(), after_id))
    })
    .await
    .map_err(|e| format!("list logs task: {e}"))?
}

#[tauri::command(async)]
pub fn clear_app_logs() -> Result<(), String> {
    app_log::clear();
    Ok(())
}

/// Frontend crash/error sink: render exceptions (ErrorBoundary catches),
/// uncaught errors and unhandled rejections land in the app log under the
/// `webview` target, so a white-screen-class UI failure leaves a diagnosis
/// trail (Logs page → 应用日志) instead of dying silently.
#[tauri::command(async)]
pub fn log_frontend_event(level: Option<String>, message: String) -> Result<(), String> {
    let level = level
        .as_deref()
        .and_then(LogLevel::parse)
        .unwrap_or(LogLevel::Error);
    // Frontend strings can be huge (stack traces, component trees) — clamp
    // so one bad crash can't bloat the log file or the in-memory ring.
    let message = message.chars().take(4000).collect::<String>();
    app_log::push(level, "webview", message);
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct CoreLogTail {
    /// Absolute path of the log file (current or last core session).
    pub path: Option<String>,
    pub lines: Vec<String>,
}

/// Tail of a core's hourly log file (Logs page kernel-log view). Xray has
/// no per-connection API, so its raw log carries accepted connections and
/// routing decisions — read at `info` level. `kind` selects which core's
/// file to read (sing-box | xray | mihomo) — under multi-core mode the
/// sidecar writes its own file separate from the main core's; `None` keeps
/// the historical behavior of tailing the main core.
#[tauri::command(async)]
pub fn get_core_log_tail(
    state: State<'_, AppState>,
    limit: Option<usize>,
    kind: Option<String>,
) -> Result<CoreLogTail, String> {
    let limit = limit.unwrap_or(300).clamp(1, 1_000);
    let runtime = state.lock_runtime();
    let tail = match kind.as_deref() {
        Some("singbox") => runtime.core_log_tail_for(CoreKind::SingBox, limit),
        Some("xray") => runtime.core_log_tail_for(CoreKind::Xray, limit),
        Some("mihomo") => runtime.core_log_tail_for(CoreKind::Mihomo, limit),
        // Legacy / default: whatever the main manager last ran.
        _ => runtime.core.core_log_tail(limit),
    };
    drop(runtime);
    Ok(match tail {
        Some((path, lines)) => CoreLogTail {
            path: Some(path.display().to_string()),
            lines,
        },
        None => CoreLogTail {
            path: None,
            lines: Vec::new(),
        },
    })
}

/// Truncate the current-hour log file of the given core (same manager
/// resolution as `get_core_log_tail`). Only the file this app instance
/// writes is cleared — previous hours' rotated files stay for retention.
#[tauri::command(async)]
pub fn clear_core_log(state: State<'_, AppState>, kind: String) -> Result<(), String> {
    let parsed = match kind.as_str() {
        "singbox" => CoreKind::SingBox,
        "xray" => CoreKind::Xray,
        "mihomo" => CoreKind::Mihomo,
        _ => return Err(format!("unknown core kind: {kind}")),
    };
    state
        .lock_runtime()
        .core_log_clear_for(parsed)
        .map_err(|e| e.to_string())
}
