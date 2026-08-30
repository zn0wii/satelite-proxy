//! Build / flatten a single proxy node from the add-config form.

use crate::domain::{
    ManualNodeDraft, ParseResult, Protocol, ProtocolConfig, ProxyNode, ShadowTlsOpts,
    SubscriptionFormat, TlsConfig, Transport,
};
use crate::error::{AppError, AppResult};
use std::collections::BTreeMap;

fn opt_nonempty(s: &Option<String>) -> Option<String> {
    s.as_ref()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn split_csv(s: &str) -> Vec<String> {
    s.split([',', '\n', ';'])
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

pub fn draft_to_node(
    draft: &ManualNodeDraft,
    fallback_name: Option<&str>,
) -> Result<ProxyNode, String> {
    let protocol = Protocol::from_clash_type(&draft.protocol)
        .ok_or_else(|| format!("unsupported protocol: {}", draft.protocol))?;
    let server = draft.server.trim().to_string();
    if !matches!(protocol, Protocol::Tor) && server.is_empty() {
        return Err("server is required".into());
    }
    if !matches!(protocol, Protocol::Tor) && draft.port == 0 {
        return Err("port is required".into());
    }
    let name = opt_nonempty(&draft.name)
        .or_else(|| {
            fallback_name
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| {
            if server.is_empty() {
                protocol.as_str().to_string()
            } else {
                format!("{}-{}-{}", protocol.as_str(), server, draft.port)
            }
        });

    let tls = build_tls(draft, protocol);
    let transport = build_transport(draft, protocol);
    let config = build_config(draft, protocol)?;

    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol,
        server: if server.is_empty() {
            "localhost".into()
        } else {
            server
        },
        port: draft.port,
        tls,
        transport,
        udp: draft.udp,
        config,
        source: Some("manual".into()),
        latency_ms: None,
        latency_at: None,
    }
    .with_computed_id())
}

pub fn parse_manual_draft(
    draft: &ManualNodeDraft,
    fallback_name: Option<&str>,
) -> AppResult<ParseResult> {
    match draft_to_node(draft, fallback_name) {
        Ok(node) => Ok(ParseResult {
            nodes: vec![node],
            skipped: Vec::new(),
            format: SubscriptionFormat::Manual,
        }),
        Err(reason) => {
            if reason.contains("unsupported") {
                Err(AppError::UnsupportedProxyType(draft.protocol.clone()))
            } else {
                Err(AppError::InvalidProxy {
                    name: draft.name.clone().unwrap_or_else(|| "node".into()),
                    reason,
                })
            }
        }
    }
}

pub fn parse_single_uri(uri: &str, name_override: Option<&str>) -> AppResult<ParseResult> {
    let uri = uri.trim();
    if uri.is_empty() {
        return Err(AppError::EmptySubscription);
    }
    // Allow a single URI or a short list — still stored as a "node" profile.
    let mut parsed = crate::subscription::parse_uri_list(uri, SubscriptionFormat::UriList)?;
    if let Some(name) = name_override.map(str::trim).filter(|s| !s.is_empty()) {
        if parsed.nodes.len() == 1 {
            parsed.nodes[0].name = name.to_string();
            parsed.nodes[0] = parsed.nodes[0].clone().with_computed_id();
        }
    }
    Ok(parsed)
}

fn tls_wanted(protocol: Protocol) -> bool {
    matches!(
        protocol,
        Protocol::Vless
            | Protocol::Vmess
            | Protocol::Trojan
            | Protocol::Hysteria2
            | Protocol::Tuic
            | Protocol::Http
            | Protocol::Hysteria
            | Protocol::AnyTls
            | Protocol::ShadowTls
            | Protocol::Naive
            | Protocol::Socks5
    )
}

fn tls_default_on(protocol: Protocol) -> bool {
    matches!(
        protocol,
        Protocol::Vless
            | Protocol::Trojan
            | Protocol::Hysteria2
            | Protocol::Tuic
            | Protocol::Hysteria
            | Protocol::AnyTls
            | Protocol::ShadowTls
            | Protocol::Naive
    )
}

fn build_tls(draft: &ManualNodeDraft, protocol: Protocol) -> Option<TlsConfig> {
    if !tls_wanted(protocol) {
        return None;
    }
    let enabled = draft.tls.unwrap_or_else(|| {
        tls_default_on(protocol)
            || opt_nonempty(&draft.sni).is_some()
            || opt_nonempty(&draft.reality_public_key).is_some()
    });
    if !enabled {
        return None;
    }
    let alpn = opt_nonempty(&draft.alpn).map(|s| split_csv(&s));
    Some(TlsConfig {
        enabled: true,
        server_name: opt_nonempty(&draft.sni),
        insecure: draft.insecure,
        alpn,
        utls_fingerprint: opt_nonempty(&draft.fingerprint),
        reality_public_key: opt_nonempty(&draft.reality_public_key),
        reality_short_id: opt_nonempty(&draft.reality_short_id),
    })
}

fn transport_wanted(protocol: Protocol) -> bool {
    matches!(
        protocol,
        Protocol::Vless | Protocol::Vmess | Protocol::Trojan | Protocol::Shadowsocks
    )
}

fn build_transport(draft: &ManualNodeDraft, protocol: Protocol) -> Option<Transport> {
    if !transport_wanted(protocol) {
        return None;
    }
    let network = draft
        .network
        .as_deref()
        .unwrap_or("tcp")
        .trim()
        .to_ascii_lowercase();
    match network.as_str() {
        "ws" | "websocket" => {
            let host = opt_nonempty(&draft.host);
            let headers = host.map(|h| {
                let mut m = BTreeMap::new();
                m.insert("Host".into(), h);
                m
            });
            Some(Transport::Ws {
                path: opt_nonempty(&draft.path),
                headers,
                max_early_data: None,
            })
        }
        "grpc" => Some(Transport::Grpc {
            service_name: opt_nonempty(&draft.service_name),
        }),
        "http" | "h2" => Some(Transport::Http {
            path: opt_nonempty(&draft.path),
            host: opt_nonempty(&draft.host).map(|h| split_csv(&h)),
        }),
        "httpupgrade" | "http-upgrade" => Some(Transport::HttpUpgrade {
            path: opt_nonempty(&draft.path),
            host: opt_nonempty(&draft.host),
        }),
        // Xray-only: such nodes only work via multi-core Xray delegation
        // (the manual form's hint says so). No mode field in the draft —
        // Xray defaults to "auto".
        "xhttp" | "splithttp" => Some(Transport::Xhttp {
            path: opt_nonempty(&draft.path),
            host: opt_nonempty(&draft.host),
            mode: None,
        }),
        _ => Some(Transport::Tcp),
    }
}

fn req(s: &Option<String>, field: &str) -> Result<String, String> {
    opt_nonempty(s).ok_or_else(|| format!("missing {field}"))
}

/// Parse the `host=..;password=..;version=..;fingerprint=..` string the edit
/// form shows for a shadow-tls plugin (the format `node_to_draft` renders)
/// back into structured opts. Mirrors the clash parser's defaults: version 3,
/// host and password required.
fn parse_shadow_tls_opts(opts: Option<&str>) -> Result<ShadowTlsOpts, String> {
    let raw = opts.ok_or("ss: shadow-tls missing plugin_opts (host/password)")?;
    let mut host = None;
    let mut password = None;
    let mut version = 3u8;
    let mut fingerprint = None;
    for pair in raw.split(';') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| "ss: shadow-tls plugin_opts must be k=v pairs".to_string())?;
        let value = value.trim();
        match key.trim() {
            "host" => host = Some(value.to_string()),
            "password" => password = Some(value.to_string()),
            "version" => {
                version = value
                    .parse()
                    .map_err(|_| format!("ss: shadow-tls invalid version {value}"))?;
            }
            "fingerprint" => fingerprint = Some(value.to_string()),
            _ => {}
        }
    }
    if !(1..=3).contains(&version) {
        return Err(format!(
            "ss: shadow-tls version must be 1, 2, or 3 (got {version})"
        ));
    }
    Ok(ShadowTlsOpts {
        host: host.ok_or("ss: shadow-tls missing host")?,
        password: password.ok_or("ss: shadow-tls missing password")?,
        version,
        fingerprint: fingerprint.filter(|f| !f.is_empty()),
    })
}

