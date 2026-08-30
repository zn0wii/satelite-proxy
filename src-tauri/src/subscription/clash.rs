//! Parse Clash YAML `proxies:` list into normalized [`ProxyNode`]s.

use crate::domain::{
    ParseResult, Protocol, ProtocolConfig, ProxyNode, ShadowTlsOpts, SkippedProxy,
    SubscriptionFormat, TlsConfig, Transport,
};
use crate::error::{AppError, AppResult};
use crate::subscription::yaml_util::{
    as_mapping, get_bool, get_map, get_str, get_str_list, get_u16, get_u32, map_to_string_map,
    value_to_string,
};
use serde::Deserialize as _;
use serde_yaml::Value;

/// Parse a full Clash config document or a bare proxies list.
///
/// Some providers concatenate several Clash documents into one payload
/// (separated by `---`). `serde_yaml::from_str` rejects multi-document input,
/// so iterate every document with the `Deserializer` and merge all `proxies`
/// lists in document order.
pub fn parse_clash_yaml(content: &str) -> AppResult<ParseResult> {
    let content = content.trim();
    if content.is_empty() {
        return Err(AppError::EmptySubscription);
    }

    let mut proxies: Vec<Value> = Vec::new();
    for document in serde_yaml::Deserializer::from_str(content) {
        let root = Value::deserialize(document)
            .map_err(|e| AppError::SubscriptionParse(format!("invalid yaml: {e}")))?;
        if let Some(list) = extract_proxies_seq(&root) {
            proxies.extend(list.iter().cloned());
        }
    }

    if proxies.is_empty() {
        return Err(AppError::SubscriptionParse(
            "no `proxies` list found in clash yaml".into(),
        ));
    }
    crate::subscription::ensure_entry_limit(proxies.len())?;

    let mut nodes = Vec::new();
    let mut skipped = Vec::new();

    for (idx, item) in proxies.iter().enumerate() {
        match parse_proxy_entry(item) {
            Ok(node) => nodes.push(node.with_computed_id()),
            Err(reason) => {
                let name = as_mapping(item)
                    .and_then(|m| get_str(m, &["name"]))
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
        format: SubscriptionFormat::ClashYaml,
    })
}

fn extract_proxies_seq(root: &Value) -> Option<&Vec<Value>> {
    match root {
        Value::Sequence(seq) => Some(seq),
        Value::Mapping(map) => map
            .get(Value::String("proxies".into()))
            .and_then(|v| v.as_sequence()),
        _ => None,
    }
}

fn parse_proxy_entry(value: &Value) -> Result<ProxyNode, String> {
    let map = as_mapping(value).ok_or_else(|| "proxy entry is not a map".to_string())?;

    let name = get_str(map, &["name"]).unwrap_or_else(|| "unnamed".into());
    let type_str = get_str(map, &["type"]).ok_or_else(|| "missing type".to_string())?;
    let protocol = Protocol::from_clash_type(&type_str)
        .ok_or_else(|| format!("unsupported type: {type_str}"))?;
    let server = if matches!(protocol, Protocol::Tor) {
        get_str(map, &["server"]).unwrap_or_else(|| "localhost".into())
    } else {
        get_str(map, &["server"]).ok_or_else(|| "missing server".to_string())?
    };
    let port = if matches!(protocol, Protocol::Tor) {
        get_u16(map, &["port"]).unwrap_or(0)
    } else {
        get_u16(map, &["port"]).ok_or_else(|| "missing or invalid port".to_string())?
    };

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
    };

    Ok(ProxyNode {
        id: String::new(),
        name,
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
    })
}

fn parse_ss(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let method =
        get_str(map, &["cipher", "method"]).ok_or_else(|| "ss: missing cipher".to_string())?;
    let password = get_str(map, &["password"]).ok_or_else(|| "ss: missing password".to_string())?;

    // Clash names the obfs SIP003 plugin "obfs"; sing-box's built-in registry
    // keys it as "obfs-local" (transport/sip003/plugin.go RegisterPlugin
    // calls). Passing "obfs" through verbatim causes sing-box to fail with
    // "plugin not found: obfs". v2ray-plugin's name is shared by both.
    let plugin = get_str(map, &["plugin"]).map(|p| match p.as_str() {
        "obfs" => "obfs-local".to_string(),
        _ => p,
    });
    // shadow-tls has no equivalent in sing-box's SIP003 plugin_opts grammar:
    // sing-box models it as a separate `shadowtls` outbound that the ss
    // outbound detours through, not as an arg string on the ss outbound
    // itself (see xmdhs/clash2singbox's shadowTls()). Parse it into a
    // dedicated field instead of falling through to the generic key=value
    // join, which would silently produce a node that looks converted but
    // can't actually connect.
    let shadow_tls = if plugin.as_deref() == Some("shadow-tls") {
        let opts = get_map(map, &["plugin-opts", "plugin_opts"])
            .ok_or_else(|| "ss: shadow-tls missing plugin-opts".to_string())?;
        let host =
            get_str(opts, &["host"]).ok_or_else(|| "ss: shadow-tls missing host".to_string())?;
        let tls_password = get_str(opts, &["password"])
            .ok_or_else(|| "ss: shadow-tls missing password".to_string())?;
        // mihomo defaults shadow-tls to protocol version 3 when unset.
        let version = get_u16(opts, &["version"]).map(|v| v as u8).unwrap_or(3);
        let fingerprint = get_str(map, &["client-fingerprint", "client_fingerprint"])
            .and_then(|s| normalize_utls_fingerprint(&s));
        Some(ShadowTlsOpts {
            host,
            password: tls_password,
            version,
            fingerprint,
        })
    } else {
        None
    };
    let plugin_opts = if shadow_tls.is_some() {
        None
    } else {
        get_map(map, &["plugin-opts", "plugin_opts"])
            .map(|m| build_plugin_opts(plugin.as_deref(), m))
    };

    Ok((
        None,
        None,
        ProtocolConfig::Shadowsocks {
            method,
            password,
            plugin,
            plugin_opts,
            shadow_tls,
        },
    ))
}

