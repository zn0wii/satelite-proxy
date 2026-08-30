//! Parse proxy share URIs: ss://, vmess://, vless://, trojan://, hysteria2://, tuic://, anytls://, snell://

use crate::domain::{
    ParseResult, Protocol, ProtocolConfig, ProxyNode, SkippedProxy, SubscriptionFormat, TlsConfig,
    Transport,
};
use crate::error::{AppError, AppResult};
use base64::{engine::general_purpose, Engine as _};
use std::collections::BTreeMap;
use url::Url;

pub fn parse_uri_list(content: &str, format: SubscriptionFormat) -> AppResult<ParseResult> {
    let content = content.trim();
    if content.is_empty() {
        return Err(AppError::EmptySubscription);
    }

    let mut nodes = Vec::new();
    let mut skipped = Vec::new();
    let mut entries = 0;

    for (idx, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        entries += 1;
        crate::subscription::ensure_entry_limit(entries)?;
        match parse_uri_line(line) {
            Ok(node) => nodes.push(node.with_computed_id()),
            Err(reason) => skipped.push(SkippedProxy {
                name: Some(format!("line-{}", idx + 1)),
                reason,
            }),
        }
    }

    if nodes.is_empty() {
        return Err(AppError::NoProxies);
    }

    Ok(ParseResult {
        nodes,
        skipped,
        format,
    })
}

pub fn parse_uri_line(line: &str) -> Result<ProxyNode, String> {
    let line = line.trim();
    let scheme = line.split(':').next().unwrap_or("").to_ascii_lowercase();

    match scheme.as_str() {
        "ss" => parse_ss_uri(line),
        "vmess" => parse_vmess_uri(line),
        "vless" => parse_vless_uri(line),
        "trojan" => parse_trojan_uri(line),
        "hysteria2" | "hy2" => parse_hysteria2_uri(line),
        "tuic" => parse_tuic_uri(line),
        "socks" | "socks5" => parse_socks_uri(line),
        "http" | "https" => parse_http_uri(line),
        "hysteria" | "hy" => parse_hysteria_uri(line),
        "shadowtls" => parse_shadowtls_uri(line),
        "ssh" => parse_ssh_uri(line),
        "naive" | "naive+https" | "naive+quic" => parse_naive_uri(line),
        "tor" => parse_tor_uri(line),
        "anytls" => parse_anytls_uri(line),
        "snell" => parse_snell_uri(line),
        _ => Err(format!("unsupported uri scheme: {scheme}")),
    }
}

fn basic_url_parts(
    line: &str,
    label: &str,
    default_port: Option<u16>,
) -> Result<(Url, String, u16, String, BTreeMap<String, String>), String> {
    let url = Url::parse(line).map_err(|e| format!("{label} url: {e}"))?;
    let server = url
        .host_str()
        .ok_or_else(|| format!("{label}: missing host"))?
        .to_string();
    let port = url
        .port()
        .or(default_port)
        .ok_or_else(|| format!("{label}: missing port"))?;
    let name = fragment_name(&url).unwrap_or_else(|| format!("{label}-{server}-{port}"));
    let query = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Ok((url, server, port, name, query))
}

fn tls_from_query(query: &BTreeMap<String, String>, default_enabled: bool) -> Option<TlsConfig> {
    let enabled = query
        .get("tls")
        .map(|v| matches!(v.as_str(), "1" | "true"))
        .unwrap_or(default_enabled);
    if !enabled {
        return None;
    }
    Some(TlsConfig {
        enabled: true,
        server_name: query.get("sni").or_else(|| query.get("peer")).cloned(),
        insecure: query
            .get("insecure")
            .or_else(|| query.get("allowInsecure"))
            .map(|v| matches!(v.as_str(), "1" | "true")),
        alpn: None,
        utls_fingerprint: None,
        reality_public_key: None,
        reality_short_id: None,
    })
}

fn parse_http_uri(line: &str) -> Result<ProxyNode, String> {
    let (url, server, port, name, query) = basic_url_parts(line, "http", None)?;
    let username = (!url.username().is_empty()).then(|| percent_decode(url.username()));
    let password = url.password().map(percent_decode);
    let tls = tls_from_query(&query, url.scheme().eq_ignore_ascii_case("https"));
    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Http,
        server,
        port,
        tls,
        transport: None,
        udp: Some(false),
        config: ProtocolConfig::Http {
            username,
            password,
            path: query.get("path").cloned(),
        },
        source: Some(url.scheme().into()),
        latency_ms: None,
        latency_at: None,
    })
}

