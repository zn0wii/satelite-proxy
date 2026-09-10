//! Kernel log stream listener: subscribe to the Clash API `/logs` WebSocket
//! and turn outbound dial failures into passive smart-switch samples.
//!
//! mihomo-only by design. The two kernels expose failed dials differently:
//! - sing-box registers connections in `/connections` *before* the outbound
//!   dial (`route/route.go`: trackers wrap pre-dial), so its connection
//!   journal already captures dial timeouts as zero-byte closes
//!   (`Runtime::passive_node_stats`).
//! - mihomo creates its tracker only *after* the dial succeeds
//!   (`tunnel/tunnel.go`: `statistic.NewTCPTracker` runs post-`DialContext`),
//!   so a dial-timeout death never appears in `/connections` at all — the
//!   kernel log stream is the only passive signal for it.
//!
//! Cost: warning-level subscription → silent while healthy; one small JSON
//! parse + substring scan per event; state is a bounded ring on `Runtime`.

use crate::api::ClashApi;
use crate::state::AppState;
use serde::Deserialize;
use std::net::TcpStream;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::Message;
use tungstenite::{client as ws_client, Error as WsError};

const IDLE_MS: u64 = 1_000;
const RECONNECT_MS: u64 = 2_000;
/// WS read timeout — also the cadence at which we re-check session liveness.
const READ_TIMEOUT_MS: u64 = 500;

/// Main selector group emitted by the config generators (恒 `proxy`).
const MAIN_GROUP: &str = "proxy";
/// Node outbound tags (`node-<id16>`), shared by every generator.
const NODE_TAG_PREFIX: &str = "node-";

pub fn spawn_log_listener(app: AppHandle) {
    if let Err(error) = thread::Builder::new()
        .name("log-listener".into())
        .spawn(move || log_listener_loop(app))
    {
        crate::app_log::error(
            "log_listener",
            format!("failed to start log listener: {error}"),
        );
    }
}

fn log_listener_loop(app: AppHandle) {
    loop {
        let Some(state) = app.try_state::<AppState>() else {
            thread::sleep(Duration::from_millis(IDLE_MS));
            continue;
        };
        if state.is_core_transitioning() {
            thread::sleep(Duration::from_millis(IDLE_MS));
            continue;
        }

        let (api, is_mihomo) = {
            let rt = state.lock_runtime();
            (
                rt.clash_api_clone(),
                rt.core.kind() == crate::core::CoreKind::Mihomo,
            )
        };
        // sing-box: the connection journal already sees dial failures — a
        // second (identical) signal would double-count its passive samples.
        // Xray has no Clash API (and no smart switch) at all.
        let Some(api) = api else {
            thread::sleep(Duration::from_millis(IDLE_MS));
            continue;
        };
        if !is_mihomo || !state.is_core_running() {
            thread::sleep(Duration::from_millis(IDLE_MS));
            continue;
        }

        if let Err(e) = stream_kernel_logs(&state, &api) {
            crate::app_log::debug("log_listener", format!("log WS: {e}"));
            thread::sleep(Duration::from_millis(RECONNECT_MS));
        }
    }
}