/// Build a SIP003 `plugin_opts` string for sing-box from Clash's nested
/// `plugin-opts` map.
///
/// Clash/mihomo models plugin options as typed struct fields (bool/int/string)
/// decoded from YAML, then re-serializes loosely for its own plugin
/// implementations. sing-box's built-in `obfs-local` and `v2ray-plugin`
/// instead parse a flat SIP003 arg string (`transport/sip003/args.go`,
/// ported from the reference parser in shadowsocks/v2ray-plugin's own
/// `args.go`) where each flag is either `key=value` or a bare `key` — a bare
/// key is stored as value `"1"` by the parser itself, which is also why
/// sing-box's `tls`/`mux` checks only care whether the key is present, not
/// what (if anything) follows `=`. `tls=false` and `tls=true` are therefore
/// BOTH "enabled" to sing-box. Naively joining Clash's typed values as
/// `key=value` produces strings sing-box misinterprets rather than rejects,
/// which is worse than a parse error. This maps each known key explicitly
/// instead of doing a blind key=value join, and backslash-escapes `;`, `=`,
/// and `\` in values per the same reference parser (unescaped occurrences
/// would silently split the string into the wrong fields).
fn build_plugin_opts(plugin: Option<&str>, m: &serde_yaml::Mapping) -> String {
    let get = |key: &str| -> Option<String> {
        m.get(Value::String(key.into())).and_then(value_to_string)
    };
    let get_flag = |key: &str| -> Option<bool> {
        match m.get(Value::String(key.into())) {
            Some(Value::Bool(b)) => Some(*b),
            Some(v) => value_to_string(v).and_then(|s| match s.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => Some(true),
                "false" | "0" | "no" => Some(false),
                _ => None,
            }),
            None => None,
        }
    };
    // Backslash-escape '=', ';', '\' — SIP003's args grammar requires it
    // (shadowsocks/v2ray-plugin args.go: backslashEscape) since those bytes
    // are the field/kv delimiters in the joined string.
    let escape = |s: &str| -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if c == '\\' || c == '=' || c == ';' {
                out.push('\\');
            }
            out.push(c);
        }
        out
    };

    let mut parts: Vec<String> = Vec::new();

    match plugin {
        Some("obfs-local") => {
            // sing-box: transport/sip003/obfs.go reads only "obfs" (mode:
            // http|tls) and "obfs-host". Clash's YAML key for mode is
            // `mode`; rename it to sing-box's `obfs`.
            if let Some(mode) = get("mode") {
                if !mode.is_empty() {
                    parts.push(format!("obfs={}", escape(&mode)));
                }
            }
            if let Some(host) = get("host") {
                if !host.is_empty() {
                    parts.push(format!("obfs-host={}", escape(&host)));
                }
            }
        }
        Some("v2ray-plugin") => {
            // sing-box: transport/sip003/v2ray.go.
            if let Some(mode) = get("mode") {
                if !mode.is_empty() {
                    parts.push(format!("mode={}", escape(&mode)));
                }
            }
            if let Some(host) = get("host") {
                if !host.is_empty() {
                    parts.push(format!("host={}", escape(&host)));
                }
            }
            if let Some(path) = get("path") {
                if !path.is_empty() {
                    parts.push(format!("path={}", escape(&path)));
                }
            }
            // sing-box only checks whether "tls" is present in the arg
            // string, never its value — so a bare key is the only way to
            // express "enabled" and the key must be omitted entirely to
            // express "disabled". Emitting "tls=false" would turn TLS ON.
            if get_flag("tls") == Some(true) {
                parts.push("tls".to_string());
            }
            // sing-box parses "mux" with strconv.Atoi and defaults to 1
            // (enabled) when the key is absent entirely — so disabling mux
            // requires an explicit "mux=0"; enabling it can use the bare-key
            // form ("1" is what the reference parser stores for a bare key
            // anyway, so `mux` and `mux=1` are equivalent, but the bare form
            // matches the SIP003 convention other plugins/tools emit).
            match m.get(Value::String("mux".into())) {
                Some(Value::Bool(true)) => parts.push("mux".to_string()),
                Some(Value::Bool(false)) => parts.push("mux=0".to_string()),
                Some(v) => {
                    if let Some(s) = value_to_string(v) {
                        match s.to_ascii_lowercase().as_str() {
                            "true" | "yes" => parts.push("mux".to_string()),
                            "false" | "no" => parts.push("mux=0".to_string()),
                            _ if s.parse::<i64>().is_ok() => parts.push(format!("mux={s}")),
                            _ => {}
                        }
                    }
                }
                None => {}
            }
            // The following mihomo plugin-opts keys have no sing-box
            // equivalent in its built-in v2ray-plugin (transport/sip003/v2ray.go
            // reads only tls/cert/certRaw/mode/host/path/mux) and are
            // dropped rather than passed through:
            // - skip-cert-verify: sing-box's plugin-private TLS client
            //   never reads an insecure/skip flag, so this option cannot be
            //   honored at all here, not just mis-transcribed.
            // - headers, ech-opts, fingerprint, certificate, private-key,
            //   name-cert-verify, v2ray-http-upgrade(-fast-open): mihomo-only
            //   extensions sing-box's implementation does not parse.
        }
        _ => {
            // Unknown/custom plugin: fall back to a best-effort key=value
            // join (previous behavior) since we don't know its arg schema.
            parts.extend(m.iter().filter_map(|(k, v)| {
                Some(format!(
                    "{}={}",
                    escape(&value_to_string(k)?),
                    escape(&value_to_string(v)?)
                ))
            }));
        }
    }

    parts.join(";")
}

fn parse_vmess(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let uuid = get_str(map, &["uuid", "id"]).ok_or_else(|| "vmess: missing uuid".to_string())?;
    let alter_id = get_u16(map, &["alterId", "alter_id", "aid"]).unwrap_or(0);
    let security = get_str(map, &["cipher", "security"]).unwrap_or_else(|| "auto".into());

    let tls = parse_tls_common(map, false);
    let transport = parse_transport(map)?;

    Ok((
        tls,
        transport,
        ProtocolConfig::Vmess {
            uuid,
            alter_id,
            security,
        },
    ))
}

fn parse_vless(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let uuid = get_str(map, &["uuid", "id"]).ok_or_else(|| "vless: missing uuid".to_string())?;
    let flow = get_str(map, &["flow"]);
    let packet_encoding =
        get_str(map, &["packet-encoding", "packet_encoding"]).unwrap_or_else(|| "xudp".into());

    let mut tls = parse_tls_common(map, true);
    if let Some(ref mut t) = tls {
        if let Some(opts) = get_map(map, &["reality-opts", "reality_opts"]) {
            t.reality_public_key = get_str(opts, &["public-key", "public_key"]);
            t.reality_short_id = get_str(opts, &["short-id", "short_id"]);
        }
    }

    let transport = parse_transport(map)?;

    Ok((
        tls,
        transport,
        ProtocolConfig::Vless {
            uuid,
            flow,
            packet_encoding,
        },
    ))
}