fn build_config(draft: &ManualNodeDraft, protocol: Protocol) -> Result<ProtocolConfig, String> {
    match protocol {
        Protocol::Shadowsocks => {
            let plugin = opt_nonempty(&draft.plugin);
            let plugin_opts = opt_nonempty(&draft.plugin_opts);
            // sing-box has no SIP003 form for shadow-tls (see clash.rs): when
            // the form names that plugin, lift the opts string into the
            // dedicated field so the builder emits the shadowtls detour.
            let (plugin, plugin_opts, shadow_tls) = if plugin.as_deref() == Some("shadow-tls") {
                let st = parse_shadow_tls_opts(plugin_opts.as_deref())?;
                (Some("shadow-tls".into()), None, Some(st))
            } else {
                (plugin, plugin_opts, None)
            };
            Ok(ProtocolConfig::Shadowsocks {
                method: opt_nonempty(&draft.method).unwrap_or_else(|| "aes-256-gcm".into()),
                password: req(&draft.password, "password")?,
                plugin,
                plugin_opts,
                shadow_tls,
            })
        }
        Protocol::Vmess => Ok(ProtocolConfig::Vmess {
            uuid: req(&draft.uuid, "uuid")?,
            alter_id: draft.alter_id.unwrap_or(0),
            security: opt_nonempty(&draft.security).unwrap_or_else(|| "auto".into()),
        }),
        Protocol::Vless => Ok(ProtocolConfig::Vless {
            uuid: req(&draft.uuid, "uuid")?,
            flow: opt_nonempty(&draft.flow),
            packet_encoding: opt_nonempty(&draft.packet_encoding).unwrap_or_else(|| "xudp".into()),
        }),
        Protocol::Trojan => Ok(ProtocolConfig::Trojan {
            password: req(&draft.password, "password")?,
        }),
        Protocol::Hysteria2 => Ok(ProtocolConfig::Hysteria2 {
            password: req(&draft.password, "password")?,
            up_mbps: draft.up_mbps,
            down_mbps: draft.down_mbps,
            obfs: opt_nonempty(&draft.obfs),
            obfs_password: opt_nonempty(&draft.obfs_password),
        }),
        Protocol::Tuic => Ok(ProtocolConfig::Tuic {
            uuid: req(&draft.uuid, "uuid")?,
            password: opt_nonempty(&draft.password).unwrap_or_default(),
            congestion_control: opt_nonempty(&draft.congestion_control),
            udp_relay_mode: opt_nonempty(&draft.udp_relay_mode),
            zero_rtt_handshake: draft.zero_rtt_handshake.unwrap_or(false),
        }),
        Protocol::Socks5 => Ok(ProtocolConfig::Socks5 {
            username: opt_nonempty(&draft.username),
            password: opt_nonempty(&draft.password),
        }),
        Protocol::Http => Ok(ProtocolConfig::Http {
            username: opt_nonempty(&draft.username),
            password: opt_nonempty(&draft.password),
            path: opt_nonempty(&draft.path),
        }),
        Protocol::Hysteria => Ok(ProtocolConfig::Hysteria {
            auth: req(&draft.password, "auth")?,
            auth_base64: false,
            up_mbps: draft.up_mbps,
            down_mbps: draft.down_mbps,
            obfs: opt_nonempty(&draft.obfs),
        }),
        Protocol::ShadowTls => {
            let version = draft.version.unwrap_or(3);
            if !(1..=3).contains(&version) {
                return Err("shadowtls: version must be 1, 2, or 3".into());
            }
            Ok(ProtocolConfig::ShadowTls {
                version,
                password: opt_nonempty(&draft.password),
            })
        }
        Protocol::Ssh => {
            let password = opt_nonempty(&draft.password);
            let private_key = opt_nonempty(&draft.private_key);
            if password.is_none() && private_key.is_none() {
                return Err("ssh: missing password or private key".into());
            }
            Ok(ProtocolConfig::Ssh {
                user: opt_nonempty(&draft.user)
                    .or_else(|| opt_nonempty(&draft.username))
                    .unwrap_or_else(|| "root".into()),
                password,
                private_key,
                private_key_passphrase: opt_nonempty(&draft.private_key_passphrase),
                host_key: Vec::new(),
            })
        }
        Protocol::Naive => Ok(ProtocolConfig::Naive {
            username: req(&draft.username, "username")?,
            password: req(&draft.password, "password")?,
            quic: draft.quic.unwrap_or(false),
        }),
        Protocol::Tor => Ok(ProtocolConfig::Tor {
            executable_path: req(&draft.executable_path, "executable_path")?,
            extra_args: Vec::new(),
            data_directory: None,
        }),
        Protocol::WireGuard => {
            let local = opt_nonempty(&draft.local_address)
                .map(|s| split_csv(&s))
                .filter(|v| !v.is_empty())
                .ok_or_else(|| "wireguard: missing local address".to_string())?;
            Ok(ProtocolConfig::WireGuard {
                local_address: local,
                private_key: req(&draft.private_key, "private_key")?,
                peer_public_key: req(&draft.peer_public_key, "peer_public_key")?,
                pre_shared_key: opt_nonempty(&draft.pre_shared_key),
                reserved: Vec::new(),
                mtu: draft.mtu,
            })
        }
        Protocol::AnyTls => Ok(ProtocolConfig::AnyTls {
            password: req(&draft.password, "password")?,
        }),
        Protocol::Snell => Ok(ProtocolConfig::Snell {
            psk: opt_nonempty(&draft.psk)
                .or_else(|| opt_nonempty(&draft.password))
                .ok_or_else(|| "snell: missing psk".to_string())?,
            version: draft.version.unwrap_or(4),
            userkey: None,
            reuse: None,
            obfs_mode: opt_nonempty(&draft.obfs),
            obfs_host: opt_nonempty(&draft.host),
            mode: None,
        }),
    }
}

