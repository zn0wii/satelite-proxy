//! Parse sing-box JSON (full config, `{ "outbounds": [...] }`, or a single outbound).

use crate::domain::{
    ParseResult, Protocol, ProtocolConfig, ProxyNode, SkippedProxy, SubscriptionFormat, TlsConfig,
    Transport,
};
use crate::error::{AppError, AppResult};
use crate::subscription::json_util::{
    as_object, get_bool, get_obj, get_str, get_str_list, get_u16, get_u32, get_u8,
    map_to_string_map, value_to_string,
};
use serde_json::{Map, Value};

const SKIP_TYPES: &[&str] = &[
    "direct",
    "block",
    "dns",
    "selector",
    "urltest",
    "loadbalance",
    "relay",
    "pass",
];

/// A complete sing-box document that can be passed to `sing-box run -c`.
/// Requires a JSON object with both `inbounds` and `outbounds` arrays.
pub fn validate_complete_singbox_config(content: &str) -> AppResult<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(AppError::EmptySubscription);
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| AppError::SubscriptionParse(format!("sing-box 配置必须是合法 JSON：{e}")))?;
    let obj = value.as_object().ok_or_else(|| {
        AppError::SubscriptionParse("sing-box 配置必须是 JSON 对象，不能是数组或片段".into())
    })?;
    let inbounds = obj
        .get("inbounds")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AppError::SubscriptionParse("完整 sing-box 配置必须包含 inbounds 数组".into())
        })?;
    let outbounds = obj
        .get("outbounds")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AppError::SubscriptionParse("完整 sing-box 配置必须包含 outbounds 数组".into())
        })?;
    if inbounds.is_empty() {
        return Err(AppError::SubscriptionParse(
            "完整 sing-box 配置的 inbounds 不能为空".into(),
        ));
    }
    if outbounds.is_empty() {
        return Err(AppError::SubscriptionParse(
            "完整 sing-box 配置的 outbounds 不能为空".into(),
        ));
    }
    serde_json::to_string_pretty(&value)
        .map_err(|e| AppError::SubscriptionParse(format!("serialize sing-box config: {e}")))
}

pub fn looks_like_singbox_json(value: &Value) -> bool {
    match value {
        Value::Array(arr) => arr.iter().any(is_outbound_like),
        Value::Object(map) => {
            if map.contains_key("outbounds") {
                return true;
            }
            is_outbound_object(map)
        }
        _ => false,
    }
}

fn is_outbound_like(value: &Value) -> bool {
    as_object(value).is_some_and(is_outbound_object)
}

fn is_outbound_object(map: &Map<String, Value>) -> bool {
    let Some(type_str) = get_str(map, &["type"]) else {
        return false;
    };
    let t = type_str.to_ascii_lowercase();
    if SKIP_TYPES.contains(&t.as_str()) {
        return true;
    }
    Protocol::from_singbox_type(&t).is_some()
        && (map.contains_key("server")
            || map.contains_key("server_port")
            || map.contains_key("tag")
            || matches!(t.as_str(), "tor" | "wireguard"))
}

/// Parse a full sing-box document, an `{ "outbounds": [...] }` object, an outbound
/// array, or a single outbound object.
pub fn parse_singbox_json(content: &str) -> AppResult<ParseResult> {
    let content = content.trim();
    if content.is_empty() {
        return Err(AppError::EmptySubscription);
    }
    let root: Value = serde_json::from_str(content)
        .map_err(|e| AppError::SubscriptionParse(format!("invalid json: {e}")))?;
    parse_singbox_value(&root)
}