fn parse_trojan(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let password =
        get_str(map, &["password"]).ok_or_else(|| "trojan: missing password".to_string())?;

    // Trojan is TLS by default in clash.
    let mut tls = parse_tls_common(map, true).unwrap_or(TlsConfig {
        enabled: true,
        ..Default::default()
    });
    tls.enabled = true;
    if tls.server_name.is_none() {
        tls.server_name = get_str(map, &["sni", "servername", "server-name"]);
    }

    let transport = parse_transport(map)?;

    Ok((Some(tls), transport, ProtocolConfig::Trojan { password }))
}

/// Mihomo / sing-box AnyTLS: password + TLS (sni / skip-cert-verify).
fn parse_anytls(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let password = get_str(map, &["password", "uuid"])
        .ok_or_else(|| "anytls: missing password".to_string())?;

    let mut tls = parse_tls_common(map, true).unwrap_or(TlsConfig {
        enabled: true,
        ..Default::default()
    });
    tls.enabled = true;
    if tls.server_name.is_none() {
        tls.server_name = get_str(map, &["sni", "servername", "server-name"]);
    }
    if tls.insecure.is_none() {
        tls.insecure = get_bool(map, &["skip-cert-verify", "skip_cert_verify", "insecure"]);
    }

    Ok((Some(tls), None, ProtocolConfig::AnyTls { password }))
}

/// Clash Snell: psk + version + optional obfs-opts.
fn parse_snell(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let psk = get_str(map, &["psk", "password"]).ok_or_else(|| "snell: missing psk".to_string())?;

    let version = get_u16(map, &["version"]).map(|v| v as u8).unwrap_or(4);

    let userkey = get_str(map, &["userkey", "user-key", "user_key"]);
    let reuse = get_bool(map, &["reuse"]);

    let mut obfs_mode = get_str(map, &["obfs"]);
    let mut obfs_host = get_str(map, &["obfs-host", "obfs_host", "host"]);
    if let Some(opts) = get_map(map, &["obfs-opts", "obfs_opts"]) {
        if obfs_mode.is_none() {
            obfs_mode = get_str(opts, &["mode"]);
        }
        if obfs_host.is_none() {
            obfs_host = get_str(opts, &["host"]);
        }
    }

    // v6 shaping mode (if present as top-level or under mode)
    let mode = get_str(map, &["mode"]).filter(|m| {
        matches!(
            m.to_ascii_lowercase().as_str(),
            "default" | "unshaped" | "unsafe-raw" | "unsafe_raw"
        )
    });

    Ok((
        None,
        None,
        ProtocolConfig::Snell {
            psk,
            version,
            userkey,
            reuse,
            obfs_mode,
            obfs_host,
            mode,
        },
    ))
}

fn parse_hysteria2(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let password = get_str(map, &["password", "auth"])
        .ok_or_else(|| "hysteria2: missing password".to_string())?;
    let up_mbps = get_u32(map, &["up", "up-mbps", "up_mbps"]);
    let down_mbps = get_u32(map, &["down", "down-mbps", "down_mbps"]);

    let mut obfs = get_str(map, &["obfs"]);
    let mut obfs_password = get_str(map, &["obfs-password", "obfs_password"]);
    if let Some(opts) = get_map(map, &["obfs-opts", "obfs_opts"]) {
        if obfs.is_none() {
            obfs = get_str(opts, &["type"]);
        }
        if obfs_password.is_none() {
            obfs_password = get_str(opts, &["password"]);
        }
    }

    let mut tls = parse_tls_common(map, true).unwrap_or(TlsConfig {
        enabled: true,
        ..Default::default()
    });
    tls.enabled = true;
    if tls.server_name.is_none() {
        tls.server_name = get_str(map, &["sni", "servername"]);
    }

    Ok((
        Some(tls),
        None,
        ProtocolConfig::Hysteria2 {
            password,
            up_mbps,
            down_mbps,
            obfs,
            obfs_password,
        },
    ))
}

fn parse_tuic(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let uuid = get_str(map, &["uuid"]).ok_or_else(|| "tuic: missing uuid".to_string())?;
    let password = get_str(map, &["password"]).unwrap_or_default();
    let congestion_control = get_str(
        map,
        &[
            "congestion-controller",
            "congestion_controller",
            "congestion-control",
        ],
    );
    let udp_relay_mode = get_str(map, &["udp-relay-mode", "udp_relay_mode"]);
    let zero_rtt_handshake = get_bool(
        map,
        &["reduce-rtt", "zero-rtt-handshake", "zero_rtt_handshake"],
    )
    .unwrap_or(false);

    let mut tls = parse_tls_common(map, true).unwrap_or(TlsConfig {
        enabled: true,
        ..Default::default()
    });
    tls.enabled = true;
    if tls.server_name.is_none() {
        tls.server_name = get_str(map, &["sni", "servername"]);
    }
    if tls.alpn.is_none() {
        tls.alpn = get_str_list(map, &["alpn"]);
    }

    Ok((
        Some(tls),
        None,
        ProtocolConfig::Tuic {
            uuid,
            password,
            congestion_control,
            udp_relay_mode,
            zero_rtt_handshake,
        },
    ))
}

fn parse_socks5(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let username = get_str(map, &["username", "user"]);
    let password = get_str(map, &["password"]);
    let tls = parse_tls_common(map, false);

    Ok((tls, None, ProtocolConfig::Socks5 { username, password }))
}

fn parse_http(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let username = get_str(map, &["username", "user"]);
    let password = get_str(map, &["password"]);
    let path = get_str(map, &["path"]);
    // Clash uses type:http + tls:true for HTTPS proxy nodes, and some providers use type:https.
    let default_tls = get_str(map, &["type"])
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false);
    let tls = parse_tls_common(map, default_tls);
    Ok((
        tls,
        None,
        ProtocolConfig::Http {
            username,
            password,
            path,
        },
    ))
}