fn parse_hysteria_uri(line: &str) -> Result<ProxyNode, String> {
    let (url, server, port, name, query) = basic_url_parts(line, "hysteria", Some(443))?;
    let auth = if !url.username().is_empty() {
        percent_decode(url.username())
    } else {
        query
            .get("auth")
            .or_else(|| query.get("auth_str"))
            .cloned()
            .unwrap_or_default()
    };
    if auth.is_empty() {
        return Err("hysteria: missing auth".into());
    }
    let num = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| query.get(*k))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
    };
    let tls = tls_from_query(&query, true);
    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Hysteria,
        server,
        port,
        tls,
        transport: None,
        udp: Some(true),
        config: ProtocolConfig::Hysteria {
            auth,
            auth_base64: false,
            up_mbps: num(&["upmbps", "up_mbps", "up"]),
            down_mbps: num(&["downmbps", "down_mbps", "down"]),
            obfs: query.get("obfs").cloned(),
        },
        source: Some(url.scheme().into()),
        latency_ms: None,
        latency_at: None,
    })
}

fn parse_shadowtls_uri(line: &str) -> Result<ProxyNode, String> {
    let (url, server, port, name, query) = basic_url_parts(line, "shadowtls", Some(443))?;
    let password = (!url.username().is_empty())
        .then(|| percent_decode(url.username()))
        .or_else(|| query.get("password").cloned());
    let version = query
        .get("version")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    if !(1..=3).contains(&version) {
        return Err("shadowtls: version must be 1, 2, or 3".into());
    }
    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::ShadowTls,
        server,
        port,
        tls: tls_from_query(&query, true),
        transport: None,
        udp: Some(false),
        config: ProtocolConfig::ShadowTls { version, password },
        source: Some(url.scheme().into()),
        latency_ms: None,
        latency_at: None,
    })
}

fn parse_ssh_uri(line: &str) -> Result<ProxyNode, String> {
    let (url, server, port, name, query) = basic_url_parts(line, "ssh", Some(22))?;
    let user = if url.username().is_empty() {
        "root".into()
    } else {
        percent_decode(url.username())
    };
    let password = url
        .password()
        .map(percent_decode)
        .or_else(|| query.get("password").cloned());
    let private_key = query
        .get("private_key")
        .or_else(|| query.get("private-key"))
        .cloned();
    if password.is_none() && private_key.is_none() {
        return Err("ssh: missing password or private key".into());
    }
    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Ssh,
        server,
        port,
        tls: None,
        transport: None,
        udp: Some(false),
        config: ProtocolConfig::Ssh {
            user,
            password,
            private_key,
            private_key_passphrase: query.get("private_key_passphrase").cloned(),
            host_key: Vec::new(),
        },
        source: Some(url.scheme().into()),
        latency_ms: None,
        latency_at: None,
    })
}

fn parse_naive_uri(line: &str) -> Result<ProxyNode, String> {
    let normalized = if line.starts_with("naive+https://") || line.starts_with("naive+quic://") {
        line.replacen(line.split(':').next().unwrap_or("naive"), "naive", 1)
    } else {
        line.to_string()
    };
    let (url, server, port, name, query) = basic_url_parts(&normalized, "naive", Some(443))?;
    let username = percent_decode(url.username());
    let password = url.password().map(percent_decode).unwrap_or_default();
    if username.is_empty() || password.is_empty() {
        return Err("naive: missing username/password".into());
    }
    let quic = line.starts_with("naive+quic://")
        || query
            .get("quic")
            .map(|v| matches!(v.as_str(), "1" | "true"))
            .unwrap_or(false);
    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Naive,
        server,
        port,
        tls: tls_from_query(&query, true),
        transport: None,
        udp: Some(true),
        config: ProtocolConfig::Naive {
            username,
            password,
            quic,
        },
        source: Some(url.scheme().into()),
        latency_ms: None,
        latency_at: None,
    })
}

fn parse_tor_uri(line: &str) -> Result<ProxyNode, String> {
    let (url, _host, _port, name, query) = basic_url_parts(line, "tor", Some(1))?;
    let executable_path = query
        .get("executable_path")
        .or_else(|| query.get("executable-path"))
        .cloned()
        .ok_or_else(|| "tor: external executable_path is required by bundled core".to_string())?;
    let extra_args = query
        .get("extra_args")
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Tor,
        server: "localhost".into(),
        port: 0,
        tls: None,
        transport: None,
        udp: Some(false),
        config: ProtocolConfig::Tor {
            executable_path,
            extra_args,
            data_directory: query.get("data_directory").cloned(),
        },
        source: Some(url.scheme().into()),
        latency_ms: None,
        latency_at: None,
    })
}