pub fn node_to_draft(node: &ProxyNode) -> ManualNodeDraft {
    let mut draft = ManualNodeDraft {
        protocol: node.protocol.as_str().to_string(),
        server: node.server.clone(),
        port: node.port,
        name: Some(node.name.clone()),
        udp: node.udp,
        ..ManualNodeDraft::default()
    };
    if let Some(tls) = &node.tls {
        draft.tls = Some(tls.enabled);
        draft.sni = tls.server_name.clone();
        draft.insecure = tls.insecure;
        draft.alpn = tls.alpn.as_ref().map(|v| v.join(","));
        draft.fingerprint = tls.utls_fingerprint.clone();
        draft.reality_public_key = tls.reality_public_key.clone();
        draft.reality_short_id = tls.reality_short_id.clone();
    }
    match &node.transport {
        Some(Transport::Ws { path, headers, .. }) => {
            draft.network = Some("ws".into());
            draft.path = path.clone();
            draft.host = headers
                .as_ref()
                .and_then(|h| h.get("Host").or_else(|| h.get("host")).cloned());
        }
        Some(Transport::Grpc { service_name }) => {
            draft.network = Some("grpc".into());
            draft.service_name = service_name.clone();
        }
        Some(Transport::Http { path, host }) => {
            draft.network = Some("http".into());
            draft.path = path.clone();
            draft.host = host.as_ref().map(|h| h.join(","));
        }
        Some(Transport::HttpUpgrade { path, host }) => {
            draft.network = Some("httpupgrade".into());
            draft.path = path.clone();
            draft.host = host.clone();
        }
        Some(Transport::Xhttp { path, host, .. }) => {
            draft.network = Some("xhttp".into());
            draft.path = path.clone();
            draft.host = host.clone();
        }
        Some(Transport::Tcp) | None => {
            draft.network = Some("tcp".into());
        }
    }
    match &node.config {
        ProtocolConfig::Shadowsocks {
            method,
            password,
            plugin,
            plugin_opts,
            shadow_tls,
        } => {
            draft.method = Some(method.clone());
            draft.password = Some(password.clone());
            if let Some(st) = shadow_tls {
                // Mirror the structured opts back into the SIP003-style
                // strings the form edits; build_config parses them out again.
                let mut opts = format!(
                    "host={};password={};version={}",
                    st.host, st.password, st.version
                );
                if let Some(fp) = &st.fingerprint {
                    opts.push_str(&format!(";fingerprint={fp}"));
                }
                draft.plugin = Some("shadow-tls".into());
                draft.plugin_opts = Some(opts);
            } else {
                draft.plugin = plugin.clone();
                draft.plugin_opts = plugin_opts.clone();
            }
        }
        ProtocolConfig::Vmess {
            uuid,
            alter_id,
            security,
        } => {
            draft.uuid = Some(uuid.clone());
            draft.alter_id = Some(*alter_id);
            draft.security = Some(security.clone());
        }
        ProtocolConfig::Vless {
            uuid,
            flow,
            packet_encoding,
        } => {
            draft.uuid = Some(uuid.clone());
            draft.flow = flow.clone();
            draft.packet_encoding = Some(packet_encoding.clone());
        }
        ProtocolConfig::Trojan { password } => {
            draft.password = Some(password.clone());
        }
        ProtocolConfig::Hysteria2 {
            password,
            up_mbps,
            down_mbps,
            obfs,
            obfs_password,
        } => {
            draft.password = Some(password.clone());
            draft.up_mbps = *up_mbps;
            draft.down_mbps = *down_mbps;
            draft.obfs = obfs.clone();
            draft.obfs_password = obfs_password.clone();
        }
        ProtocolConfig::Tuic {
            uuid,
            password,
            congestion_control,
            udp_relay_mode,
            zero_rtt_handshake,
        } => {
            draft.uuid = Some(uuid.clone());
            draft.password = Some(password.clone());
            draft.congestion_control = congestion_control.clone();
            draft.udp_relay_mode = udp_relay_mode.clone();
            draft.zero_rtt_handshake = Some(*zero_rtt_handshake);
        }
        ProtocolConfig::Socks5 { username, password } => {
            draft.username = username.clone();
            draft.password = password.clone();
        }
        ProtocolConfig::Http {
            username,
            password,
            path,
        } => {
            draft.username = username.clone();
            draft.password = password.clone();
            if draft.path.is_none() {
                draft.path = path.clone();
            }
        }
        ProtocolConfig::Hysteria {
            auth,
            up_mbps,
            down_mbps,
            obfs,
            ..
        } => {
            draft.password = Some(auth.clone());
            draft.up_mbps = *up_mbps;
            draft.down_mbps = *down_mbps;
            draft.obfs = obfs.clone();
        }
        ProtocolConfig::ShadowTls { version, password } => {
            draft.version = Some(*version);
            draft.password = password.clone();
        }
        ProtocolConfig::Ssh {
            user,
            password,
            private_key,
            private_key_passphrase,
            ..
        } => {
            draft.user = Some(user.clone());
            draft.password = password.clone();
            draft.private_key = private_key.clone();
            draft.private_key_passphrase = private_key_passphrase.clone();
        }
        ProtocolConfig::Naive {
            username,
            password,
            quic,
        } => {
            draft.username = Some(username.clone());
            draft.password = Some(password.clone());
            draft.quic = Some(*quic);
        }
        ProtocolConfig::Tor {
            executable_path, ..
        } => {
            draft.executable_path = Some(executable_path.clone());
        }
        ProtocolConfig::WireGuard {
            local_address,
            private_key,
            peer_public_key,
            pre_shared_key,
            mtu,
            ..
        } => {
            draft.local_address = Some(local_address.join(","));
            draft.private_key = Some(private_key.clone());
            draft.peer_public_key = Some(peer_public_key.clone());
            draft.pre_shared_key = pre_shared_key.clone();
            draft.mtu = *mtu;
        }
        ProtocolConfig::AnyTls { password } => {
            draft.password = Some(password.clone());
        }
        ProtocolConfig::Snell {
            psk,
            version,
            obfs_mode,
            obfs_host,
            ..
        } => {
            draft.psk = Some(psk.clone());
            draft.password = Some(psk.clone());
            draft.version = Some(*version);
            draft.obfs = obfs_mode.clone();
            if draft.host.is_none() {
                draft.host = obfs_host.clone();
            }
        }
    }
    draft
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ss_shadow_tls_plugin_roundtrip_through_structured_opts() {
        let draft = ManualNodeDraft {
            protocol: "shadowsocks".into(),
            server: "ss.example.com".into(),
            port: 8388,
            method: Some("aes-256-gcm".into()),
            password: Some("secret".into()),
            plugin: Some("shadow-tls".into()),
            plugin_opts: Some(
                "host=www.bing.com;password=tls-pass;version=3;fingerprint=chrome".into(),
            ),
            ..Default::default()
        };
        let node = draft_to_node(&draft, None).unwrap();
        let ProtocolConfig::Shadowsocks { shadow_tls, .. } = &node.config else {
            panic!("expected shadowsocks config");
        };
        let st = shadow_tls.as_ref().expect("structured shadow_tls opts");
        assert_eq!(st.host, "www.bing.com");
        assert_eq!(st.password, "tls-pass");
        assert_eq!(st.version, 3);
        assert_eq!(st.fingerprint.as_deref(), Some("chrome"));

        // The edit form renders the structured opts back as the same strings.
        let back = node_to_draft(&node);
        assert_eq!(back.plugin.as_deref(), Some("shadow-tls"));
        assert_eq!(
            back.plugin_opts.as_deref(),
            Some("host=www.bing.com;password=tls-pass;version=3;fingerprint=chrome")
        );
    }

    #[test]
    fn ss_shadow_tls_plugin_requires_host_and_password() {
        let draft = ManualNodeDraft {
            protocol: "shadowsocks".into(),
            server: "ss.example.com".into(),
            port: 8388,
            method: Some("aes-256-gcm".into()),
            password: Some("secret".into()),
            plugin: Some("shadow-tls".into()),
            plugin_opts: Some("version=3".into()),
            ..Default::default()
        };
        let err = draft_to_node(&draft, None).unwrap_err().to_string();
        assert!(err.contains("shadow-tls missing host"), "err: {err}");
    }

    #[test]
    fn vless_form_roundtrip() {
        let draft = ManualNodeDraft {
            protocol: "vless".into(),
            server: "vl.example.com".into(),
            port: 443,
            name: Some("V1".into()),
            uuid: Some("22222222-2222-2222-2222-222222222222".into()),
            flow: Some("xtls-rprx-vision".into()),
            tls: Some(true),
            sni: Some("www.microsoft.com".into()),
            fingerprint: Some("chrome".into()),
            reality_public_key: Some("pubkey123".into()),
            reality_short_id: Some("abcd".into()),
            network: Some("tcp".into()),
            ..Default::default()
        };
        let node = draft_to_node(&draft, None).unwrap();
        assert_eq!(node.name, "V1");
        assert_eq!(node.protocol, Protocol::Vless);
        let back = node_to_draft(&node);
        assert_eq!(
            back.uuid.as_deref(),
            Some("22222222-2222-2222-2222-222222222222")
        );
        assert_eq!(back.sni.as_deref(), Some("www.microsoft.com"));
        assert_eq!(back.reality_public_key.as_deref(), Some("pubkey123"));
    }

    #[test]
    fn ss_requires_password() {
        let draft = ManualNodeDraft {
            protocol: "shadowsocks".into(),
            server: "ss.example.com".into(),
            port: 8388,
            method: Some("aes-256-gcm".into()),
            ..Default::default()
        };
        assert!(draft_to_node(&draft, None).is_err());
    }
}