fn parse_hysteria(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let auth_str = get_str(map, &["auth-str", "auth_str"]);
    let auth_base64 = auth_str.is_none();
    let auth = auth_str
        .or_else(|| get_str(map, &["auth"]))
        .ok_or_else(|| "hysteria: missing auth/auth-str".to_string())?;
    let up_mbps = get_u32(map, &["up", "up-mbps", "up_mbps"]);
    let down_mbps = get_u32(map, &["down", "down-mbps", "down_mbps"]);
    let obfs = get_str(map, &["obfs"]);
    let tls = parse_tls_common(map, true);
    Ok((
        tls,
        None,
        ProtocolConfig::Hysteria {
            auth,
            auth_base64,
            up_mbps,
            down_mbps,
            obfs,
        },
    ))
}

fn parse_shadowtls(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let version = get_u16(map, &["version"]).unwrap_or(1) as u8;
    if !(1..=3).contains(&version) {
        return Err("shadowtls: version must be 1, 2, or 3".into());
    }
    let password = get_str(map, &["password"]);
    let tls = parse_tls_common(map, true);
    Ok((tls, None, ProtocolConfig::ShadowTls { version, password }))
}

fn parse_ssh(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let user = get_str(map, &["user", "username"]).unwrap_or_else(|| "root".into());
    let password = get_str(map, &["password"]);
    let private_key = get_str(map, &["private-key", "private_key"]);
    if password.is_none() && private_key.is_none() {
        return Err("ssh: missing password or private key".into());
    }
    let private_key_passphrase =
        get_str(map, &["private-key-passphrase", "private_key_passphrase"]);
    let host_key = map
        .get(Value::String("host-key".into()))
        .or_else(|| map.get(Value::String("host_key".into())))
        .and_then(Value::as_sequence)
        .map(|v| v.iter().filter_map(value_to_string).collect())
        .unwrap_or_default();
    Ok((
        None,
        None,
        ProtocolConfig::Ssh {
            user,
            password,
            private_key,
            private_key_passphrase,
            host_key,
        },
    ))
}

fn parse_naive(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let username =
        get_str(map, &["username", "user"]).ok_or_else(|| "naive: missing username".to_string())?;
    let password =
        get_str(map, &["password"]).ok_or_else(|| "naive: missing password".to_string())?;
    let quic = get_bool(map, &["quic"]).unwrap_or(false);
    Ok((
        parse_tls_common(map, true),
        None,
        ProtocolConfig::Naive {
            username,
            password,
            quic,
        },
    ))
}

fn parse_tor(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let executable_path = get_str(map, &["executable-path", "executable_path"])
        .ok_or_else(|| "tor: external executable-path is required by bundled core".to_string())?;
    let extra_args = map
        .get(Value::String("extra-args".into()))
        .or_else(|| map.get(Value::String("extra_args".into())))
        .and_then(Value::as_sequence)
        .map(|v| v.iter().filter_map(value_to_string).collect())
        .unwrap_or_default();
    let data_directory = get_str(map, &["data-directory", "data_directory"]);
    Ok((
        None,
        None,
        ProtocolConfig::Tor {
            executable_path,
            extra_args,
            data_directory,
        },
    ))
}

/// sing-box wireguard `address` entries must be CIDR prefixes (`netip.Prefix`).
/// Clash yaml typically gives a bare IP; pad it with /32 (IPv4) or /128 (IPv6).
fn normalize_cidr(addr: String) -> String {
    if addr.contains('/') {
        return addr;
    }
    if addr.contains(':') {
        format!("{addr}/128")
    } else {
        format!("{addr}/32")
    }
}

fn parse_wireguard(
    map: &serde_yaml::Mapping,
) -> Result<(Option<TlsConfig>, Option<Transport>, ProtocolConfig), String> {
    let private_key = get_str(map, &["private-key", "private_key"])
        .ok_or_else(|| "wireguard: missing private key".to_string())?;
    let peer_public_key = get_str(map, &["public-key", "peer-public-key", "peer_public_key"])
        .ok_or_else(|| "wireguard: missing peer public key".to_string())?;
    let pre_shared_key = get_str(map, &["pre-shared-key", "pre_shared_key"]);
    let local_address: Vec<String> = map
        .get(Value::String("ip".into()))
        .or_else(|| map.get(Value::String("local-address".into())))
        .or_else(|| map.get(Value::String("local_address".into())))
        .map(|v| match v {
            Value::Sequence(items) => items.iter().filter_map(value_to_string).collect(),
            _ => value_to_string(v).into_iter().collect(),
        })
        .unwrap_or_default();
    let mut local_address: Vec<String> = local_address.into_iter().map(normalize_cidr).collect();
    if let Some(ipv6) = get_str(map, &["ipv6"]) {
        local_address.push(normalize_cidr(ipv6));
    }
    if local_address.is_empty() {
        return Err("wireguard: missing local address".into());
    }
    let reserved = map
        .get(Value::String("reserved".into()))
        .and_then(Value::as_sequence)
        .map(|v| {
            v.iter()
                .filter_map(|x| x.as_u64().and_then(|n| u8::try_from(n).ok()))
                .collect()
        })
        .unwrap_or_default();
    let mtu = get_u32(map, &["mtu"]);
    Ok((
        None,
        None,
        ProtocolConfig::WireGuard {
            local_address,
            private_key,
            peer_public_key,
            pre_shared_key,
            reserved,
            mtu,
        },
    ))
}

fn parse_tls_common(map: &serde_yaml::Mapping, default_enabled: bool) -> Option<TlsConfig> {
    let explicit = get_bool(map, &["tls"]);
    let has_sni = get_str(map, &["sni", "servername", "server-name"]).is_some();
    let has_reality = get_map(map, &["reality-opts", "reality_opts"]).is_some();
    let enabled = explicit.unwrap_or(default_enabled || has_sni || has_reality);

    if !enabled && !has_reality {
        // Still allow skip-cert-only entries to be ignored.
        if explicit == Some(false) {
            return Some(TlsConfig {
                enabled: false,
                ..Default::default()
            });
        }
        if !default_enabled {
            return None;
        }
    }

    let server_name = get_str(map, &["sni", "servername", "server-name"]);
    let insecure = get_bool(map, &["skip-cert-verify", "skip_cert_verify", "insecure"]);
    // Prefer explicit client-fingerprint. Generic `fingerprint` is often a pin/hash
    // (e.g. 64-char hex on hysteria2) and is NOT a valid sing-box uTLS name.
    let utls_fingerprint = get_str(map, &["client-fingerprint", "client_fingerprint"])
        .or_else(|| get_str(map, &["fingerprint"]))
        .and_then(|s| normalize_utls_fingerprint(&s));

    let alpn = get_str_list(map, &["alpn"]);

    let mut tls = TlsConfig {
        enabled: enabled || has_reality,
        server_name,
        insecure,
        alpn,
        utls_fingerprint,
        reality_public_key: None,
        reality_short_id: None,
    };

    if let Some(opts) = get_map(map, &["reality-opts", "reality_opts"]) {
        tls.reality_public_key = get_str(opts, &["public-key", "public_key"]);
        tls.reality_short_id = get_str(opts, &["short-id", "short_id"]);
        tls.enabled = true;
    }

    Some(tls)
}