fn decode_base64_flexible(input: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    // URL_SAFE and STANDARD, with/without padding
    general_purpose::STANDARD
        .decode(&cleaned)
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(&cleaned))
        .or_else(|_| general_purpose::URL_SAFE.decode(&cleaned))
        .or_else(|_| general_purpose::URL_SAFE_NO_PAD.decode(&cleaned))
        .map_err(|e| format!("base64 decode failed: {e}"))
}

fn percent_decode(s: &str) -> String {
    urlencoding::decode(s)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| s.to_string())
}

fn fragment_name(url: &Url) -> Option<String> {
    url.fragment().map(percent_decode).filter(|s| !s.is_empty())
}

/// ss://method:password@host:port#name
/// ss://base64(method:password@host:port)#name
fn parse_ss_uri(line: &str) -> Result<ProxyNode, String> {
    let rest = line
        .strip_prefix("ss://")
        .or_else(|| line.strip_prefix("SS://"))
        .ok_or_else(|| "not ss uri".to_string())?;

    let (body, name_from_frag) = split_fragment(rest);
    let body = body.split('?').next().unwrap_or(body);

    let decoded = if body.contains('@') {
        body.to_string()
    } else {
        let bytes = decode_base64_flexible(body)?;
        String::from_utf8(bytes).map_err(|e| e.to_string())?
    };

    // method:password@host:port
    let (userinfo, hostport) = decoded
        .rsplit_once('@')
        .ok_or_else(|| "ss: missing @".to_string())?;
    let (method, password) = userinfo
        .split_once(':')
        .ok_or_else(|| "ss: missing method:password".to_string())?;
    let (server, port) = split_host_port(hostport)?;

    let name = name_from_frag.unwrap_or_else(|| format!("ss-{server}-{port}"));

    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Shadowsocks,
        server,
        port,
        tls: None,
        transport: None,
        udp: Some(true),
        config: ProtocolConfig::Shadowsocks {
            method: percent_decode(method),
            password: percent_decode(password),
            plugin: None,
            plugin_opts: None,
            shadow_tls: None,
        },
        source: Some("ss".into()),
        latency_ms: None,
        latency_at: None,
    })
}

/// vmess://base64(json)
fn parse_vmess_uri(line: &str) -> Result<ProxyNode, String> {
    let rest = line
        .strip_prefix("vmess://")
        .or_else(|| line.strip_prefix("VMESS://"))
        .ok_or_else(|| "not vmess uri".to_string())?;

    let bytes = decode_base64_flexible(rest.trim())?;
    let json: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("vmess json: {e}"))?;

    let name = json
        .get("ps")
        .or_else(|| json.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("vmess")
        .to_string();
    let server = json
        .get("add")
        .or_else(|| json.get("host"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "vmess: missing add".to_string())?
        .to_string();
    let port = json
        .get("port")
        .map(|v| match v {
            serde_json::Value::Number(n) => n.as_u64().map(|u| u as u16),
            serde_json::Value::String(s) => s.parse().ok(),
            _ => None,
        })
        .flatten()
        .ok_or_else(|| "vmess: missing port".to_string())?;
    let uuid = json
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "vmess: missing id".to_string())?
        .to_string();
    let alter_id = json
        .get("aid")
        .map(|v| match v {
            serde_json::Value::Number(n) => n.as_u64().unwrap_or(0) as u16,
            serde_json::Value::String(s) => s.parse().unwrap_or(0),
            _ => 0,
        })
        .unwrap_or(0);
    let security = json
        .get("scy")
        .or_else(|| json.get("security"))
        .and_then(|v| v.as_str())
        .unwrap_or("auto")
        .to_string();

    let tls_enabled = json
        .get("tls")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty() && s != "none")
        .unwrap_or(false);
    let sni = json
        .get("sni")
        .or_else(|| json.get("host"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let network = json
        .get("net")
        .and_then(|v| v.as_str())
        .unwrap_or("tcp")
        .to_ascii_lowercase();
    let path = json
        .get("path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let host_header = json
        .get("host")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let transport = match network.as_str() {
        "ws" => {
            let mut headers = BTreeMap::new();
            if let Some(h) = host_header.clone() {
                headers.insert("Host".into(), h);
            }
            Some(Transport::Ws {
                path,
                headers: if headers.is_empty() {
                    None
                } else {
                    Some(headers)
                },
                max_early_data: None,
            })
        }
        "grpc" => Some(Transport::Grpc { service_name: path }),
        "h2" | "http" => Some(Transport::Http {
            path,
            host: host_header.map(|h| vec![h]),
        }),
        "httpupgrade" => Some(Transport::HttpUpgrade {
            path,
            host: host_header,
        }),
        "xhttp" | "splithttp" => Some(Transport::Xhttp {
            path,
            host: host_header,
            mode: None,
        }),
        "tcp" | "" => Some(Transport::Tcp),
        other => return Err(format!("unsupported transport: {other}")),
    };

    let tls = if tls_enabled {
        Some(TlsConfig {
            enabled: true,
            server_name: sni,
            insecure: None,
            alpn: None,
            utls_fingerprint: None,
            reality_public_key: None,
            reality_short_id: None,
        })
    } else {
        None
    };

    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Vmess,
        server,
        port,
        tls,
        transport,
        udp: None,
        config: ProtocolConfig::Vmess {
            uuid,
            alter_id,
            security,
        },
        source: Some("vmess".into()),
        latency_ms: None,
        latency_at: None,
    })
}

