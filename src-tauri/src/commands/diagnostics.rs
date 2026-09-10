//! Network diagnostics surfaced to the UI. Detection only — never mutates
//! system network settings. See `core::macos_net` for the rationale.

use crate::services::exit_ip::{probe, ExitIpInfo};
use crate::state::AppState;
use tauri::State;

#[derive(Debug, Clone, serde::Serialize)]
pub struct NetworkDiagnosticsResult {
    /// Empty when nothing was detected (or the platform isn't supported).
    pub issues: Vec<DiagnosticIssue>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiagnosticIssue {
    pub id: String,
    pub issue: String,
    pub suggestion: String,
}

/// Run best-effort network diagnostics (currently: TUN-bypassing system DNS,
/// macOS only). Never modifies system settings — UI-facing detection only.
#[tauri::command]
pub fn diagnose_network() -> NetworkDiagnosticsResult {
    #[cfg(target_os = "macos")]
    let issues = crate::core::macos_net::diagnose_system_dns_bypass()
        .into_iter()
        .map(|diag| DiagnosticIssue {
            id: "dns-bypasses-tun".into(),
            issue: diag.issue,
            suggestion: diag.suggestion,
        })
        .collect();

    #[cfg(not(target_os = "macos"))]
    let issues = Vec::new();

    NetworkDiagnosticsResult { issues }
}

/// Dashboard network-probe card: exit-IP half of the probe. Races a few
/// public IP APIs through the core's mixed inbound when the core is running
/// (Rule/Global mode) — the answer is the current exit; through Direct mode
/// or with the core stopped it goes direct and reports the machine's own
/// public IP instead. See `services::exit_ip`.
#[tauri::command]
pub async fn check_exit_ip(state: State<'_, AppState>) -> Result<ExitIpInfo, String> {
    let status = state.proxy_status().map_err(|e| e.to_string())?;
    let via_proxy = status.running && status.outbound_mode != "direct";
    probe(status.mixed_port, via_proxy).await
}