pub fn parse_singbox_value(root: &Value) -> AppResult<ParseResult> {
    let items = extract_outbounds(root)
        .ok_or_else(|| AppError::SubscriptionParse("no sing-box outbounds found".into()))?;
    crate::subscription::ensure_entry_limit(items.len())?;

    let mut nodes = Vec::new();
    let mut skipped = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        match parse_outbound(item) {
            Ok(ParseOutbound::Node(node)) => nodes.push(node.with_computed_id()),
            Ok(ParseOutbound::Skip { name, reason }) => {
                skipped.push(SkippedProxy { name, reason });
            }
            Err(reason) => {
                let name = as_object(item)
                    .and_then(|m| get_str(m, &["tag", "name"]))
                    .or_else(|| Some(format!("index-{idx}")));
                skipped.push(SkippedProxy { name, reason });
            }
        }
    }

    if nodes.is_empty() {
        return Err(AppError::NoProxies);
    }

    Ok(ParseResult {
        nodes,
        skipped,
        format: SubscriptionFormat::SingboxJson,
    })
}

fn extract_outbounds(root: &Value) -> Option<Vec<&Value>> {
    match root {
        Value::Array(seq) => Some(seq.iter().collect()),
        Value::Object(map) => {
            if let Some(Value::Array(seq)) = map.get("outbounds") {
                return Some(seq.iter().collect());
            }
            if is_outbound_object(map) {
                return Some(vec![root]);
            }
            None
        }
        _ => None,
    }
}

enum ParseOutbound {
    Node(ProxyNode),
    Skip {
        name: Option<String>,
        reason: String,
    },
}

fn parse_outbound(value: &Value) -> Result<ParseOutbound, String> {
    let map = as_object(value).ok_or_else(|| "outbound is not an object".to_string())?;
    let type_str = get_str(map, &["type"]).ok_or_else(|| "missing type".to_string())?;
    let type_lc = type_str.to_ascii_lowercase();
    let name = get_str(map, &["tag", "name"]);

    if SKIP_TYPES.contains(&type_lc.as_str()) {
        return Ok(ParseOutbound::Skip {
            name: name.clone(),
            reason: format!("skipped outbound type: {type_lc}"),
        });
    }

    let protocol = Protocol::from_singbox_type(&type_lc)
        .ok_or_else(|| format!("unsupported type: {type_str}"))?;

    let (server, port) = server_and_port(map, protocol)?;
    let display = name.unwrap_or_else(|| format!("{type_lc}-{server}-{port}"));
    let udp = get_bool(map, &["udp"]);
    let (tls, transport, config) = match protocol {
        Protocol::Shadowsocks => parse_ss(map)?,
        Protocol::Vmess => parse_vmess(map)?,
        Protocol::Vless => parse_vless(map)?,
        Protocol::Trojan => parse_trojan(map)?,
        Protocol::Hysteria2 => parse_hysteria2(map)?,
        Protocol::Tuic => parse_tuic(map)?,
        Protocol::Socks5 => parse_socks5(map)?,
        Protocol::Http => parse_http(map)?,
        Protocol::Hysteria => parse_hysteria(map)?,
        Protocol::ShadowTls => parse_shadowtls(map)?,
        Protocol::Ssh => parse_ssh(map)?,
        Protocol::Naive => parse_naive(map)?,
        Protocol::Tor => parse_tor(map)?,
        Protocol::WireGuard => parse_wireguard(map)?,
        Protocol::AnyTls => parse_anytls(map)?,
        Protocol::Snell => parse_snell(map)?,
        // sing-box has no masque outbound; a "masque" type here is foreign
        // data — reject so the entry lands in `skipped` with a reason.
        Protocol::Masque => return Err("sing-box config cannot carry masque outbounds".into()),
    };

    Ok(ParseOutbound::Node(ProxyNode {
        id: String::new(),
        name: display,
        protocol,
        server,
        port,
        tls,
        transport,
        udp,
        config,
        source: Some(type_str),
        latency_ms: None,
        latency_at: None,
    }))
}