fn parse_vless_uri(line: &str) -> Result<ProxyNode, String> {
    let url = Url::parse(line).map_err(|e| format!("vless url: {e}"))?;
    let uuid = percent_decode(url.username());
    if uuid.is_empty() {
        return Err("vless: missing uuid".into());
    }
    let server = url
        .host_str()
        .ok_or_else(|| "vless: missing host".to_string())?
        .to_string();
    let port = url
        .port()
        .ok_or_else(|| "vless: missing port".to_string())?;
    let name = fragment_name(&url).unwrap_or_else(|| format!("vless-{server}-{port}"));

    let query: BTreeMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let security = query.get("security").map(|s| s.as_str()).unwrap_or("none");
    let mut tls = if security == "tls" || security == "reality" {
        Some(TlsConfig {
            enabled: true,
            server_name: query
                .get("sni")
                .cloned()
                .or_else(|| query.get("host").cloned()),
            insecure: query
                .get("allowInsecure")
                .or_else(|| query.get("insecure"))
                .map(|v| v == "1" || v == "true"),
            alpn: query.get("alpn").map(|s| {
                s.split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            }),
            utls_fingerprint: query.get("fp").cloned().and_then(|s| normalize_utls_fp(&s)),
            reality_public_key: query.get("pbk").cloned(),
            reality_short_id: query.get("sid").cloned(),
        })
    } else {
        None
    };
    if security == "reality" {
        if let Some(ref mut t) = tls {
            t.enabled = true;
        }
    }

    let network = query
        .get("type")
        .map(|s| s.as_str())
        .unwrap_or("tcp")
        .to_ascii_lowercase();
    let transport = match network.as_str() {
        "ws" => {
            let mut headers = BTreeMap::new();
            if let Some(h) = query.get("host") {
                headers.insert("Host".into(), h.clone());
            }
            Some(Transport::Ws {
                path: query.get("path").cloned(),
                headers: if headers.is_empty() {
                    None
                } else {
                    Some(headers)
                },
                max_early_data: None,
            })
        }
        "grpc" => Some(Transport::Grpc {
            service_name: query
                .get("serviceName")
                .cloned()
                .or_else(|| query.get("service_name").cloned()),
        }),
        "http" | "h2" => Some(Transport::Http {
            path: query.get("path").cloned(),
            host: query.get("host").map(|h| vec![h.clone()]),
        }),
        "httpupgrade" => Some(Transport::HttpUpgrade {
            path: query.get("path").cloned(),
            host: query.get("host").cloned(),
        }),
        "xhttp" | "splithttp" => Some(Transport::Xhttp {
            path: query.get("path").cloned(),
            host: query.get("host").cloned(),
            mode: query.get("mode").cloned(),
        }),
        "tcp" | "" => Some(Transport::Tcp),
        other => return Err(format!("unsupported transport: {other}")),
    };

    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Vless,
        server,
        port,
        tls,
        transport,
        udp: None,
        config: ProtocolConfig::Vless {
            uuid,
            flow: query.get("flow").cloned().filter(|s| !s.is_empty()),
            packet_encoding: "xudp".into(),
        },
        source: Some("vless".into()),
        latency_ms: None,
        latency_at: None,
    })
}

