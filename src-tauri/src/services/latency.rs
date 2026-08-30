//! Latency probe helpers.
//!
//! - **UI 测速** (`test_nodes_latency`): through-kernel clash delay API for
//!   every protocol when the core is running — a TCP-connect probe measures
//!   raw reachability, which lies about proxy health (e.g. mihomo/other-core
//!   nodes pass TCP handshakes with flying colors and die mid-proxy). Direct
//!   TCP is the fallback when the core is stopped or the caller has no
//!   mapping into the running config (custom sing-box profiles). UDP-only
//!   protocols (hysteria/hysteria2/tuic) have no TCP fallback at all —
//!   without the core they report an explicit "start the proxy" error.
//! - **Smart switch**: ranks candidates with [`probe_nodes_ranked`] — TCP
//!   ping for TCP-capable nodes (a better speed correlate in practice), the
//!   through-kernel URL probe only for QUIC-only protocols and for the
//!   current node's health confirmation.
//!
//! Clash path uses **unified delay** (like mihomo / FlClash): probe twice and
//! report the second RTT so handshake / cold-connect bias is reduced.

use crate::api::ClashApi;
use crate::config::outbound_tag;
use crate::domain::ProxyNode;
use crate::error::AppResult;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio::time::timeout;

const DEFAULT_TIMEOUT_MS: u64 = 5000;
const DEFAULT_CONCURRENCY: usize = 30;
const GLOBAL_CONCURRENCY: usize = 30;
const CACHE_TTL: Duration = Duration::from_secs(90);
const FAILURE_CACHE_TTL: Duration = Duration::from_secs(15);
const MAX_CACHE_ENTRIES: usize = 4096;
const CACHE_TRIM_TO: usize = 3072;

static GLOBAL_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(GLOBAL_CONCURRENCY)));
struct ProbeCache {
    entries: HashMap<String, (Instant, LatencyResult)>,
    last_prune: Instant,
}

static PROBE_CACHE: LazyLock<Mutex<ProbeCache>> = LazyLock::new(|| {
    Mutex::new(ProbeCache {
        entries: HashMap::new(),
        last_prune: Instant::now(),
    })
});
static PROBE_LOCKS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Serialize)]
pub struct LatencyResult {
    pub id: String,
    pub name: String,
    /// None means timeout / unreachable
    pub latency_ms: Option<u32>,
    pub error: Option<String>,
    pub tested_at: i64,
    /// `clash_api` | `tcp`
    pub method: String,
}

pub async fn probe_nodes(
    nodes: &[ProxyNode],
    timeout_ms: Option<u64>,
    concurrency: Option<usize>,
    clash: Option<ClashApi>,
    probe_url: String,
) -> AppResult<Vec<LatencyResult>> {
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    let concurrency = concurrency.unwrap_or(DEFAULT_CONCURRENCY).max(1);
    let mut pending = nodes.iter().cloned().enumerate();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..concurrency.min(nodes.len()) {
        if let Some((index, node)) = pending.next() {
            spawn_probe_task(
                &mut tasks,
                index,
                node,
                timeout_ms,
                clash.clone(),
                probe_url.clone(),
            );
        }
    }

    let mut indexed_results = Vec::with_capacity(nodes.len());
    let mut task_errors = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(result) => indexed_results.push(result),
            Err(e) => task_errors.push(LatencyResult {
                id: String::new(),
                name: String::new(),
                latency_ms: None,
                error: Some(format!("join error: {e}")),
                tested_at: now_secs(),
                method: "error".into(),
            }),
        }
        if let Some((index, node)) = pending.next() {
            spawn_probe_task(
                &mut tasks,
                index,
                node,
                timeout_ms,
                clash.clone(),
                probe_url.clone(),
            );
        }
    }
    indexed_results.sort_unstable_by_key(|(index, _)| *index);
    let mut results: Vec<_> = indexed_results
        .into_iter()
        .map(|(_, result)| result)
        .collect();
    results.append(&mut task_errors);
    Ok(results)
}

