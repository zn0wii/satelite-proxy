//! Connection journal: stream Clash API snapshots and maintain history.
//!
//! Interval adapts: fast while a connection-data consumer is on screen,
//! slow otherwise (docs/webview2-memory-optimization-plan.md).

use crate::api::{parse_connections_json, ClashApi};
use crate::state::AppState;
use std::net::TcpStream;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::Message;
use tungstenite::{client as ws_client, Error as WsError};

/// Fast rate: catch short-lived conns (live tables + journal fidelity).
const WS_INTERVAL_ACTIVE_MS: u64 = 100;
/// Idle rate: no connection-data consumer on screen (UI pages poll at
/// 1.5–2.5s; anything reading faster than this keeps ACTIVE alive).
const WS_INTERVAL_BACKGROUND_MS: u64 = 1000;
/// A connection-data command within this window counts as live demand.
const DEMAND_WINDOW: Duration = Duration::from_secs(4);
/// Re-evaluate the interval at most this often — the rate is baked into the
/// WS URL, so every change is a reconnect; boundary flapping (snapshot size
/// hovering at a tier edge, page switches) must not churn sockets.
const INTERVAL_RECHECK: Duration = Duration::from_secs(2);
/// TUN sees every machine connection — frames reach thousands of rows and
/// MB-scale JSON. Stretch the interval so per-frame cost stays bounded.
/// Floors must stay ≥ ACTIVE (mihomo's /connections ticker panics on ≤ 0).
const TUN_TIER1_CONNS: usize = 1_500;
const TUN_TIER1_MS: u64 = 250;
const TUN_TIER2_CONNS: usize = 4_000;
const TUN_TIER2_MS: u64 = 1_000;
/// HTTP poll when WS is unavailable.
const FALLBACK_HTTP_MS: u64 = 350;
const IDLE_MS: u64 = 500;
const RECONNECT_MS: u64 = 500;

pub fn spawn_connection_journal(app: AppHandle) {
    if let Err(error) = thread::Builder::new()
        .name("conn-journal".into())
        .spawn(move || journal_loop(app))
    {
        crate::app_log::error(
            "journal",
            format!("failed to start connection journal: {error}"),
        );
    }
}

fn journal_interval_ms(state: &AppState) -> u64 {
    interval_for(
        state.is_ui_visible(),
        state.conn_query_recent(DEMAND_WINDOW),
        state.snapshot_connections(),
    )
}

/// Pure rate decision (unit-tested): fast only when visible AND someone is
/// actually reading connection data; TUN-sized frames back off regardless.
fn interval_for(visible: bool, demand: bool, last_frame_conns: usize) -> u64 {
    let base = if visible && demand {
        WS_INTERVAL_ACTIVE_MS
    } else {
        WS_INTERVAL_BACKGROUND_MS
    };
    let tun_floor = if last_frame_conns > TUN_TIER2_CONNS {
        TUN_TIER2_MS
    } else if last_frame_conns > TUN_TIER1_CONNS {
        TUN_TIER1_MS
    } else {
        WS_INTERVAL_ACTIVE_MS
    };
    base.max(tun_floor)
}

fn journal_loop(app: AppHandle) {
    loop {
        let Some(state) = app.try_state::<AppState>() else {
            thread::sleep(Duration::from_millis(IDLE_MS));
            continue;
        };

        if state.is_core_transitioning() {
            thread::sleep(Duration::from_millis(IDLE_MS));
            continue;
        }

        let api = state.try_clash_api_clone();

        let Some(api) = api else {
            // Xray mode: no Clash API — poll the metrics module for traffic
            // totals instead. Per-connection data does not exist.
            if let Some(metrics) = state.try_xray_metrics_clone() {
                poll_xray_metrics(&state, &metrics);
            } else {
                thread::sleep(Duration::from_millis(IDLE_MS));
            }
            continue;
        };

        // A spontaneously dead core (crash / external kill) leaves the stale
        // ClashApi handle behind — retrying WS against its corpse spams
        // connection-refused logs forever. Park until it is (re)started.
        if !state.is_core_running() {
            thread::sleep(Duration::from_millis(IDLE_MS));
            continue;
        }

        let interval = journal_interval_ms(&state);
        let chosen_at = std::time::Instant::now();

        match stream_ws_snapshots(
            &api,
            interval,
            // Rate changes cost a WS reconnect (the interval is baked into
            // the URL) — throttle re-evaluation so demand expiry / tier-edge
            // flapping cannot churn sockets.
            || chosen_at.elapsed() >= INTERVAL_RECHECK && journal_interval_ms(&state) != interval,
            |text| {
                if state.is_core_transitioning() {
                    return;
                }
                match parse_connections_json(text) {
                    Ok(snap) => {
                        state.try_apply_connection_snapshot(&api, snap);
                    }
                    Err(e) => {
                        crate::app_log::debug("journal", format!("parse: {e}"));
                    }
                }
            },
        ) {
            Ok(()) => {}
            Err(e) => {
                crate::app_log::debug("journal", format!("WS: {e}; fallback HTTP"));
                for _ in 0..10 {
                    if state.is_core_transitioning() {
                        break;
                    }
                    let still = state.try_clash_api_clone();
                    let Some(api) = still else { break };
                    match api.list_connections() {
                        Ok(snap) => {
                            state.try_apply_connection_snapshot(&api, snap);
                        }
                        Err(_) => break,
                    }
                    thread::sleep(Duration::from_millis(FALLBACK_HTTP_MS));
                }
                thread::sleep(Duration::from_millis(RECONNECT_MS));
            }
        }
    }
}