fn parse_trojan_uri(line: &str) -> Result<ProxyNode, String> {
    let url = Url::parse(line).map_err(|e| format!("trojan url: {e}"))?;
    let password = percent_decode(url.username());
    if password.is_empty() {
        return Err("trojan: missing password".into());
    }
    let server = url
        .host_str()
        .ok_or_else(|| "trojan: missing host".to_string())?
        .to_string();
    let port = url
        .port()
        .ok_or_else(|| "trojan: missing port".to_string())?;
    let name = fragment_name(&url).unwrap_or_else(|| format!("trojan-{server}-{port}"));
    let query: BTreeMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let tls = Some(TlsConfig {
        enabled: true,
        server_name: query
            .get("sni")
            .cloned()
            .or_else(|| query.get("peer").cloned()),
        insecure: query.get("allowInsecure").map(|v| v == "1" || v == "true"),
        alpn: query.get("alpn").map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        }),
        utls_fingerprint: query.get("fp").cloned().and_then(|s| normalize_utls_fp(&s)),
        reality_public_key: None,
        reality_short_id: None,
    });

    let network = query
        .get("type")
        .map(|s| s.as_str())
        .unwrap_or("tcp")
        .to_ascii_lowercase();
    let transport = match network.as_str() {
        "ws" => {
            let mut headers = BTreeMap::new();
            if let Some(h) = query.get("host") {
                headers.insert("Host".into(), h.clone());
            }
            Some(Transport::Ws {
                path: query.get("path").cloned(),
                headers: if headers.is_empty() {
                    None
                } else {
                    Some(headers)
                },
                max_early_data: None,
            })
        }
        "grpc" => Some(Transport::Grpc {
            service_name: query.get("serviceName").cloned(),
        }),
        "http" | "h2" => Some(Transport::Http {
            path: query.get("path").cloned(),
            host: query.get("host").map(|h| vec![h.clone()]),
        }),
        "httpupgrade" => Some(Transport::HttpUpgrade {
            path: query.get("path").cloned(),
            host: query.get("host").cloned(),
        }),
        "xhttp" | "splithttp" => Some(Transport::Xhttp {
            path: query.get("path").cloned(),
            host: query.get("host").cloned(),
            mode: query.get("mode").cloned(),
        }),
        "tcp" | "" => Some(Transport::Tcp),
        other => return Err(format!("unsupported transport: {other}")),
    };

    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Trojan,
        server,
        port,
        tls,
        transport,
        udp: None,
        config: ProtocolConfig::Trojan { password },
        source: Some("trojan".into()),
        latency_ms: None,
        latency_at: None,
    })
}

/// snell://psk@host:port?version=4&obfs=http&obfs-host=bing.com#name
fn parse_snell_uri(line: &str) -> Result<ProxyNode, String> {
    let url = Url::parse(line).map_err(|e| format!("snell url: {e}"))?;
    let psk = percent_decode(url.username());
    if psk.is_empty() {
        return Err("snell: missing psk".into());
    }
    let server = url
        .host_str()
        .ok_or_else(|| "snell: missing host".to_string())?
        .to_string();
    let port = url
        .port()
        .ok_or_else(|| "snell: missing port".to_string())?;
    let name = fragment_name(&url).unwrap_or_else(|| format!("snell-{server}-{port}"));
    let query: BTreeMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let version = query
        .get("version")
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or(4);

    let userkey = query
        .get("userkey")
        .or_else(|| query.get("user-key"))
        .cloned()
        .filter(|s| !s.is_empty());

    let reuse = query
        .get("reuse")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let obfs_mode = query
        .get("obfs")
        .or_else(|| query.get("obfs-mode"))
        .or_else(|| query.get("obfs_mode"))
        .cloned();
    let obfs_host = query
        .get("obfs-host")
        .or_else(|| query.get("obfs_host"))
        .or_else(|| query.get("host"))
        .cloned();

    let mode = query.get("mode").cloned().filter(|m| {
        matches!(
            m.to_ascii_lowercase().as_str(),
            "default" | "unshaped" | "unsafe-raw" | "unsafe_raw"
        )
    });

    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Snell,
        server,
        port,
        tls: None,
        transport: None,
        udp: None,
        config: ProtocolConfig::Snell {
            psk,
            version,
            userkey,
            reuse,
            obfs_mode,
            obfs_host,
            mode,
        },
        source: Some("snell".into()),
        latency_ms: None,
        latency_at: None,
    })
}