/// Pure-TCP fast ping for the Nodes page "Ping 测试" button: direct TCP
/// reachability, never routed through the kernel even when the core is
/// running — that's the point (the through-kernel path is accurate but
/// slow). Reuses the probe pool (caller-set concurrency, per-key coalescing
/// and the shared `tcp|` cache). QUIC-only protocols have no TCP port to
/// ping at all, so their `unsupported` note is rewritten from the
/// "core not running" wording into a ping-appropriate one.
pub async fn ping_nodes(
    nodes: &[ProxyNode],
    timeout_ms: Option<u64>,
    concurrency: Option<usize>,
) -> AppResult<Vec<LatencyResult>> {
    let mut results = probe_nodes(nodes, timeout_ms, concurrency, None, String::new()).await?;
    for r in &mut results {
        if r.method == "unsupported" {
            r.error =
                Some("QUIC-only protocol: no TCP port to ping — use the real-latency test with the core running".into());
        }
    }
    Ok(results)
}

/// Ranking probe for smart switch: TCP ping for every TCP-capable node —
/// empirically a better speed correlate than through-kernel URL probes,
/// whose numbers are dominated by probe-server and TLS variance — and the
/// kernel URL probe only for QUIC-only protocols, which have no TCP port
/// to ping. Keeps candidate ranking and the current-node comparison
/// like-for-like (see smart_switch). `clash` serves the QUIC fallback;
/// without it those nodes come back `unsupported`. Results keep the
/// caller's node order.
pub async fn probe_nodes_ranked(
    nodes: &[ProxyNode],
    timeout_ms: u64,
    concurrency: usize,
    clash: Option<ClashApi>,
    probe_url: &str,
) -> AppResult<Vec<LatencyResult>> {
    if nodes.is_empty() {
        return Ok(vec![]);
    }
    let mut tcp_nodes = Vec::new();
    let mut udp_nodes = Vec::new();
    for node in nodes {
        if node.protocol.is_udp_only() {
            udp_nodes.push(node.clone());
        } else {
            tcp_nodes.push(node.clone());
        }
    }
    let tcp_results = if tcp_nodes.is_empty() {
        Vec::new()
    } else {
        ping_nodes(&tcp_nodes, Some(timeout_ms), Some(concurrency)).await?
    };
    let udp_results = if udp_nodes.is_empty() {
        Vec::new()
    } else {
        probe_nodes(
            &udp_nodes,
            Some(timeout_ms),
            Some(concurrency),
            clash,
            probe_url.to_string(),
        )
        .await?
    };
    // Merge back into the caller's order so index-based consumers are stable.
    let by_id: HashMap<String, LatencyResult> = tcp_results
        .into_iter()
        .chain(udp_results)
        .map(|r| (r.id.clone(), r))
        .collect();
    Ok(nodes
        .iter()
        .map(|n| {
            by_id.get(&n.id).cloned().unwrap_or(LatencyResult {
                id: n.id.clone(),
                name: n.name.clone(),
                latency_ms: None,
                error: Some("missing probe result".into()),
                tested_at: now_secs(),
                method: "error".into(),
            })
        })
        .collect())
}

fn spawn_probe_task(
    tasks: &mut tokio::task::JoinSet<(usize, LatencyResult)>,
    index: usize,
    node: ProxyNode,
    timeout_ms: u64,
    clash: Option<ClashApi>,
    probe_url: String,
) {
    tasks.spawn(async move {
        let id = node.id.clone();
        let name = node.name.clone();
        let server = node.server.clone();
        let port = node.port;
        let tag = outbound_tag(&node);
        // Through-kernel delay is the only meaningful health signal for TCP
        // protocols too: a plain TCP connect succeeds for nodes whose proxy
        // path is broken (e.g. mihomo/other-core nodes ace TCP handshakes and
        // then die mid-proxy). Whenever the core is up, ask the kernel to
        // dial the probe URL through the node; direct TCP remains the
        // fallback for a stopped core and for callers without a mapping
        // into the running config (custom sing-box profiles, Xray metrics
        // mode). UDP-only protocols cannot fall back at all — report that
        // explicitly instead of a TCP probe that can only ever time out.
        let use_clash = clash.is_some();
        if node.protocol.is_udp_only() && clash.is_none() {
            return (
                index,
                LatencyResult {
                    id,
                    name,
                    latency_ms: None,
                    error: Some("core not running: start the proxy to test this protocol".into()),
                    tested_at: now_secs(),
                    method: "unsupported".into(),
                },
            );
        }
        let key = if use_clash {
            let api = clash.as_ref().expect("checked by use_clash");
            format!(
                "clash|{}|{}|{id}|{tag}|{probe_url}|{timeout_ms}",
                api.base, api.secret
            )
        } else {
            format!("tcp|{id}|{server}|{port}|{timeout_ms}")
        };
        let result = probe_coalesced(key, move || async move {
            if use_clash {
                probe_clash(
                    clash.expect("checked by use_clash"),
                    id,
                    name,
                    tag,
                    probe_url,
                    timeout_ms,
                )
                .await
            } else {
                probe_tcp(id, name, &server, port, timeout_ms).await
            }
        })
        .await;
        (index, result)
    });
}

