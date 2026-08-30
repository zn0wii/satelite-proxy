//! Tauri commands for the Proxy Chain feature: named node pools and the
//! multi-hop chains built from them (see `config::builder`'s
//! `build_pool_selectors` / `build_chain_outbounds`).

use crate::domain::{ChainHop, NodePool, PoolMode, ProxyChain};
use crate::state::AppState;
use tauri::{AppHandle, State};

/// Queue one globally debounced restart — mirrors `rules.rs::apply_running`.
/// Pool/chain edits change the generated outbounds the same way rule edits
/// do, so they need the same debounced-restart contract.
fn apply_running(app: &AppHandle) {
    crate::rule_apply::request_restart(app.clone(), Vec::new());
}

#[tauri::command(async)]
pub fn list_pools(state: State<'_, AppState>) -> Result<Vec<NodePool>, String> {
    state
        .with_store(|store| Ok(store.pools.clone()))
        .map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn create_pool(
    state: State<'_, AppState>,
    name: String,
    mode: PoolMode,
) -> Result<NodePool, String> {
    state
        .with_store_mut(|store| store.create_pool(&name, mode))
        .map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn update_pool(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    name: String,
    mode: PoolMode,
) -> Result<NodePool, String> {
    let pool = state
        .with_store_mut(|store| store.update_pool(&id, &name, mode))
        .map_err(|e| e.to_string())?;
    // A pool's membership feeds every chain/rule that references it —
    // always worth a restart, same as editing a rule set's keyword filter.
    apply_running(&app);
    Ok(pool)
}

#[tauri::command(async)]
pub fn delete_pool(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    state
        .with_store_mut(|store| store.delete_pool(&id))
        .map_err(|e| e.to_string())?;
    apply_running(&app);
    Ok(())
}

#[tauri::command(async)]
pub fn list_chains(state: State<'_, AppState>) -> Result<Vec<ProxyChain>, String> {
    state
        .with_store(|store| Ok(store.chains.clone()))
        .map_err(|e| e.to_string())
}

/// Rule-set references per chain id (set-level pin or any single rule,
/// deduped per set) — mirrors `delete_chain`'s guard so the list page can
/// show "used by N rule sets" before a delete is attempted.
#[tauri::command(async)]
pub fn list_chain_usage(
    state: State<'_, AppState>,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    state
        .with_store(|store| Ok(store.chain_rule_usage()))
        .map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn create_chain(
    state: State<'_, AppState>,
    name: String,
    hops: Vec<ChainHop>,
) -> Result<ProxyChain, String> {
    state
        .with_store_mut(|store| store.create_chain(&name, hops))
        .map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn update_chain(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    name: String,
    hops: Vec<ChainHop>,
) -> Result<ProxyChain, String> {
    let chain = state
        .with_store_mut(|store| store.update_chain(&id, &name, hops))
        .map_err(|e| e.to_string())?;
    apply_running(&app);
    Ok(chain)
}

#[tauri::command(async)]
pub fn delete_chain(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    state
        .with_store_mut(|store| store.delete_chain(&id))
        .map_err(|e| e.to_string())?;
    apply_running(&app);
    Ok(())
}

/// Probe every hop of one chain through the live Clash delay API — solo
/// latency per hop plus chain-prefix latency (the last hop's prefix is the
/// whole chain). Localizes which hop breaks a multi-hop chain; requires the
/// sing-box core to be running (chain-local outbound tags only exist there).
/// Also performs real-world exit verification: an ip.sb round-trip through
/// the whole chain, plus the actual exit IP via the app's loopback
/// diagnostics inbound (no user rule needed).
#[tauri::command]
pub async fn diagnose_chain(
    state: State<'_, AppState>,
    chain_id: String,
) -> Result<crate::services::chain_diag::ChainDiagnosis, String> {
    let (chain, pools, nodes, core_kind, probe_url, locale) = state
        .with_store(|store| {
            Ok((
                store.chains.iter().find(|c| c.id == chain_id).cloned(),
                store.pools.clone(),
                store.enabled_nodes(),
                crate::core::CoreKind::parse(&store.settings.core_type),
                store.settings.probe_url.clone(),
                store.settings.locale.clone(),
            ))
        })
        .map_err(|e| e.to_string())?;
    let chain = chain.ok_or_else(|| "链路不存在".to_string())?;
    if core_kind != crate::core::CoreKind::SingBox {
        return Err("链路诊断仅在 sing-box 内核下可用（其余内核不支持代理链）".into());
    }
    let api = state
        .lock_runtime()
        .clash_api_clone()
        .ok_or_else(|| "内核未运行，请先启动代理后再诊断".to_string())?;
    // Belt-and-braces outer cap: the inner probes are individually bounded,
    // but this guarantees the invoke always settles even if something
    // unexpected stalls the whole future.
    match tokio::time::timeout(
        std::time::Duration::from_secs(35),
        crate::services::chain_diag::diagnose(api, chain, pools, nodes, probe_url, 6000, locale),
    )
    .await
    {
        Ok(result) => Ok(result),
        Err(_) => Err("诊断超时：部分探测长时间无响应，请检查内核与节点状态".into()),
    }
}
