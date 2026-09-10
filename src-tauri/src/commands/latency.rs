use crate::services::latency::{ping_nodes_streaming, probe_nodes_streaming, LatencyResult};
use crate::state::AppState;
use serde::Serialize;
use tauri::ipc::Channel;
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

/// Forward one finished probe to the frontend over the invoke's IPC channel
/// so the UI can update that node immediately. A dead channel (webview
/// navigated away mid-run) is not an error — the batch return still carries
/// every result.
fn stream_result(channel: &Channel<LatencyResult>, r: &LatencyResult) {
    let _ = channel.send(r.clone());
}

/// Load the probe set preserving the caller's id order: the UI sends ids in
/// current display order, so probes launch — and stream back — top to
/// bottom instead of in store insertion order.
fn load_nodes_in_display_order(
    state: &State<'_, AppState>,
    ids: &Option<Vec<String>>,
) -> Result<Vec<crate::domain::ProxyNode>, String> {
    state
        .with_store(|store| {
            let all = store.enabled_nodes();
            Ok(match ids {
                Some(ids) => {
                    let by_id: std::collections::HashMap<&str, &crate::domain::ProxyNode> =
                        all.iter().map(|n| (n.id.as_str(), n)).collect();
                    ids.iter()
                        .filter_map(|id| by_id.get(id.as_str()).map(|n| (*n).clone()))
                        .collect()
                }
                None => all,
            })
        })
        .map_err(|e| e.to_string())
}

/// Test latency for each node: direct TCP connect for TCP-based protocols,
/// clash delay API for UDP-only protocols (hysteria/hysteria2/tuic) when the
/// core is running — a plain TCP connect to those ports always times out
/// regardless of node health, so it can't report raw reachability for them.
///
/// Each finished probe is also pushed through `on_result` (Tauri channel) as
/// soon as it completes — per-node streaming UI updates instead of one big
/// batch at the end.
#[tauri::command]
pub async fn test_nodes_latency(
    state: State<'_, AppState>,
    ids: Option<Vec<String>>,
    timeout_ms: Option<u64>,
    on_result: Channel<LatencyResult>,
) -> Result<LatencyBatchResult, String> {
    let nodes = load_nodes_in_display_order(&state, &ids)?;

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

    // Manual run: never serve cached numbers — every node is really probed.
    // The fresh results are still written to the shared probe cache for
    // background consumers (smart_switch ranking/health).
    let results = probe_nodes_streaming(
        &nodes,
        timeout_ms,
        Some(30),
        clash,
        probe_url,
        |r| stream_result(&on_result, r),
        false,
    )
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
/// back as `unsupported`. Streams per-node results via `on_result`.
#[tauri::command]
pub async fn ping_nodes_latency(
    state: State<'_, AppState>,
    ids: Option<Vec<String>>,
    timeout_ms: Option<u64>,
    on_result: Channel<LatencyResult>,
) -> Result<LatencyBatchResult, String> {
    let nodes = load_nodes_in_display_order(&state, &ids)?;

    if nodes.is_empty() {
        return Ok(LatencyBatchResult {
            results: vec![],
            tested: 0,
            ok: 0,
            failed: 0,
            method: "none".into(),
        });
    }

    // Manual run — bypass cache reads, write fresh results (see test_nodes_latency).
    let results = ping_nodes_streaming(
        &nodes,
        timeout_ms,
        Some(30),
        |r| stream_result(&on_result, r),
        false,
    )
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
    on_result: Channel<LatencyResult>,
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

    // Always TCP — same rationale as test_nodes_latency. Manual run —
    // bypass cache reads, write fresh results.
    let results = probe_nodes_streaming(
        &nodes,
        timeout_ms,
        Some(30),
        None,
        String::new(),
        |r| stream_result(&on_result, r),
        false,
    )
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