fn stream_kernel_logs(state: &AppState, api: &ClashApi) -> Result<(), String> {
    let url = api.logs_ws_url("warning");
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
        .unwrap_or("127.0.0.1:19090")
        .to_string();

    let stream = TcpStream::connect(&host_port).map_err(|e| format!("tcp {host_port}: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS)));
    let _ = stream.set_nodelay(true);

    let (mut socket, _resp) =
        ws_client(request, stream).map_err(|e| format!("ws handshake: {e}"))?;

    loop {
        // Core restarted (new session token) or is transitioning → re-acquire.
        if !api.is_active() || state.is_core_transitioning() {
            let _ = socket.close(None);
            return Ok(());
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                if let Some(line) = parse_dial_failure_frame(text.as_str()) {
                    record_failure(state, line);
                }
            }
            Ok(Message::Binary(bin)) => {
                if let Ok(text) = std::str::from_utf8(&bin) {
                    if let Some(line) = parse_dial_failure_frame(text) {
                        record_failure(state, line);
                    }
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

/// One parsed dial-failure log line.
#[derive(Debug, PartialEq)]
struct DialFailureLine {
    /// Outbound the failure is routed through (group or node tag).
    proxy: String,
    /// Destination host key (port stripped) for the multi-dest gate.
    dest: String,
}

/// `/logs` WS frame: `{"type": "warning"|"error", "payload": "<line>"}`.
#[derive(Deserialize)]
struct LogFrame {
    #[serde(rename = "type")]
    level: String,
    payload: String,
}

fn parse_dial_failure_frame(text: &str) -> Option<DialFailureLine> {
    let frame: LogFrame = serde_json::from_str(text).ok()?;
    let level = frame.level.to_ascii_lowercase();
    if level != "warning" && level != "error" {
        return None;
    }
    parse_dial_failure(&frame.payload)
}

/// mihomo `logMetadataErr` (tunnel/tunnel.go, Warnln) shapes:
///
/// ```text
/// [TCP] dial <proxy> (match <RuleType>/<payload>) <src> --> <dst> error: <err>
/// [TCP] dial <proxy> <src> --> <dst> error: <err>              (rule == nil)
/// ```
///
/// Emitted per retry (up to 10 attempts per connection), `[UDP]` likewise.
/// The rule-nil form only occurs for DIRECT/GLOBAL routing (no rule
/// matched), whose proxy name is not a node anyway.
fn parse_dial_failure(payload: &str) -> Option<DialFailureLine> {
    let rest = payload
        .strip_prefix("[TCP] dial ")
        .or_else(|| payload.strip_prefix("[UDP] dial "))?;
    let err_idx = rest.find(" error: ")?;
    let head = &rest[..err_idx]; // "<proxy> (match ...) <src> --> <dst>"
    let arrow = head.rfind(" --> ")?;
    let dest = dest_host_key(head[arrow + 5..].trim());
    let name_part = &head[..arrow];
    let proxy = match name_part.find(" (match ") {
        Some(i) => name_part[..i].trim(),
        None => name_part
            .split(' ')
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())?,
    };
    if proxy.is_empty() || dest.is_empty() {
        return None;
    }
    Some(DialFailureLine {
        proxy: proxy.to_string(),
        dest,
    })
}

/// `"github.com:443"` → `"github.com"`; `"[2001:db8::1]:443"` →
/// `"2001:db8::1"`; bare hosts pass through. Port-less IPv6 literals (which
/// end in `:` after the split) are returned untouched.
fn dest_host_key(dest: &str) -> String {
    if let Some((host, port)) = dest.rsplit_once(':') {
        if !port.is_empty()
            && port.chars().all(|c| c.is_ascii_digit())
            && !host.is_empty()
            && !host.ends_with(':')
        {
            return host.trim_matches(|c| c == '[' || c == ']').to_string();
        }
    }
    dest.to_string()
}

/// Attribute the failure and store it as a passive sample.
///
/// - `node-<id16>` (rule-pinned node): recorded under its own tag.
/// - `proxy` (main selector group): attributed to the group's current
///   selection — the app-persisted current node. A switch mid-flight keeps
///   old events on the old node's tag, which is the era they belong to.
/// - anything else (`DIRECT` / `REJECT` / `GLOBAL` / `smart-*` pools):
///   ignored — not a main-node health signal (DIRECT dial failures must not
///   blame the proxy node).
fn record_failure(state: &AppState, line: DialFailureLine) {
    let tag = if line.proxy.starts_with(NODE_TAG_PREFIX) {
        line.proxy
    } else if line.proxy == MAIN_GROUP {
        let current = state.with_store(|s| {
            Ok(s.settings
                .current_node_id
                .as_ref()
                .and_then(|id| s.find_node(id))
                .map(crate::config::outbound_tag))
        });
        match current.unwrap_or(None) {
            Some(tag) => tag,
            None => return,
        }
    } else {
        return;
    };

    state
        .lock_runtime()
        .record_proxy_dial_failure(&tag, &line.dest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rule_matched_dial_failure() {
        let line = parse_dial_failure(
            "[TCP] dial proxy (match RuleSet/proxy) 192.168.1.2:52341 --> github.com:443 error: dial tcp 13.250.231.171:39645: i/o timeout",
        )
        .expect("should parse");
        assert_eq!(line.proxy, "proxy");
        assert_eq!(line.dest, "github.com");
    }

    #[test]
    fn parses_retry_line_and_pinned_node() {
        let line = parse_dial_failure(
            "[TCP] dial node-2db123c73eacab2b (match GeoSite/cn) 192.168.1.2:1 --> ipwho.is:443 error: connect: connection refused, retry 2",
        )
        .expect("should parse");
        assert_eq!(line.proxy, "node-2db123c73eacab2b");
        assert_eq!(line.dest, "ipwho.is");
    }

    #[test]
    fn parses_rule_nil_form() {
        let line = parse_dial_failure(
            "[UDP] dial GLOBAL 192.168.1.2:1 --> 8.8.8.8:53 error: dial udp: i/o timeout",
        )
        .expect("should parse");
        assert_eq!(line.proxy, "GLOBAL");
        assert_eq!(line.dest, "8.8.8.8");
    }

    #[test]
    fn parses_ipv6_destination() {
        let line = parse_dial_failure(
            "[TCP] dial proxy (match Match) ::1 --> [2001:db8::1]:443 error: i/o timeout",
        )
        .expect("should parse");
        assert_eq!(line.dest, "2001:db8::1");
    }

    #[test]
    fn ignores_non_dial_lines() {
        assert!(parse_dial_failure(
            "[TCP] connected 1.2.3.4:443 --> github.com:443 match RuleSet/x using proxy"
        )
        .is_none());
        assert!(parse_dial_failure("start initial compatible provider(PROXY) fetch").is_none());
        // Info-level connection-matched lines (shape check only — the level
        // filter already rejects them before parsing).
        assert!(parse_dial_failure("[TCP] connected ...").is_none());
    }

    #[test]
    fn frame_level_filter() {
        let warn = parse_dial_failure_frame(
            r#"{"type":"warning","payload":"[TCP] dial proxy (match Match) a --> b.com:443 error: x"}"#,
        );
        assert!(warn.is_some());
        let info = parse_dial_failure_frame(
            r#"{"type":"info","payload":"[TCP] dial proxy (match Match) a --> b.com:443 error: x"}"#,
        );
        assert!(info.is_none());
        let not_json = parse_dial_failure_frame("not json");
        assert!(not_json.is_none());
    }

    #[test]
    fn dest_key_shapes() {
        assert_eq!(dest_host_key("github.com:443"), "github.com");
        assert_eq!(dest_host_key("[2001:db8::1]:443"), "2001:db8::1");
        assert_eq!(dest_host_key("2001:db8::1"), "2001:db8::1");
        assert_eq!(dest_host_key("1.2.3.4:80"), "1.2.3.4");
        assert_eq!(dest_host_key("bare-host"), "bare-host");
    }
}