fn server_and_port(map: &Map<String, Value>, protocol: Protocol) -> Result<(String, u16), String> {
    if matches!(protocol, Protocol::Tor) {
        return Ok((
            get_str(map, &["server"]).unwrap_or_else(|| "localhost".into()),
            get_u16(map, &["server_port", "port"]).unwrap_or(0),
        ));
    }
    if matches!(protocol, Protocol::WireGuard) {
        if let Some(peer) = first_peer(map) {
            let server = get_str(peer, &["address", "server"])
                .or_else(|| get_str(map, &["server"]))
                .ok_or_else(|| "wireguard: missing server".to_string())?;
            let port = get_u16(peer, &["port", "server_port"])
                .or_else(|| get_u16(map, &["server_port", "port"]))
                .ok_or_else(|| "wireguard: missing port".to_string())?;
            return Ok((server, port));
        }
    }
    let server = get_str(map, &["server"]).ok_or_else(|| "missing server".to_string())?;
    let port = get_u16(map, &["server_port", "port"])
        .ok_or_else(|| "missing or invalid server_port".to_string())?;
    Ok((server, port))
}

fn first_peer(map: &Map<String, Value>) -> Option<&Map<String, Value>> {
    map.get("peers")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(Value::as_object)
}

fn parse_tls(map: &Map<String, Value>, default_enabled: bool) -> Option<TlsConfig> {
    let tls = get_obj(map, &["tls"]);
    let enabled = tls
        .and_then(|t| get_bool(t, &["enabled"]))
        .unwrap_or(default_enabled);
    if !enabled && tls.is_none() {
        return None;
    }
    if !enabled {
        return Some(TlsConfig {
            enabled: false,
            ..Default::default()
        });
    }
    let tls_map = tls;
    let mut cfg = TlsConfig {
        enabled: true,
        server_name: tls_map.and_then(|t| get_str(t, &["server_name", "servername", "sni"])),
        insecure: tls_map.and_then(|t| get_bool(t, &["insecure"])),
        alpn: tls_map.and_then(|t| get_str_list(t, &["alpn"])),
        utls_fingerprint: tls_map
            .and_then(|t| get_obj(t, &["utls"]))
            .and_then(|u| get_str(u, &["fingerprint"])),
        reality_public_key: None,
        reality_short_id: None,
    };
    if let Some(reality) = tls_map.and_then(|t| get_obj(t, &["reality"])) {
        if get_bool(reality, &["enabled"]).unwrap_or(true) {
            cfg.reality_public_key = get_str(reality, &["public_key"]);
            cfg.reality_short_id = get_str(reality, &["short_id"]);
            cfg.enabled = true;
        }
    }
    Some(cfg)
}

fn parse_transport(map: &Map<String, Value>) -> Option<Transport> {
    let t = get_obj(map, &["transport"])?;
    let kind = get_str(t, &["type"]).unwrap_or_else(|| "tcp".into());
    match kind.to_ascii_lowercase().as_str() {
        "ws" | "websocket" => {
            let headers = get_obj(t, &["headers"]).map(map_to_string_map);
            Some(Transport::Ws {
                path: get_str(t, &["path"]),
                headers,
                max_early_data: get_u32(t, &["max_early_data", "max-early-data"]),
            })
        }
        "grpc" => Some(Transport::Grpc {
            service_name: get_str(t, &["service_name", "serviceName"]),
        }),
        "http" | "h2" => {
            let host = match t.get("host") {
                Some(Value::Array(items)) => {
                    Some(items.iter().filter_map(value_to_string).collect())
                }
                Some(Value::String(s)) => Some(vec![s.clone()]),
                _ => None,
            };
            Some(Transport::Http {
                path: get_str(t, &["path"]),
                host,
            })
        }
        "httpupgrade" | "http-upgrade" => Some(Transport::HttpUpgrade {
            path: get_str(t, &["path"]),
            host: get_str(t, &["host"]),
        }),
        "tcp" | "" => Some(Transport::Tcp),
        _ => Some(Transport::Tcp),
    }
}