/// sing-box uTLS only accepts named profiles (not pin-sha256 / hex hashes).
fn normalize_utls_fingerprint(raw: &str) -> Option<String> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    const VALID: &[&str] = &[
        "chrome",
        "firefox",
        "safari",
        "ios",
        "android",
        "edge",
        "360",
        "qq",
        "random",
        "chrome_psk",
        "chrome_psk_shuffle",
        "chrome_padding_psk_shuffle",
        "chrome_pq",
        "chrome_pq_psk",
    ];
    if VALID.contains(&s.as_str()) {
        Some(s)
    } else {
        None
    }
}

/// Parse the clash `network` field into a [`Transport`]. Unknown networks
/// (xhttp / splithttp / kcp …) are a hard error — the node gets skipped with
/// the reason — because silently degrading them to plain Tcp used to produce
/// nodes that parse fine but can never connect.
fn parse_transport(map: &serde_yaml::Mapping) -> Result<Option<Transport>, String> {
    let network = get_str(map, &["network", "net"]).unwrap_or_else(|| "tcp".into());
    let transport = match network.to_ascii_lowercase().as_str() {
        "ws" | "websocket" => {
            let opts = get_map(map, &["ws-opts", "ws_opts"]);
            let path = opts
                .and_then(|m| get_str(m, &["path"]))
                .or_else(|| get_str(map, &["ws-path", "ws_path", "path"]));
            let headers = opts
                .and_then(|m| get_map(m, &["headers"]))
                .map(map_to_string_map)
                .or_else(|| {
                    get_str(map, &["ws-headers", "host", "Host"]).map(|h| {
                        let mut m = std::collections::BTreeMap::new();
                        m.insert("Host".into(), h);
                        m
                    })
                });
            let max_early_data =
                opts.and_then(|m| get_u32(m, &["max-early-data", "max_early_data"]));
            Some(Transport::Ws {
                path,
                headers,
                max_early_data,
            })
        }
        "grpc" => {
            let opts = get_map(map, &["grpc-opts", "grpc_opts"]);
            let service_name = opts
                .and_then(|m| {
                    get_str(
                        m,
                        &["grpc-service-name", "grpc_service_name", "serviceName"],
                    )
                })
                .or_else(|| get_str(map, &["grpc-service-name", "service_name"]));
            Some(Transport::Grpc { service_name })
        }
        "http" | "h2" => {
            let opts = get_map(map, &["http-opts", "h2-opts", "http_opts"]);
            let path = opts.and_then(|m| {
                m.get(Value::String("path".into())).and_then(|v| match v {
                    Value::Sequence(seq) => seq.first().and_then(value_to_string),
                    other => value_to_string(other),
                })
            });
            let host = opts.and_then(|m| {
                m.get(Value::String("host".into())).and_then(|v| match v {
                    Value::Sequence(seq) => {
                        Some(seq.iter().filter_map(value_to_string).collect::<Vec<_>>())
                    }
                    Value::String(s) => Some(vec![s.clone()]),
                    _ => None,
                })
            });
            Some(Transport::Http { path, host })
        }
        "httpupgrade" | "http-upgrade" => {
            let opts = get_map(map, &["http-opts", "httpupgrade-opts"]);
            let path = opts.and_then(|m| get_str(m, &["path"]));
            let host = opts.and_then(|m| get_str(m, &["host"]));
            Some(Transport::HttpUpgrade { path, host })
        }
        // Xray-only transport; carried in the model so multi-core mode can
        // delegate such nodes to the Xray sidecar (sing-box rejects them).
        "xhttp" | "splithttp" => {
            let opts = get_map(map, &["xhttp-opts", "xhttp_opts", "splithttp-opts"]);
            Some(Transport::Xhttp {
                path: opts.and_then(|m| get_str(m, &["path"])),
                host: opts
                    .and_then(|m| get_str(m, &["host"]))
                    .or_else(|| opts.and_then(|m| get_str(m, &["Host"]))),
                mode: opts.and_then(|m| get_str(m, &["mode"])),
            })
        }
        "tcp" | "" => Some(Transport::Tcp),
        other => return Err(format!("unsupported transport: {other}")),
    };
    Ok(transport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ProtocolConfig;

    const SAMPLE: &str = r#"
proxies:
  - name: "SS-HK"
    type: ss
    server: ss.example.com
    port: 8388
    cipher: aes-256-gcm
    password: "secret"
    udp: true
  - name: "VMess-WS"
    type: vmess
    server: vm.example.com
    port: 443
    uuid: 11111111-1111-1111-1111-111111111111
    alterId: 0
    cipher: auto
    tls: true
    skip-cert-verify: true
    servername: cdn.example.com
    network: ws
    ws-opts:
      path: /ray
      headers:
        Host: cdn.example.com
  - name: "VLESS-Reality"
    type: vless
    server: vl.example.com
    port: 443
    uuid: 22222222-2222-2222-2222-222222222222
    tls: true
    servername: www.microsoft.com
    client-fingerprint: chrome
    network: tcp
    flow: xtls-rprx-vision
    reality-opts:
      public-key: pubkey123
      short-id: abcd
  - name: "Trojan-1"
    type: trojan
    server: tj.example.com
    port: 443
    password: "tjpass"
    sni: tj.example.com
    skip-cert-verify: false
  - name: "Hy2"
    type: hysteria2
    server: hy2.example.com
    port: 443
    password: "hy2pass"
    sni: hy2.example.com
    skip-cert-verify: true
    up: "100"
    down: "100"
  - name: "TUIC-1"
    type: tuic
    server: tuic.example.com
    port: 443
    uuid: 33333333-3333-3333-3333-333333333333
    password: "tuicpass"
    sni: tuic.example.com
    congestion-controller: bbr
    udp-relay-mode: native
  - name: "Socks"
    type: socks5
    server: 127.0.0.1
    port: 1080
    username: user
    password: pass
  - name: "SSR-skip"
    type: ssr
    server: x.com
    port: 1
"#;

    #[test]
    fn parses_mixed_clash_proxies() {
        let result = parse_clash_yaml(SAMPLE).expect("parse ok");
        assert_eq!(result.format, SubscriptionFormat::ClashYaml);
        assert_eq!(result.nodes.len(), 7);
        assert_eq!(result.skipped.len(), 1);
        assert!(result.skipped[0].reason.contains("unsupported type: ssr"));
        let ss = result.nodes.iter().find(|n| n.name == "SS-HK").expect("ss");
        assert_eq!(ss.protocol, Protocol::Shadowsocks);
        assert_eq!(ss.server, "ss.example.com");
        assert_eq!(ss.port, 8388);
        assert_eq!(ss.udp, Some(true));
        match &ss.config {
            ProtocolConfig::Shadowsocks {
                method, password, ..
            } => {
                assert_eq!(method, "aes-256-gcm");
                assert_eq!(password, "secret");
            }
            _ => panic!("expected ss config"),
        }

        let vm = result
            .nodes
            .iter()
            .find(|n| n.name == "VMess-WS")
            .expect("vmess");
        assert!(vm.tls.as_ref().is_some_and(|t| t.enabled));
        assert!(matches!(
            vm.transport,
            Some(Transport::Ws {
                path: Some(ref p),
                ..
            }) if p == "/ray"
        ));

        let vl = result
            .nodes
            .iter()
            .find(|n| n.name == "VLESS-Reality")
            .expect("vless");
        let tls = vl.tls.as_ref().expect("tls");
        assert_eq!(tls.reality_public_key.as_deref(), Some("pubkey123"));
        assert_eq!(tls.utls_fingerprint.as_deref(), Some("chrome"));
        match &vl.config {
            ProtocolConfig::Vless { flow, .. } => {
                assert_eq!(flow.as_deref(), Some("xtls-rprx-vision"));
            }
            _ => panic!("expected vless"),
        }

        assert!(result.nodes.iter().all(|n| !n.id.is_empty()));
    }

    #[test]
    fn unknown_transport_skips_node_instead_of_degrading_to_tcp() {
        // kcp has no representation in the Transport model — it must be an
        // explicit skip, never a silent downgrade to plain Tcp (the old
        // catch-all behavior that produced nodes which could never connect).
        // xhttp IS representable now (see parses_xhttp_network_with_opts).
        let yaml = r#"
proxies:
  - name: "VM-KCP"
    type: vmess
    server: vm.example.com
    port: 443
    uuid: 11111111-1111-1111-1111-111111111111
    alterId: 0
    cipher: auto
    network: kcp
  - name: "VL-OK"
    type: vless
    server: vl.example.com
    port: 443
    uuid: 22222222-2222-2222-2222-222222222222
    tls: true
    network: tcp
"#;
        let result = parse_clash_yaml(yaml).expect("parse ok");
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "VL-OK");
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].name, Some("VM-KCP".into()));
        assert!(result.skipped[0]
            .reason
            .contains("unsupported transport: kcp"));
    }

    #[test]
    fn parses_xhttp_network_with_opts() {
        // xhttp IS representable (Xray-only) — must parse into the model,
        // never degrade to Tcp.
        let yaml = r#"
proxies:
  - name: "VL-XHTTP"
    type: vless
    server: vl.example.com
    port: 443
    uuid: 22222222-2222-2222-2222-222222222222
    tls: true
    network: xhttp
    xhttp-opts:
      path: /upload
      host: cdn.example.com
      mode: stream-up
"#;
        let result = parse_clash_yaml(yaml).expect("parse ok");
        assert_eq!(result.nodes.len(), 1);
        match &result.nodes[0].transport {
            Some(Transport::Xhttp { path, host, mode }) => {
                assert_eq!(path.as_deref(), Some("/upload"));
                assert_eq!(host.as_deref(), Some("cdn.example.com"));
                assert_eq!(mode.as_deref(), Some("stream-up"));
            }
            other => panic!("expected xhttp transport, got {other:?}"),
        }
    }

    #[test]
    fn ignores_hex_fingerprint_as_utls() {
        // Many hy2 panels put a pin/hash in `fingerprint` — not a uTLS profile.
        let yaml = r#"
- name: "Hy2-BadFp"
  type: hysteria2
  server: hy2.example.com
  port: 443
  password: "x"
  sni: www.example.com
  fingerprint: 59777b9f4c7e20e49d88b179b5e3f75031f1e08be731670b2ee09acb6c1f3811
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        let n = &result.nodes[0];
        let tls = n.tls.as_ref().expect("tls");
        assert!(
            tls.utls_fingerprint.is_none(),
            "hex fingerprint must not become utls"
        );
    }

    #[test]
    fn parses_bare_sequence() {
        let yaml = r#"
- name: only
  type: ss
  server: a.com
  port: 1
  cipher: aes-128-gcm
  password: p
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        assert_eq!(result.nodes.len(), 1);
    }

    // Some providers concatenate several Clash documents into one payload
    // (`---` separators). serde_yaml's `from_str` used to reject that with
    // "deserializing from YAML containing more than one document is not
    // supported"; the proxies lists of all documents must be merged instead,
    // and documents without a `proxies` list are skipped.
    #[test]
    fn merges_multi_document_clash_yaml() {
        let yaml = r#"
proxies:
  - name: "Doc1-SS"
    type: ss
    server: a.com
    port: 1
    cipher: aes-256-gcm
    password: p
---
mixed-port: 7890
mode: rule
---
proxies:
  - name: "Doc3-Trojan"
    type: trojan
    server: b.com
    port: 443
    password: q
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        assert_eq!(result.nodes.len(), 2);
        assert_eq!(result.nodes[0].name, "Doc1-SS");
        assert_eq!(result.nodes[1].name, "Doc3-Trojan");
    }

    // A single document with a leading `---` marker (the common form emitted
    // by exporters) must keep parsing as before.
    #[test]
    fn parses_single_document_with_leading_marker() {
        let yaml = "---\nproxies:\n  - name: only\n    type: ss\n    server: a.com\n    port: 1\n    cipher: aes-128-gcm\n    password: p\n";
        let result = parse_clash_yaml(yaml).unwrap();
        assert_eq!(result.nodes.len(), 1);
    }

    // sing-box's built-in obfs plugin is registered under the name
    // "obfs-local" (transport/sip003/plugin.go). Clash's `plugin: obfs`
    // passed straight through causes sing-box to fail with
    // "plugin not found: obfs" at runtime instead of a parse-time error, so
    // this must be caught here rather than surfacing much later.
    #[test]
    fn renames_clash_obfs_plugin_to_obfs_local() {
        let yaml = r#"
- name: "SS-Obfs"
  type: ss
  server: a.com
  port: 1
  cipher: aes-256-gcm
  password: p
  plugin: obfs
  plugin-opts:
    mode: http
    host: www.bing.com
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        match &result.nodes[0].config {
            ProtocolConfig::Shadowsocks {
                plugin,
                plugin_opts,
                ..
            } => {
                assert_eq!(plugin.as_deref(), Some("obfs-local"));
                // obfs-local reads "obfs" (not "mode") and "obfs-host" (not
                // "host") — transport/sip003/obfs.go.
                let opts = plugin_opts.as_deref().unwrap_or_default();
                assert!(opts.contains("obfs=http"), "opts: {opts}");
                assert!(opts.contains("obfs-host=www.bing.com"), "opts: {opts}");
            }
            _ => panic!("expected ss config"),
        }
    }

    // sing-box has no SIP003 arg-string equivalent for shadow-tls (it's a
    // separate `shadowtls` outbound + detour instead), so a shadow-tls node
    // must be skipped at parse time rather than silently emitting a
    // plugin_opts string sing-box can't use. Other nodes in the same
    // subscription must still parse normally.
    #[test]
    fn parses_shadow_tls_plugin_into_dedicated_field() {
        let yaml = r#"
- name: "SS-ShadowTLS"
  type: ss
  server: a.com
  port: 1
  cipher: aes-256-gcm
  password: p
  plugin: shadow-tls
  plugin-opts:
    host: www.bing.com
    password: tls-pass
    version: 3
  client-fingerprint: chrome
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert!(result.skipped.is_empty());
        match &result.nodes[0].config {
            ProtocolConfig::Shadowsocks {
                plugin,
                plugin_opts,
                shadow_tls,
                ..
            } => {
                // shadow-tls has no SIP003 arg-string form, so plugin/plugin_opts
                // must stay unset — it's carried in the dedicated field instead.
                assert_eq!(plugin.as_deref(), Some("shadow-tls"));
                assert!(plugin_opts.is_none());
                let st = shadow_tls.as_ref().expect("shadow_tls opts");
                assert_eq!(st.host, "www.bing.com");
                assert_eq!(st.password, "tls-pass");
                assert_eq!(st.version, 3);
                assert_eq!(st.fingerprint.as_deref(), Some("chrome"));
            }
            _ => panic!("expected ss config"),
        }
    }

    // mihomo defaults shadow-tls to protocol version 3 when the field is absent.
    #[test]
    fn defaults_shadow_tls_version_to_3() {
        let yaml = r#"
- name: "SS-ShadowTLS"
  type: ss
  server: a.com
  port: 1
  cipher: aes-256-gcm
  password: p
  plugin: shadow-tls
  plugin-opts:
    host: www.bing.com
    password: tls-pass
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        match &result.nodes[0].config {
            ProtocolConfig::Shadowsocks { shadow_tls, .. } => {
                assert_eq!(shadow_tls.as_ref().unwrap().version, 3);
            }
            _ => panic!("expected ss config"),
        }
    }

    // Clash models v2ray-plugin's `mux` as a bool; sing-box parses it with
    // strconv.Atoi and errors out on anything non-numeric
    // (transport/sip003/v2ray.go: `E.Cause(err, "parse mux value")`).
    // Emitting the Clash bool verbatim ("mux=true") reproduces that crash —
    // this is the bug this whole conversion path exists to prevent. The
    // reference SIP003 parser (shadowsocks/v2ray-plugin args.go) stores a
    // bare key as value "1", so a bare `mux` is what sing-box actually
    // expects for "enabled", not `mux=1`.
    #[test]
    fn converts_v2ray_plugin_mux_bool_to_sing_box_bare_key() {
        let yaml = r#"
- name: "SS-V2ray-Mux"
  type: ss
  server: a.com
  port: 1
  cipher: aes-256-gcm
  password: p
  plugin: v2ray-plugin
  plugin-opts:
    mode: websocket
    mux: true
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        match &result.nodes[0].config {
            ProtocolConfig::Shadowsocks { plugin_opts, .. } => {
                let opts = plugin_opts.as_deref().unwrap_or_default();
                assert!(
                    opts.split(';').any(|p| p == "mux"),
                    "expected bare `mux` key, got: {opts}"
                );
                assert!(!opts.contains("mux=true"), "opts: {opts}");
            }
            _ => panic!("expected ss config"),
        }
    }

    // sing-box's v2ray-plugin only checks whether the "tls" key is present
    // in the plugin_opts string at all — it never inspects the value after
    // "=" (transport/sip003/v2ray.go: `if _, loaded := pluginOpts.Get("tls");
    // loaded { tlsOptions.Enabled = true }`). So a naive key=value transcription
    // of Clash's `tls: false` would produce "tls=false", which sing-box reads
    // as TLS *enabled* — the opposite of what was configured. The only way to
    // express "disabled" is to omit the key entirely.
    #[test]
    fn omits_v2ray_plugin_tls_key_when_disabled() {
        let yaml = r#"
- name: "SS-V2ray-NoTls"
  type: ss
  server: a.com
  port: 1
  cipher: aes-256-gcm
  password: p
  plugin: v2ray-plugin
  plugin-opts:
    mode: websocket
    tls: false
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        match &result.nodes[0].config {
            ProtocolConfig::Shadowsocks { plugin_opts, .. } => {
                let opts = plugin_opts.as_deref().unwrap_or_default();
                assert!(!opts.contains("tls"), "opts: {opts}");
            }
            _ => panic!("expected ss config"),
        }
    }

    #[test]
    fn emits_bare_v2ray_plugin_tls_key_when_enabled() {
        let yaml = r#"
- name: "SS-V2ray-Tls"
  type: ss
  server: a.com
  port: 1
  cipher: aes-256-gcm
  password: p
  plugin: v2ray-plugin
  plugin-opts:
    mode: websocket
    tls: true
    host: cdn.example.com
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        match &result.nodes[0].config {
            ProtocolConfig::Shadowsocks { plugin_opts, .. } => {
                let opts = plugin_opts.as_deref().unwrap_or_default();
                // Must be a bare key, not "tls=true" — sing-box's arg parser
                // doesn't reject the latter, but writing it as bare matches
                // sing-box's own emitted format and avoids relying on the
                // "value is ignored anyway" quirk.
                assert!(
                    opts.split(';').any(|p| p == "tls"),
                    "expected bare `tls` key, got: {opts}"
                );
                assert!(opts.contains("host=cdn.example.com"), "opts: {opts}");
            }
            _ => panic!("expected ss config"),
        }
    }

    // skip-cert-verify has no equivalent in sing-box's built-in v2ray-plugin:
    // its plugin-private TLS client (transport/sip003/v2ray.go) never sets
    // an Insecure/skip field on the option.OutboundTLSOptions it builds, and
    // the outbound's own top-level `tls.insecure` isn't consulted by the
    // plugin either. So this Clash option cannot be honored at all through
    // sing-box's plugin_opts — it must be dropped rather than mis-transcribed
    // into something that looks like it works but silently doesn't.
    #[test]
    fn drops_unsupported_skip_cert_verify_from_v2ray_plugin_opts() {
        let yaml = r#"
- name: "SS-V2ray-Insecure"
  type: ss
  server: a.com
  port: 1
  cipher: aes-256-gcm
  password: p
  plugin: v2ray-plugin
  plugin-opts:
    mode: websocket
    tls: true
    skip-cert-verify: true
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        match &result.nodes[0].config {
            ProtocolConfig::Shadowsocks { plugin_opts, .. } => {
                let opts = plugin_opts.as_deref().unwrap_or_default();
                assert!(!opts.contains("skip-cert-verify"), "opts: {opts}");
            }
            _ => panic!("expected ss config"),
        }
    }

    // The SIP003 arg grammar (shadowsocks/v2ray-plugin args.go) uses `;` to
    // separate key=value pairs and `=` to separate key from value, with `\`
    // as the escape character — so any of those three bytes appearing
    // unescaped inside a value corrupts the split for every field after it.
    // A `path` containing `;` is a realistic case (query strings, multiple
    // ws sub-paths some servers configure).
    #[test]
    fn escapes_delimiter_characters_in_v2ray_plugin_opts() {
        let yaml = r#"
- name: "SS-V2ray-Escape"
  type: ss
  server: a.com
  port: 1
  cipher: aes-256-gcm
  password: p
  plugin: v2ray-plugin
  plugin-opts:
    mode: websocket
    path: "/a;b=c"
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        match &result.nodes[0].config {
            ProtocolConfig::Shadowsocks { plugin_opts, .. } => {
                let opts = plugin_opts.as_deref().unwrap_or_default();
                assert!(opts.contains(r"path=/a\;b\=c"), "opts: {opts}");
            }
            _ => panic!("expected ss config"),
        }
    }

    // mihomo's standard `alpn` form is a YAML sequence (`alpn: [h2, http/1.1]`),
    // not a comma-joined string. get_str (String/Number/Bool only) can't see
    // a Value::Sequence, so this used to silently drop the field.
    #[test]
    fn parses_alpn_as_yaml_sequence() {
        let yaml = r#"
- name: "Trojan-Alpn"
  type: trojan
  server: a.com
  port: 443
  password: p
  alpn: [h2, "http/1.1"]
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        let tls = result.nodes[0].tls.as_ref().expect("tls");
        assert_eq!(
            tls.alpn.as_deref(),
            Some(&["h2".to_string(), "http/1.1".to_string()][..])
        );
    }

    // sing-box's wireguard `address` entries are netip.Prefix and require a
    // CIDR suffix; Clash's `ip:` is normally a bare address.
    #[test]
    fn normalizes_wireguard_address_to_cidr() {
        let yaml = r#"
- name: "WG"
  type: wireguard
  server: a.com
  port: 51820
  private-key: "cHJpdmF0ZQ=="
  public-key: "cHVibGlj"
  ip: 10.0.0.2
  ipv6: "fd00::2"
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        match &result.nodes[0].config {
            ProtocolConfig::WireGuard { local_address, .. } => {
                assert_eq!(
                    local_address,
                    &["10.0.0.2/32".to_string(), "fd00::2/128".to_string()]
                );
            }
            _ => panic!("expected wireguard config"),
        }
    }

    // A `/`-suffixed address must be left untouched rather than double-padded.
    #[test]
    fn leaves_wireguard_address_with_existing_cidr_untouched() {
        let yaml = r#"
- name: "WG"
  type: wireguard
  server: a.com
  port: 51820
  private-key: "cHJpdmF0ZQ=="
  public-key: "cHVibGlj"
  ip: 10.0.0.2/24
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        match &result.nodes[0].config {
            ProtocolConfig::WireGuard { local_address, .. } => {
                assert_eq!(local_address, &["10.0.0.2/24".to_string()]);
            }
            _ => panic!("expected wireguard config"),
        }
    }

    // clash2singbox's anyToMbps: plain numbers pass through as Mbps; unit
    // suffixes (K/M/G/T, lower-case b or upper-case B) scale relative to
    // Mbps, with bytes (B) multiplied by 8. A naive "take leading digits"
    // parse mistakes "1Gbps" (1000 Mbps) for 1 Mbps.
    #[test]
    fn converts_hysteria_rate_units_to_mbps() {
        let yaml = r#"
- name: "Hy2"
  type: hysteria2
  server: a.com
  port: 443
  password: p
  up: "1Gbps"
  down: "100 Mbps"
"#;
        let result = parse_clash_yaml(yaml).unwrap();
        match &result.nodes[0].config {
            ProtocolConfig::Hysteria2 {
                up_mbps, down_mbps, ..
            } => {
                assert_eq!(*up_mbps, Some(1000));
                assert_eq!(*down_mbps, Some(100));
            }
            _ => panic!("expected hysteria2 config"),
        }
    }
}