/// Xray metrics polling loop: sample `/debug/vars` traffic totals until the
/// session goes away or a core transition starts. 1s cadence — the counters
/// are cumulative, so faster polling buys nothing.
fn poll_xray_metrics(state: &AppState, metrics: &crate::api::XrayMetrics) {
    const XRAY_POLL_MS: u64 = 1000;
    while metrics.is_active() && !state.is_core_transitioning() {
        match metrics.traffic_totals() {
            Some(totals) => {
                state.try_apply_metrics_snapshot(metrics, totals);
            }
            None => break,
        }
        for _ in 0..(XRAY_POLL_MS / 50) {
            if !metrics.is_active() || state.is_core_transitioning() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn stream_ws_snapshots(
    api: &ClashApi,
    interval_ms: u64,
    mut interval_changed: impl FnMut() -> bool,
    mut on_text: impl FnMut(&str),
) -> Result<(), String> {
    let url = api.connections_ws_url(interval_ms);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("ws request: {e}"))?;
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", api.secret)
            .parse()
            .map_err(|e| format!("auth header: {e}"))?,
    );

    let host_port = api
        .base
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or("127.0.0.1:19090");

    let stream = TcpStream::connect(host_port).map_err(|e| format!("tcp {host_port}: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let _ = stream.set_nodelay(true);

    let (mut socket, _resp) =
        ws_client(request, stream).map_err(|e| format!("ws handshake: {e}"))?;

    loop {
        if !api.is_active() {
            let _ = socket.close(None);
            return Ok(());
        }
        if interval_changed() {
            let _ = socket.close(None);
            return Ok(());
        }
        match socket.read() {
            Ok(Message::Text(text)) => on_text(text.as_str()),
            Ok(Message::Binary(bin)) => {
                if let Ok(text) = std::str::from_utf8(&bin) {
                    on_text(text);
                }
            }
            Ok(Message::Ping(p)) => {
                let _ = socket.send(Message::Pong(p));
            }
            Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
            Ok(Message::Close(_)) => return Ok(()),
            Err(WsError::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(WsError::ConnectionClosed) | Err(WsError::AlreadyClosed) => return Ok(()),
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_rate_needs_visibility_and_demand() {
        assert_eq!(
            interval_for(true, true, 0),
            WS_INTERVAL_ACTIVE_MS,
            "visible + live consumer → fast"
        );
        // No consumer on screen (nodes/settings pages) or hidden to tray:
        // nothing reads faster than 1.5s — journal at background rate.
        assert_eq!(interval_for(true, false, 0), WS_INTERVAL_BACKGROUND_MS);
        assert_eq!(interval_for(false, true, 0), WS_INTERVAL_BACKGROUND_MS);
        assert_eq!(interval_for(false, false, 0), WS_INTERVAL_BACKGROUND_MS);
    }

    #[test]
    fn tun_sized_frames_back_off_even_with_consumers() {
        assert_eq!(
            interval_for(true, true, TUN_TIER1_CONNS),
            WS_INTERVAL_ACTIVE_MS
        );
        assert_eq!(interval_for(true, true, TUN_TIER1_CONNS + 1), TUN_TIER1_MS);
        assert_eq!(interval_for(true, true, TUN_TIER2_CONNS + 1), TUN_TIER2_MS);
        // Floors never push the background rate back down to ACTIVE, and
        // never below ACTIVE (mihomo's ticker panics on ≤ 0).
        assert_eq!(
            interval_for(false, false, TUN_TIER2_CONNS + 1),
            WS_INTERVAL_BACKGROUND_MS
        );
        assert!(interval_for(true, true, usize::MAX) >= WS_INTERVAL_ACTIVE_MS);
        assert!(WS_INTERVAL_ACTIVE_MS > 0);
    }
}