fn parse_ss(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let method =
        get_str(map, &["method", "cipher"]).ok_or_else(|| "ss: missing method".to_string())?;
    let password = get_str(map, &["password"]).ok_or_else(|| "ss: missing password".to_string())?;
    Ok((
        parse_tls(map, false),
        parse_transport(map),
        ProtocolConfig::Shadowsocks {
            method,
            password,
            plugin: get_str(map, &["plugin"]),
            plugin_opts: get_str(map, &["plugin_opts", "plugin-opts"]),
            shadow_tls: None,
        },
    ))
}

fn parse_vmess(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let uuid = get_str(map, &["uuid"]).ok_or_else(|| "vmess: missing uuid".to_string())?;
    Ok((
        parse_tls(map, false),
        parse_transport(map),
        ProtocolConfig::Vmess {
            uuid,
            alter_id: get_u16(map, &["alter_id", "alterId"]).unwrap_or(0),
            security: get_str(map, &["security", "cipher"]).unwrap_or_else(|| "auto".into()),
        },
    ))
}

fn parse_vless(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let uuid = get_str(map, &["uuid"]).ok_or_else(|| "vless: missing uuid".to_string())?;
    Ok((
        parse_tls(map, true),
        parse_transport(map),
        ProtocolConfig::Vless {
            uuid,
            flow: get_str(map, &["flow"]),
            packet_encoding: get_str(map, &["packet_encoding", "packet-encoding"])
                .unwrap_or_else(|| "xudp".into()),
        },
    ))
}

fn parse_trojan(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let password =
        get_str(map, &["password"]).ok_or_else(|| "trojan: missing password".to_string())?;
    let mut tls = parse_tls(map, true).unwrap_or(TlsConfig {
        enabled: true,
        ..Default::default()
    });
    tls.enabled = true;
    Ok((
        Some(tls),
        parse_transport(map),
        ProtocolConfig::Trojan { password },
    ))
}

fn parse_hysteria2(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let password = get_str(map, &["password", "auth"])
        .ok_or_else(|| "hysteria2: missing password".to_string())?;
    let (obfs, obfs_password) = if let Some(obfs_obj) = get_obj(map, &["obfs"]) {
        (
            get_str(obfs_obj, &["type"]),
            get_str(obfs_obj, &["password"]),
        )
    } else {
        (get_str(map, &["obfs"]), get_str(map, &["obfs_password"]))
    };
    let mut tls = parse_tls(map, true).unwrap_or(TlsConfig {
        enabled: true,
        ..Default::default()
    });
    tls.enabled = true;
    Ok((
        Some(tls),
        None,
        ProtocolConfig::Hysteria2 {
            password,
            up_mbps: get_u32(map, &["up_mbps", "up"]),
            down_mbps: get_u32(map, &["down_mbps", "down"]),
            obfs,
            obfs_password,
        },
    ))
}

fn parse_tuic(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let uuid = get_str(map, &["uuid"]).ok_or_else(|| "tuic: missing uuid".to_string())?;
    let mut tls = parse_tls(map, true).unwrap_or(TlsConfig {
        enabled: true,
        ..Default::default()
    });
    tls.enabled = true;
    Ok((
        Some(tls),
        None,
        ProtocolConfig::Tuic {
            uuid,
            password: get_str(map, &["password"]).unwrap_or_default(),
            congestion_control: get_str(map, &["congestion_control"]),
            udp_relay_mode: get_str(map, &["udp_relay_mode"]),
            zero_rtt_handshake: get_bool(map, &["zero_rtt_handshake"]).unwrap_or(false),
        },
    ))
}

fn parse_socks5(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    Ok((
        parse_tls(map, false),
        None,
        ProtocolConfig::Socks5 {
            username: get_str(map, &["username", "user"]),
            password: get_str(map, &["password"]),
        },
    ))
}

fn parse_http(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    Ok((
        parse_tls(map, false),
        None,
        ProtocolConfig::Http {
            username: get_str(map, &["username", "user"]),
            password: get_str(map, &["password"]),
            path: get_str(map, &["path"]),
        },
    ))
}

