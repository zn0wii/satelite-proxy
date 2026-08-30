use crate::services::latency::{ping_nodes, probe_nodes, LatencyResult};
use crate::state::AppState;
use serde::Serialize;
use tauri::State;

use super::config::custom_config_nodes;

#[derive(Debug, Serialize)]
pub struct LatencyBatchResult {
    pub results: Vec<LatencyResult>,
    pub tested: usize,
    pub ok: usize,
    pub failed: usize,
    /// `clash_api` or `tcp`
    pub method: String,
}

/// Test latency for each node: direct TCP connect for TCP-based protocols,
/// clash delay API for UDP-only protocols (hysteria/hysteria2/tuic) when the
/// core is running — a plain TCP connect to those ports always times out
/// regardless of node health, so it can't report raw reachability for them.
#[tauri::command]
pub async fn test_nodes_latency(
    state: State<'_, AppState>,
    ids: Option<Vec<String>>,
    timeout_ms: Option<u64>,
) -> Result<LatencyBatchResult, String> {
    let nodes = state
        .with_store(|store| {
            let all = store.enabled_nodes();
            let filtered = if let Some(ids) = &ids {
                let set: std::collections::HashSet<_> = ids.iter().cloned().collect();
                all.into_iter().filter(|n| set.contains(&n.id)).collect()
            } else {
                all
            };
            Ok(filtered)
        })
        .map_err(|e| e.to_string())?;

    if nodes.is_empty() {
        return Ok(LatencyBatchResult {
            results: vec![],
            tested: 0,
            ok: 0,
            failed: 0,
            method: "none".into(),
        });
    }

    let probe_url = state.lock_store().settings.probe_url.clone();
    let clash = state.lock_runtime().clash_api_clone();

    let results = probe_nodes(&nodes, timeout_ms, Some(30), clash, probe_url)
        .await
        .map_err(|e| e.to_string())?;

    state
        .with_store_mut(|store| {
            for r in &results {
                if r.id.is_empty() {
                    continue;
                }
                store.update_node_latency(&r.id, r.latency_ms, r.tested_at);
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;

    let ok = results.iter().filter(|r| r.latency_ms.is_some()).count();
    let failed = results.len() - ok;
    let method = batch_method(&results);
    Ok(LatencyBatchResult {
        tested: results.len(),
        ok,
        failed,
        results,
        method,
    })
}

/// Fast "Ping 测试": direct TCP connect for every node, 30 concurrent —
/// deliberately bypasses the kernel even when the core is running (the
/// through-kernel path lives in [`test_nodes_latency`]). Reports raw
/// reachability only; QUIC-only protocols have no TCP port to ping and come
/// back as `unsupported`.
#[tauri::command]
pub async fn ping_nodes_latency(
    state: State<'_, AppState>,
    ids: Option<Vec<String>>,
    timeout_ms: Option<u64>,
) -> Result<LatencyBatchResult, String> {
    let nodes = state
        .with_store(|store| {
            let all = store.enabled_nodes();
            let filtered = if let Some(ids) = &ids {
                let set: std::collections::HashSet<_> = ids.iter().cloned().collect();
                all.into_iter().filter(|n| set.contains(&n.id)).collect()
            } else {
                all
            };
            Ok(filtered)
        })
        .map_err(|e| e.to_string())?;

    if nodes.is_empty() {
        return Ok(LatencyBatchResult {
            results: vec![],
            tested: 0,
            ok: 0,
            failed: 0,
            method: "none".into(),
        });
    }

    let results = ping_nodes(&nodes, timeout_ms, Some(30))
        .await
        .map_err(|e| e.to_string())?;

    state
        .with_store_mut(|store| {
            for r in &results {
                if r.id.is_empty() {
                    continue;
                }
                store.update_node_latency(&r.id, r.latency_ms, r.tested_at);
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;

    let ok = results.iter().filter(|r| r.latency_ms.is_some()).count();
    let failed = results.len() - ok;
    let method = batch_method(&results);
    Ok(LatencyBatchResult {
        tested: results.len(),
        ok,
        failed,
        results,
        method,
    })
}

/// `tcp` / `clash_api` if every result used that method, `mixed` otherwise
/// (UDP-only protocols use clash_api, the rest use tcp — see [`test_nodes_latency`]).
fn batch_method(results: &[LatencyResult]) -> String {
    let mut methods = results.iter().map(|r| r.method.as_str());
    match methods.next() {
        None => "none".into(),
        Some(first) if methods.all(|m| m == first) => first.into(),
        _ => "mixed".into(),
    }
}

/// Same direct-TCP probe as [`test_nodes_latency`], but for the read-only
/// nodes extracted from the selected custom sing-box config. Results are
/// NOT persisted — custom nodes do not live in the node store, so the UI
/// keeps them for the session only. Empty when not in custom runtime mode.
#[tauri::command]
pub async fn test_custom_nodes_latency(
    state: State<'_, AppState>,
    timeout_ms: Option<u64>,
) -> Result<LatencyBatchResult, String> {
    let nodes: Vec<_> = custom_config_nodes(&state)?
        .into_iter()
        .map(|listed| listed.node)
        .collect();

    if nodes.is_empty() {
        return Ok(LatencyBatchResult {
            results: vec![],
            tested: 0,
            ok: 0,
            failed: 0,
            method: "none".into(),
        });
    }

    // Always TCP — same rationale as test_nodes_latency.
    let results = probe_nodes(&nodes, timeout_ms, Some(30), None, String::new())
        .await
        .map_err(|e| e.to_string())?;

    let ok = results.iter().filter(|r| r.latency_ms.is_some()).count();
    let failed = results.len() - ok;
    Ok(LatencyBatchResult {
        tested: results.len(),
        ok,
        failed,
        results,
        method: "tcp".into(),
    })
}