/// anytls://password@host:port?insecure=1&sni=example.com#name
fn parse_anytls_uri(line: &str) -> Result<ProxyNode, String> {
    let url = Url::parse(line).map_err(|e| format!("anytls url: {e}"))?;
    let password = percent_decode(url.username());
    if password.is_empty() {
        return Err("anytls: missing password".into());
    }
    let server = url
        .host_str()
        .ok_or_else(|| "anytls: missing host".to_string())?
        .to_string();
    let port = url
        .port()
        .ok_or_else(|| "anytls: missing port".to_string())?;
    let name = fragment_name(&url).unwrap_or_else(|| format!("anytls-{server}-{port}"));
    let query: BTreeMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let insecure = query
        .get("insecure")
        .or_else(|| query.get("allowInsecure"))
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let tls = Some(TlsConfig {
        enabled: true,
        server_name: query
            .get("sni")
            .cloned()
            .or_else(|| query.get("peer").cloned())
            .or_else(|| query.get("servername").cloned()),
        insecure,
        alpn: query.get("alpn").map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        }),
        utls_fingerprint: query.get("fp").cloned().and_then(|s| normalize_utls_fp(&s)),
        reality_public_key: None,
        reality_short_id: None,
    });

    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::AnyTls,
        server,
        port,
        tls,
        transport: None,
        udp: None,
        config: ProtocolConfig::AnyTls { password },
        source: Some("anytls".into()),
        latency_ms: None,
        latency_at: None,
    })
}

fn parse_hysteria2_uri(line: &str) -> Result<ProxyNode, String> {
    // normalize hy2:// -> hysteria2:// for Url parser
    let normalized = if line.to_ascii_lowercase().starts_with("hy2://") {
        format!("hysteria2://{}", &line[6..])
    } else {
        line.to_string()
    };
    let url = Url::parse(&normalized).map_err(|e| format!("hysteria2 url: {e}"))?;
    let password = percent_decode(url.username());
    if password.is_empty() {
        return Err("hysteria2: missing password".into());
    }
    let server = url
        .host_str()
        .ok_or_else(|| "hysteria2: missing host".to_string())?
        .to_string();
    let port = url.port().unwrap_or(443);
    let name = fragment_name(&url).unwrap_or_else(|| format!("hy2-{server}-{port}"));
    let query: BTreeMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let tls = Some(TlsConfig {
        enabled: true,
        server_name: query.get("sni").cloned(),
        insecure: query.get("insecure").map(|v| v == "1" || v == "true"),
        alpn: None,
        utls_fingerprint: None,
        reality_public_key: None,
        reality_short_id: None,
    });

    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Hysteria2,
        server,
        port,
        tls,
        transport: None,
        udp: Some(true),
        config: ProtocolConfig::Hysteria2 {
            password,
            up_mbps: None,
            down_mbps: None,
            obfs: query.get("obfs").cloned(),
            obfs_password: query.get("obfs-password").cloned(),
        },
        source: Some("hysteria2".into()),
        latency_ms: None,
        latency_at: None,
    })
}

fn parse_tuic_uri(line: &str) -> Result<ProxyNode, String> {
    let url = Url::parse(line).map_err(|e| format!("tuic url: {e}"))?;
    let uuid = percent_decode(url.username());
    let password = url.password().map(percent_decode).unwrap_or_default();
    if uuid.is_empty() {
        return Err("tuic: missing uuid".into());
    }
    let server = url
        .host_str()
        .ok_or_else(|| "tuic: missing host".to_string())?
        .to_string();
    let port = url.port().unwrap_or(443);
    let name = fragment_name(&url).unwrap_or_else(|| format!("tuic-{server}-{port}"));
    let query: BTreeMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let tls = Some(TlsConfig {
        enabled: true,
        server_name: query
            .get("sni")
            .cloned()
            .or_else(|| query.get("peer").cloned()),
        insecure: query
            .get("allow_insecure")
            .or_else(|| query.get("insecure"))
            .map(|v| v == "1" || v == "true"),
        alpn: query.get("alpn").map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        }),
        utls_fingerprint: None,
        reality_public_key: None,
        reality_short_id: None,
    });

    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Tuic,
        server,
        port,
        tls,
        transport: None,
        udp: Some(true),
        config: ProtocolConfig::Tuic {
            uuid,
            password,
            congestion_control: query
                .get("congestion_control")
                .cloned()
                .or_else(|| query.get("congestion-controller").cloned()),
            udp_relay_mode: query.get("udp_relay_mode").cloned(),
            zero_rtt_handshake: query
                .get("reduce_rtt")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false),
        },
        source: Some("tuic".into()),
        latency_ms: None,
        latency_at: None,
    })
}

fn parse_socks_uri(line: &str) -> Result<ProxyNode, String> {
    let url = Url::parse(line).map_err(|e| format!("socks url: {e}"))?;
    let server = url
        .host_str()
        .ok_or_else(|| "socks: missing host".to_string())?
        .to_string();
    let port = url
        .port()
        .ok_or_else(|| "socks: missing port".to_string())?;
    let name = fragment_name(&url).unwrap_or_else(|| format!("socks-{server}-{port}"));
    let username = if url.username().is_empty() {
        None
    } else {
        Some(percent_decode(url.username()))
    };
    let password = url.password().map(percent_decode);

    Ok(ProxyNode {
        id: String::new(),
        name,
        protocol: Protocol::Socks5,
        server,
        port,
        tls: None,
        transport: None,
        udp: None,
        config: ProtocolConfig::Socks5 { username, password },
        source: Some("socks5".into()),
        latency_ms: None,
        latency_at: None,
    })
}