fn parse_hysteria(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let auth_str = get_str(map, &["auth_str", "auth-str"]);
    let auth_base64 = auth_str.is_none();
    let auth = auth_str
        .or_else(|| get_str(map, &["auth"]))
        .ok_or_else(|| "hysteria: missing auth".to_string())?;
    let mut tls = parse_tls(map, true).unwrap_or(TlsConfig {
        enabled: true,
        ..Default::default()
    });
    tls.enabled = true;
    Ok((
        Some(tls),
        None,
        ProtocolConfig::Hysteria {
            auth,
            auth_base64,
            up_mbps: get_u32(map, &["up_mbps", "up"]),
            down_mbps: get_u32(map, &["down_mbps", "down"]),
            obfs: get_str(map, &["obfs"]),
        },
    ))
}

fn parse_shadowtls(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let version = get_u8(map, &["version"]).unwrap_or(3);
    if !(1..=3).contains(&version) {
        return Err("shadowtls: version must be 1, 2, or 3".into());
    }
    let mut tls = parse_tls(map, true).unwrap_or(TlsConfig {
        enabled: true,
        ..Default::default()
    });
    tls.enabled = true;
    Ok((
        Some(tls),
        None,
        ProtocolConfig::ShadowTls {
            version,
            password: get_str(map, &["password"]),
        },
    ))
}

fn parse_ssh(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let user = get_str(map, &["user", "username"]).unwrap_or_else(|| "root".into());
    let password = get_str(map, &["password"]);
    let private_key = get_str(map, &["private_key", "private-key"]);
    if password.is_none() && private_key.is_none() {
        return Err("ssh: missing password or private_key".into());
    }
    Ok((
        None,
        None,
        ProtocolConfig::Ssh {
            user,
            password,
            private_key,
            private_key_passphrase: get_str(map, &["private_key_passphrase"]),
            host_key: get_str_list(map, &["host_key"]).unwrap_or_default(),
        },
    ))
}

fn parse_naive(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    Ok((
        parse_tls(map, true),
        None,
        ProtocolConfig::Naive {
            username: get_str(map, &["username"])
                .ok_or_else(|| "naive: missing username".to_string())?,
            password: get_str(map, &["password"])
                .ok_or_else(|| "naive: missing password".to_string())?,
            quic: get_bool(map, &["quic"]).unwrap_or(false),
        },
    ))
}

fn parse_tor(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    Ok((
        None,
        None,
        ProtocolConfig::Tor {
            executable_path: get_str(map, &["executable_path"])
                .ok_or_else(|| "tor: missing executable_path".to_string())?,
            extra_args: get_str_list(map, &["extra_args"]).unwrap_or_default(),
            data_directory: get_str(map, &["data_directory"]),
        },
    ))
}

fn parse_wireguard(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let private_key = get_str(map, &["private_key"])
        .ok_or_else(|| "wireguard: missing private_key".to_string())?;
    let peer = first_peer(map);
    let peer_public_key = get_str(map, &["peer_public_key", "public_key"])
        .or_else(|| peer.and_then(|p| get_str(p, &["public_key"])))
        .ok_or_else(|| "wireguard: missing peer public key".to_string())?;
    let local_address = get_str_list(map, &["local_address", "address", "ip"])
        .ok_or_else(|| "wireguard: missing local_address".to_string())?;
    let reserved = map
        .get("reserved")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(|x| x.as_u64().and_then(|n| u8::try_from(n).ok()))
                .collect()
        })
        .or_else(|| {
            peer.and_then(|p| p.get("reserved"))
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_u64().and_then(|n| u8::try_from(n).ok()))
                        .collect()
                })
        })
        .unwrap_or_default();
    Ok((
        None,
        None,
        ProtocolConfig::WireGuard {
            local_address,
            private_key,
            peer_public_key,
            pre_shared_key: get_str(map, &["pre_shared_key"])
                .or_else(|| peer.and_then(|p| get_str(p, &["pre_shared_key"]))),
            reserved,
            mtu: get_u32(map, &["mtu"]),
        },
    ))
}

