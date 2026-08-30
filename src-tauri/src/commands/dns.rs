//! DNS settings commands (docs/dns.md).

use crate::config::dump_dns_rules_file;
use crate::domain::{read_system_hosts_entries, DnsSettings, HostsEntry};
use crate::state::AppState;
use tauri::{AppHandle, State};

/// Export the current DNS rules to `{app_data}/data/dns/user-dns-rules.list`.
fn dump_dns_rules(state: &AppState) {
    let rules = state
        .with_store(|s| Ok(s.dns.enabled_dns_rules()))
        .unwrap_or_default();
    if let Err(e) = dump_dns_rules_file(&state.app_data_dir, &rules) {
        eprintln!("[satelite] dump dns rules: {e}");
    }
}

#[tauri::command(async)]
pub fn get_dns_settings(state: State<'_, AppState>) -> Result<DnsSettings, String> {
    state
        .with_store(|store| Ok(store.dns.clone()))
        .map_err(|e| e.to_string())
}

/// Read the OS hosts file into a read-only entry list (for the Hosts UI).
#[tauri::command]
pub async fn read_system_hosts() -> Result<Vec<HostsEntry>, String> {
    tauri::async_runtime::spawn_blocking(read_system_hosts_entries)
        .await
        .map_err(|error| format!("read system hosts task: {error}"))
}

/// Replace full DNS settings. Optionally restart core when `apply` is true and running.
#[tauri::command(async)]
pub fn update_dns_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    mut settings: DnsSettings,
    apply: Option<bool>,
) -> Result<DnsSettings, String> {
    let apply = apply.unwrap_or(true);
    settings.ensure_rule_sets();
    state
        .with_store_mut(|store| {
            store.dns = settings;
            Ok(store.dns.clone())
        })
        .map_err(|e| e.to_string())?;

    let dns = state
        .with_store(|s| Ok(s.dns.clone()))
        .map_err(|e| e.to_string())?;

    // Export user DNS rules to disk (mirror routing rule export).
    dump_dns_rules(&state);

    if apply {
        crate::rule_apply::request_restart(app, Vec::new());
    }

    Ok(dns)
}

/// Reset DNS rules to factory defaults (other fields unchanged).
/// Rules reset reloads `resources/dns/builtin-dns-rules.list`.
#[tauri::command(async)]
pub fn reset_dns_defaults(
    app: AppHandle,
    state: State<'_, AppState>,
    section: String,
    apply: Option<bool>,
) -> Result<DnsSettings, String> {
    let apply = apply.unwrap_or(true);
    let section = section.trim().to_ascii_lowercase();

    let dns = state
        .with_store_mut(|store| {
            match section.as_str() {
                "rules" => {
                    store.dns.reset_builtin_dns_set();
                }
                other => {
                    return Err(crate::error::AppError::Config(format!(
                        "unknown DNS reset section: {other} (use rules)"
                    )));
                }
            }
            Ok(store.dns.clone())
        })
        .map_err(|e| e.to_string())?;

    dump_dns_rules(&state);

    if apply {
        crate::rule_apply::request_restart(app, Vec::new());
    }

    Ok(dns)
}

/// DNS-path diagnosis through the running core (DNS 设置页「诊断」).
///
/// Two layers, see `services::dns_diag`: live `/dns/query` resolution via
/// the core's own DNS pipeline (sing-box/mihomo), plus a local replay of the
/// generated config's decision chain showing which resolver pool each
/// domain takes (local/domestic/remote/block/hosts/fakeip), the upstream
/// server addresses, and what matched.
#[tauri::command]
pub async fn diagnose_dns(
    state: State<'_, AppState>,
    domains: Vec<String>,
) -> Result<crate::services::dns_diag::DnsDiagReport, String> {
    let input = state
        .with_store(|store| {
            Ok(crate::services::dns_diag::DnsDiagInput {
                core_type: store.settings.core_type.clone(),
                runtime_source: store.settings.runtime_source.clone(),
                tun_enabled: store.settings.tun_enabled,
                outbound_mode: store.settings.outbound_mode,
                rule_sets: store.rule_sets.clone(),
                dns: store.dns.clone(),
                data_dir: state.app_data_dir.clone(),
            })
        })
        .map_err(|e| e.to_string())?;
    let running = state.is_core_running();
    let api = state.lock_runtime().clash_api_clone();

    Ok(crate::services::dns_diag::run(input, domains, running, api).await)
}