fn normalize_utls_fp(raw: &str) -> Option<String> {
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

fn split_fragment(s: &str) -> (&str, Option<String>) {
    if let Some((body, frag)) = s.split_once('#') {
        (body, Some(percent_decode(frag)))
    } else {
        (s, None)
    }
}

fn split_host_port(hostport: &str) -> Result<(String, u16), String> {
    // [ipv6]:port or host:port
    if let Some(rest) = hostport.strip_prefix('[') {
        let (host, port_part) = rest
            .split_once("]:")
            .ok_or_else(|| "ss: invalid ipv6 hostport".to_string())?;
        let port: u16 = port_part
            .parse()
            .map_err(|_| "ss: invalid port".to_string())?;
        return Ok((host.to_string(), port));
    }
    let (host, port_s) = hostport
        .rsplit_once(':')
        .ok_or_else(|| "ss: missing port".to_string())?;
    let port: u16 = port_s.parse().map_err(|_| "ss: invalid port".to_string())?;
    Ok((host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn parse_ss_plain() {
        let node =
            parse_uri_line("ss://aes-256-gcm:p%40ss@example.com:8388#%E9%A6%99%E6%B8%AF").unwrap();
        assert_eq!(node.protocol, Protocol::Shadowsocks);
        assert_eq!(node.server, "example.com");
        assert_eq!(node.port, 8388);
        assert_eq!(node.name, "香港");
        match node.config {
            ProtocolConfig::Shadowsocks {
                method, password, ..
            } => {
                assert_eq!(method, "aes-256-gcm");
                assert_eq!(password, "p@ss");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parse_ss_base64() {
        let inner = "chacha20-ietf-poly1305:pwd@1.2.3.4:1234";
        let b64 = general_purpose::STANDARD.encode(inner);
        let node = parse_uri_line(&format!("ss://{b64}#node1")).unwrap();
        assert_eq!(node.server, "1.2.3.4");
        assert_eq!(node.port, 1234);
    }

    #[test]
    fn parse_vmess_json() {
        let json = r#"{"v":"2","ps":"VM-1","add":"vm.example.com","port":"443","id":"11111111-1111-1111-1111-111111111111","aid":"0","net":"ws","type":"none","host":"cdn.example.com","path":"/ray","tls":"tls","sni":"cdn.example.com"}"#;
        let b64 = general_purpose::STANDARD.encode(json);
        let node = parse_uri_line(&format!("vmess://{b64}")).unwrap();
        assert_eq!(node.name, "VM-1");
        assert_eq!(node.protocol, Protocol::Vmess);
        assert!(node.tls.as_ref().unwrap().enabled);
        assert!(matches!(node.transport, Some(Transport::Ws { .. })));
    }

    #[test]
    fn parse_vless_reality() {
        let uri = "vless://22222222-2222-2222-2222-222222222222@vl.example.com:443?encryption=none&flow=xtls-rprx-vision&security=reality&sni=www.microsoft.com&fp=chrome&pbk=pubkey&sid=abcd&type=tcp#VL-Reality";
        let node = parse_uri_line(uri).unwrap();
        assert_eq!(node.name, "VL-Reality");
        assert_eq!(
            node.tls.as_ref().unwrap().reality_public_key.as_deref(),
            Some("pubkey")
        );
        match node.config {
            ProtocolConfig::Vless { flow, .. } => {
                assert_eq!(flow.as_deref(), Some("xtls-rprx-vision"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parse_trojan() {
        let uri = "trojan://secret@tj.example.com:443?sni=tj.example.com#TJ";
        let node = parse_uri_line(uri).unwrap();
        assert_eq!(node.protocol, Protocol::Trojan);
        assert_eq!(node.name, "TJ");
    }

    #[test]
    fn vless_xhttp_link_parses_and_unknown_type_rejected() {
        // type=xhttp used to silently become Transport::Tcp — the node parsed
        // fine and then could never connect. It is representable now (Xray
        // serves it via multi-core delegation).
        let uri = "vless://22222222-2222-2222-2222-222222222222@vl.example.com:443?encryption=none&security=tls&sni=cdn.example.com&type=xhttp&path=%2Fupload&mode=stream-up#VL-XHTTP";
        let node = parse_uri_line(uri).unwrap();
        match node.transport {
            Some(Transport::Xhttp { path, host, mode }) => {
                assert_eq!(path.as_deref(), Some("/upload"));
                // No host param in this link (sni ≠ transport host).
                assert_eq!(host, None);
                assert_eq!(mode.as_deref(), Some("stream-up"));
            }
            other => panic!("expected xhttp transport, got {other:?}"),
        }

        // Unknown transports must be an explicit error, never a Tcp downgrade.
        let uri = "vless://22222222-2222-2222-2222-222222222222@vl.example.com:443?type=kcp#VL-KCP";
        let err = parse_uri_line(uri).unwrap_err();
        assert!(err.contains("unsupported transport: kcp"), "got: {err}");

        // httpupgrade is representable — must parse, not degrade.
        let uri = "vless://22222222-2222-2222-2222-222222222222@vl.example.com:443?encryption=none&security=tls&type=httpupgrade&path=%2Fup&host=cdn.example.com#VL-UP";
        let node = parse_uri_line(uri).unwrap();
        assert!(matches!(
            node.transport,
            Some(Transport::HttpUpgrade { .. })
        ));
    }

    #[test]
    fn parse_hy2() {
        let uri = "hysteria2://pwd@hy2.example.com:443?sni=hy2.example.com&insecure=1#HY2";
        let node = parse_uri_line(uri).unwrap();
        assert_eq!(node.protocol, Protocol::Hysteria2);
        assert_eq!(node.name, "HY2");
    }

    #[test]
    fn parse_anytls() {
        let uri = "anytls://E98E62DE-B54D-04BA-4ADE-913F610F8EE6@sdfaxxfw.s4b4.com:40001?insecure=1&sni=ac9b90d0.sdsarsdg.xin#CN|香港|负载均衡";
        let node = parse_uri_line(uri).unwrap();
        assert_eq!(node.protocol, Protocol::AnyTls);
        assert_eq!(node.server, "sdfaxxfw.s4b4.com");
        assert_eq!(node.port, 40001);
        assert_eq!(node.name, "CN|香港|负载均衡");
        let tls = node.tls.as_ref().unwrap();
        assert!(tls.enabled);
        assert_eq!(tls.insecure, Some(true));
        assert_eq!(tls.server_name.as_deref(), Some("ac9b90d0.sdsarsdg.xin"));
        match node.config {
            ProtocolConfig::AnyTls { password } => {
                assert_eq!(password, "E98E62DE-B54D-04BA-4ADE-913F610F8EE6");
            }
            _ => panic!("expected AnyTls config"),
        }
    }

    #[test]
    fn parse_snell() {
        let uri = "snell://mypsk@sn.example.com:44046?version=4&obfs=http&obfs-host=bing.com#SN-HK";
        let node = parse_uri_line(uri).unwrap();
        assert_eq!(node.protocol, Protocol::Snell);
        assert_eq!(node.server, "sn.example.com");
        assert_eq!(node.port, 44046);
        assert_eq!(node.name, "SN-HK");
        match node.config {
            ProtocolConfig::Snell {
                psk,
                version,
                obfs_mode,
                obfs_host,
                ..
            } => {
                assert_eq!(psk, "mypsk");
                assert_eq!(version, 4);
                assert_eq!(obfs_mode.as_deref(), Some("http"));
                assert_eq!(obfs_host.as_deref(), Some("bing.com"));
            }
            _ => panic!("expected Snell config"),
        }
    }

    #[test]
    fn parse_http_proxy() {
        let node = parse_uri_line("https://user:pass@proxy.example:8443#HTTPS").unwrap();
        assert_eq!(node.protocol, Protocol::Http);
        assert!(node.tls.as_ref().is_some_and(|t| t.enabled));
        assert!(matches!(
            node.config,
            ProtocolConfig::Http {
                username: Some(_),
                password: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn parse_hysteria_v1() {
        let node = parse_uri_line(
            "hysteria://secret@hy.example:443?upmbps=20&downmbps=100&sni=hy.example#HY",
        )
        .unwrap();
        assert_eq!(node.protocol, Protocol::Hysteria);
        assert!(matches!(
            node.config,
            ProtocolConfig::Hysteria {
                up_mbps: Some(20),
                down_mbps: Some(100),
                ..
            }
        ));
    }

    #[test]
    fn parse_naive_proxy() {
        let node = parse_uri_line("naive+https://user:pass@naive.example:443#Naive").unwrap();
        assert_eq!(node.protocol, Protocol::Naive);
    }
}