fn parse_anytls(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let password =
        get_str(map, &["password"]).ok_or_else(|| "anytls: missing password".to_string())?;
    let mut tls = parse_tls(map, true).unwrap_or(TlsConfig {
        enabled: true,
        ..Default::default()
    });
    tls.enabled = true;
    Ok((Some(tls), None, ProtocolConfig::AnyTls { password }))
}

fn parse_snell(
    map: &Map<String, Value>,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let psk = get_str(map, &["psk", "password"]).ok_or_else(|| "snell: missing psk".to_string())?;
    Ok((
        None,
        None,
        ProtocolConfig::Snell {
            psk,
            version: get_u8(map, &["version"]).unwrap_or(4),
            userkey: get_str(map, &["userkey"]),
            reuse: get_bool(map, &["reuse"]),
            obfs_mode: get_str(map, &["obfs_mode", "obfs"]),
            obfs_host: get_str(map, &["obfs_host"]),
            mode: get_str(map, &["mode"]),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ProtocolConfig;

    #[test]
    fn parse_full_config_skips_groups() {
        let json = r#"{
          "log": { "level": "info" },
          "outbounds": [
            {
              "type": "vless",
              "tag": "VLESS-1",
              "server": "vl.example.com",
              "server_port": 443,
              "uuid": "22222222-2222-2222-2222-222222222222",
              "tls": {
                "enabled": true,
                "server_name": "www.microsoft.com",
                "utls": { "enabled": true, "fingerprint": "chrome" },
                "reality": { "enabled": true, "public_key": "pubkey123", "short_id": "abcd" }
              }
            },
            { "type": "selector", "tag": "proxy", "outbounds": ["VLESS-1"] },
            { "type": "direct", "tag": "direct" }
          ]
        }"#;
        let r = parse_singbox_json(json).unwrap();
        assert_eq!(r.nodes.len(), 1);
        assert_eq!(r.skipped.len(), 2);
        assert_eq!(r.nodes[0].name, "VLESS-1");
        assert_eq!(r.nodes[0].protocol, Protocol::Vless);
        assert_eq!(
            r.nodes[0]
                .tls
                .as_ref()
                .unwrap()
                .reality_public_key
                .as_deref(),
            Some("pubkey123")
        );
    }

    #[test]
    fn parse_outbounds_array() {
        let json = r#"[
          {
            "type": "shadowsocks",
            "tag": "SS-HK",
            "server": "ss.example.com",
            "server_port": 8388,
            "method": "aes-256-gcm",
            "password": "secret"
          }
        ]"#;
        let r = parse_singbox_json(json).unwrap();
        assert_eq!(r.nodes.len(), 1);
        assert!(matches!(
            r.nodes[0].config,
            ProtocolConfig::Shadowsocks { .. }
        ));
    }

    #[test]
    fn complete_config_requires_inbounds() {
        let only_outbounds = r#"{"outbounds":[{"type":"direct","tag":"direct"}]}"#;
        assert!(validate_complete_singbox_config(only_outbounds).is_err());
        let full = r#"{
          "inbounds":[{"type":"mixed","tag":"mixed-in","listen":"127.0.0.1","listen_port":2080}],
          "outbounds":[{"type":"direct","tag":"direct"}]
        }"#;
        assert!(validate_complete_singbox_config(full).is_ok());
    }

    #[test]
    fn parse_single_outbound() {
        let json = r#"{
          "type": "trojan",
          "tag": "TJ",
          "server": "tj.example.com",
          "server_port": 443,
          "password": "tjpass",
          "tls": { "enabled": true, "server_name": "tj.example.com" }
        }"#;
        let r = parse_singbox_json(json).unwrap();
        assert_eq!(r.nodes.len(), 1);
        assert_eq!(r.nodes[0].server, "tj.example.com");
        assert_eq!(r.nodes[0].port, 443);
    }
}