async fn probe_coalesced<F, Fut>(key: String, probe: F) -> LatencyResult
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = LatencyResult>,
{
    if let Some(result) = cached_result(&key) {
        return result;
    }

    let probe_lock = {
        let mut map = PROBE_LOCKS.lock().unwrap_or_else(|p| p.into_inner());
        Arc::clone(
            map.entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    };
    let _key_guard = probe_lock.lock().await;
    if let Some(result) = cached_result(&key) {
        return result;
    }

    let _global_permit = Arc::clone(&GLOBAL_SEMAPHORE)
        .acquire_owned()
        .await
        .expect("global probe semaphore");
    let result = probe().await;
    cache_result(key.clone(), result.clone());
    let mut locks = PROBE_LOCKS.lock().unwrap_or_else(|p| p.into_inner());
    if locks
        .get(&key)
        .map(|current| Arc::ptr_eq(current, &probe_lock))
        .unwrap_or(false)
    {
        locks.remove(&key);
    }
    result
}

fn cached_result(key: &str) -> Option<LatencyResult> {
    let mut cache = PROBE_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    match cache.entries.get(key) {
        Some((at, result)) if at.elapsed() < cache_ttl(result) => Some(result.clone()),
        Some(_) => {
            cache.entries.remove(key);
            None
        }
        None => None,
    }
}

fn cache_result(key: String, result: LatencyResult) {
    let mut cache = PROBE_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();
    if cache.last_prune.elapsed() >= FAILURE_CACHE_TTL
        || (cache.entries.len() >= MAX_CACHE_ENTRIES && !cache.entries.contains_key(&key))
    {
        cache
            .entries
            .retain(|_, (at, cached)| at.elapsed() < cache_ttl(cached));
        cache.last_prune = now;
    }
    if cache.entries.len() >= MAX_CACHE_ENTRIES && !cache.entries.contains_key(&key) {
        let remove_count = cache.entries.len().saturating_sub(CACHE_TRIM_TO) + 1;
        let mut oldest: Vec<_> = cache
            .entries
            .iter()
            .map(|(entry_key, (at, _))| (entry_key.clone(), *at))
            .collect();
        oldest.sort_unstable_by_key(|(_, at)| *at);
        for (entry_key, _) in oldest.into_iter().take(remove_count) {
            cache.entries.remove(&entry_key);
        }
    }
    cache.entries.insert(key, (now, result));
}

fn cache_ttl(result: &LatencyResult) -> Duration {
    if result.latency_ms.is_some() {
        CACHE_TTL
    } else {
        FAILURE_CACHE_TTL
    }
}

async fn probe_clash(
    api: ClashApi,
    id: String,
    name: String,
    tag: String,
    probe_url: String,
    timeout_ms: u64,
) -> LatencyResult {
    let tested_at = now_secs();
    // Unified delay: two sequential URL tests; prefer the second (warm path).
    // Mirrors mihomo `unified-delay` / FlClash default.
    let result = tokio::task::spawn_blocking(move || {
        let first = api.delay(&tag, &probe_url, timeout_ms);
        let second = api.delay(&tag, &probe_url, timeout_ms);
        match (first, second) {
            (_, Ok(ms2)) => Ok(ms2),
            (Ok(ms1), Err(_)) => Ok(ms1),
            (Err(e1), Err(e2)) => Err(format!("{e1}; retry: {e2}")),
        }
    })
    .await;

    match result {
        Ok(Ok(ms)) => LatencyResult {
            id,
            name,
            latency_ms: Some(ms),
            error: None,
            tested_at,
            method: "clash_api".into(),
        },
        Ok(Err(e)) => LatencyResult {
            id,
            name,
            latency_ms: None,
            error: Some(e),
            tested_at,
            method: "clash_api".into(),
        },
        Err(e) => LatencyResult {
            id,
            name,
            latency_ms: None,
            error: Some(format!("join: {e}")),
            tested_at,
            method: "clash_api".into(),
        },
    }
}

async fn probe_tcp(
    id: String,
    name: String,
    server: &str,
    port: u16,
    timeout_ms: u64,
) -> LatencyResult {
    let tested_at = now_secs();
    let addr = format!("{server}:{port}");
    let start = Instant::now();

    match timeout(
        Duration::from_millis(timeout_ms),
        TcpStream::connect(addr.as_str()),
    )
    .await
    {
        Ok(Ok(_stream)) => {
            let ms = start.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            LatencyResult {
                id,
                name,
                latency_ms: Some(ms),
                error: None,
                tested_at,
                method: "tcp".into(),
            }
        }
        Ok(Err(e)) => LatencyResult {
            id,
            name,
            latency_ms: None,
            error: Some(e.to_string()),
            tested_at,
            method: "tcp".into(),
        },
        Err(_) => LatencyResult {
            id,
            name,
            latency_ms: None,
            error: Some("timeout".into()),
            tested_at,
            method: "tcp".into(),
        },
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn result(ms: Option<u32>) -> LatencyResult {
        LatencyResult {
            id: "test-node".into(),
            name: "test".into(),
            latency_ms: ms,
            error: ms.is_none().then(|| "failed".into()),
            tested_at: now_secs(),
            method: "test".into(),
        }
    }

    fn unique_key(label: &str) -> String {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        format!("test|{label}|{}", NEXT.fetch_add(1, Ordering::Relaxed))
    }

    fn node(protocol: crate::domain::Protocol) -> ProxyNode {
        use crate::domain::ProtocolConfig;
        ProxyNode {
            id: unique_key("node"),
            name: "test-node".into(),
            protocol,
            server: "127.0.0.1".into(),
            // Nothing listens here; TCP connect fails fast (connection refused)
            // instead of waiting out the timeout.
            port: 1,
            tls: None,
            transport: None,
            udp: None,
            config: ProtocolConfig::Hysteria2 {
                password: "x".into(),
                up_mbps: None,
                down_mbps: None,
                obfs: None,
                obfs_password: None,
            },
            source: None,
            latency_ms: None,
            latency_at: None,
        }
    }

    // Hysteria2/Hysteria/Tuic are QUIC-only: a plain TCP connect to their port
    // always fails regardless of node health, so probe_nodes must route them
    // through the clash delay API (when available) instead of TCP. This is
    // the behavior the UI "测速" bug report depended on — without it, every
    // hy2 node reports a spurious timeout even when the node is reachable.
    #[tokio::test]
    async fn udp_only_protocols_use_clash_api_when_available_not_tcp() {
        use crate::domain::Protocol;

        // Nothing is listening on this port; ClashApi::delay will fail, but
        // the point under test is *which* probe path ran, recorded in
        // LatencyResult::method regardless of success.
        let clash = crate::api::ClashApi::new("127.0.0.1", 1, "secret");

        for protocol in [Protocol::Hysteria2, Protocol::Hysteria, Protocol::Tuic] {
            let nodes = vec![node(protocol)];
            let results = probe_nodes(
                &nodes,
                Some(200),
                Some(1),
                Some(clash.clone()),
                String::new(),
            )
            .await
            .unwrap();
            assert_eq!(
                results[0].method, "clash_api",
                "{protocol:?} must probe via clash_api, not raw TCP"
            );
        }

        // A TCP-based protocol also probes through the kernel when the
        // clash API is available — raw TCP reachability lies about proxy
        // health (e.g. mihomo/other-core nodes ace TCP and die mid-proxy).
        // The delay call itself fails here (nothing on port 1), but the
        // point under test is *which* probe path ran.
        let nodes = vec![node(Protocol::Shadowsocks)];
        let results = probe_nodes(&nodes, Some(200), Some(1), Some(clash), String::new())
            .await
            .unwrap();
        assert_eq!(results[0].method, "clash_api");
    }

    // Without the clash API (core stopped / custom profiles / Xray), TCP
    // protocols fall back to the direct-TCP probe — still the raw signal,
    // but the only one available.
    #[tokio::test]
    async fn tcp_protocols_fall_back_to_direct_probe_without_clash_api() {
        use crate::domain::Protocol;
        let nodes = vec![node(Protocol::Shadowsocks)];
        let results = probe_nodes(&nodes, Some(200), Some(1), None, String::new())
            .await
            .unwrap();
        assert_eq!(results[0].method, "tcp");
    }

    // Without a running core there is no way to speak QUIC-only protocols at
    // all — a raw TCP probe would always time out and look like a dead node,
    // so probe_nodes must report "unsupported" explicitly instead of running
    // a doomed TCP probe (the bug this behavior fixes: hy2 nodes always
    // showing timeout even when perfectly reachable).
    #[tokio::test]
    async fn udp_only_protocols_report_unsupported_without_clash_api() {
        use crate::domain::Protocol;
        let nodes = vec![node(Protocol::Hysteria2)];
        let results = probe_nodes(&nodes, Some(200), Some(1), None, String::new())
            .await
            .unwrap();
        assert_eq!(results[0].method, "unsupported");
        assert!(results[0].latency_ms.is_none());
        assert!(results[0].error.is_some());
    }

    // The ping button must not tell users to "start the core" — it never
    // uses the core at all. QUIC-only protocols are simply unpingable.
    #[tokio::test]
    async fn ping_nodes_flags_quic_only_without_core_not_running_note() {
        use crate::domain::Protocol;
        let nodes = vec![node(Protocol::Hysteria2)];
        let results = ping_nodes(&nodes, Some(200), Some(1)).await.unwrap();
        assert_eq!(results[0].method, "unsupported");
        let err = results[0].error.as_deref().unwrap_or_default();
        assert!(
            !err.contains("core not running"),
            "ping note must not claim the core is stopped: {err}"
        );
    }

    // Smart-switch ranking: TCP-capable nodes ride the fast TCP ping even
    // when the clash API is available; QUIC-only nodes fall back to the
    // through-kernel URL probe. Order follows the input.
    #[tokio::test]
    async fn ranked_probes_use_tcp_for_tcp_capable_and_clash_for_quic() {
        use crate::domain::Protocol;
        let clash = crate::api::ClashApi::new("127.0.0.1", 1, "secret");
        let nodes = vec![node(Protocol::Shadowsocks), node(Protocol::Hysteria2)];
        let results = probe_nodes_ranked(&nodes, 200, 2, Some(clash), "")
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, nodes[0].id, "caller order must be kept");
        assert_eq!(results[0].method, "tcp");
        assert_eq!(results[1].method, "clash_api");
    }

    #[tokio::test]
    async fn identical_in_flight_probes_are_coalesced_and_cached() {
        let key = unique_key("coalesce");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..12 {
            let key = key.clone();
            let calls = Arc::clone(&calls);
            tasks.push(tokio::spawn(async move {
                probe_coalesced(key, || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    result(Some(42))
                })
                .await
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap().latency_ms, Some(42));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let cached = probe_coalesced(key, || async {
            panic!("fresh successful result must be reused");
        })
        .await;
        assert_eq!(cached.latency_ms, Some(42));
    }

    #[tokio::test]
    async fn global_probe_concurrency_never_exceeds_thirty() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for i in 0..45 {
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            tasks.push(tokio::spawn(async move {
                probe_coalesced(unique_key(&format!("global-{i}")), || async move {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(15)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    result(Some(10))
                })
                .await
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= GLOBAL_CONCURRENCY);
    }

    #[test]
    fn failures_use_shorter_cache_ttl() {
        assert_eq!(cache_ttl(&result(Some(1))), CACHE_TTL);
        assert_eq!(cache_ttl(&result(None)), FAILURE_CACHE_TTL);
    }
}
