use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Supported outbound protocols for MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Shadowsocks,
    Vmess,
    Vless,
    Trojan,
    Hysteria2,
    Tuic,
    Socks5,
    Http,
    Hysteria,
    ShadowTls,
    Ssh,
    Naive,
    Tor,
    WireGuard,
    /// AnyTLS (sing-box ≥ 1.12).
    AnyTls,
    /// Snell (sing-box ≥ 1.14, versions 4 / 6).
    Snell,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shadowsocks => "shadowsocks",
            Self::Vmess => "vmess",
            Self::Vless => "vless",
            Self::Trojan => "trojan",
            Self::Hysteria2 => "hysteria2",
            Self::Tuic => "tuic",
            Self::Socks5 => "socks5",
            Self::Http => "http",
            Self::Hysteria => "hysteria",
            Self::ShadowTls => "shadowtls",
            Self::Ssh => "ssh",
            Self::Naive => "naive",
            Self::Tor => "tor",
            Self::WireGuard => "wireguard",
            Self::AnyTls => "anytls",
            Self::Snell => "snell",
        }
    }

    pub fn from_clash_type(t: &str) -> Option<Self> {
        match t.to_ascii_lowercase().as_str() {
            "ss" | "shadowsocks" => Some(Self::Shadowsocks),
            "vmess" => Some(Self::Vmess),
            "vless" => Some(Self::Vless),
            "trojan" => Some(Self::Trojan),
            "hysteria2" | "hy2" => Some(Self::Hysteria2),
            "tuic" => Some(Self::Tuic),
            "socks5" | "socks" => Some(Self::Socks5),
            "http" | "https" => Some(Self::Http),
            "hysteria" | "hy" => Some(Self::Hysteria),
            "shadowtls" => Some(Self::ShadowTls),
            "ssh" => Some(Self::Ssh),
            "naive" => Some(Self::Naive),
            "tor" => Some(Self::Tor),
            "wireguard" | "wg" => Some(Self::WireGuard),
            "anytls" => Some(Self::AnyTls),
            "snell" => Some(Self::Snell),
            _ => None,
        }
    }

    pub fn from_singbox_type(t: &str) -> Option<Self> {
        Self::from_clash_type(t)
    }

    /// True for protocols that never accept a plain TCP connect on
    /// `server:port` (QUIC/UDP transport). Direct TCP latency probing
    /// against these always times out regardless of node health.
    pub fn is_udp_only(self) -> bool {
        matches!(self, Self::Hysteria2 | Self::Hysteria | Self::Tuic)
    }

    /// Whether the Xray core can serve this protocol as an outbound. Used to
    /// hide unusable nodes from listings while Xray is the active core
    /// (single source of truth — `CoreKind::supports` delegates here).
    ///
    /// Hysteria2 is protocol-level supported (Xray's `hysteria` transport,
    /// forced to `version: 2`), but nodes using salamander obfs are rejected
    /// per-node in `config/xray.rs` — Xray has no field for it.
    pub fn xray_supported(self) -> bool {
        matches!(
            self,
            Self::Shadowsocks
                | Self::Vmess
                | Self::Vless
                | Self::Trojan
                | Self::Hysteria2
                | Self::Socks5
                | Self::Http
                | Self::WireGuard
        )
    }

    /// Whether the mihomo (Clash Meta) core can serve this protocol as an
    /// outbound. mihomo is the canonical Clash kernel — full coverage:
    /// SS(+plugins) / VMess / VLESS (incl. REALITY + Vision) / Trojan /
    /// Hysteria(1|2) / TUIC / WireGuard / AnyTLS / Snell / SOCKS5 / HTTP /
    /// SSH. Only Naive and Tor (external-executable shapes) are missing,
    /// plus a standalone ShadowTLS proxy type (ss+shadow-tls plugin would
    /// need its own field mapping).
    pub fn mihomo_supported(self) -> bool {
        !matches!(self, Self::Naive | Self::Tor | Self::ShadowTls)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TlsConfig {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insecure: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpn: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utls_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reality_public_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reality_short_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Transport {
    Tcp,
    Ws {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        headers: Option<std::collections::BTreeMap<String, String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_early_data: Option<u32>,
    },
    Grpc {
        #[serde(skip_serializing_if = "Option::is_none")]
        service_name: Option<String>,
    },
    Http {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        host: Option<Vec<String>>,
    },
    HttpUpgrade {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        host: Option<String>,
    },
    /// Xray-only split-HTTP transport (a.k.a. splithttp). sing-box has no
    /// equivalent — nodes carrying this transport must be served by the
    /// Xray sidecar (multi-core mode) or the Xray core; the sing-box
    /// generator rejects them with a pointed error.
    Xhttp {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        host: Option<String>,
        /// `auto` | `packet-up` | `stream-up` | `stream-one` (Xray default auto).
        #[serde(skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
    },
}

/// Parameters for the shadow-tls SIP003 plugin (Clash `plugin-opts` under
/// `plugin: shadow-tls`). Rendered as a separate sing-box `shadowtls`
/// outbound the ss outbound detours through, not as a plugin_opts string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowTlsOpts {
    pub host: String,
    pub password: String,
    /// shadow-tls protocol version (1-3); mihomo defaults to 3 when absent.
    pub version: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

/// Protocol-specific fields needed to build a sing-box outbound later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "lowercase")]
pub enum ProtocolConfig {
    Shadowsocks {
        method: String,
        password: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        plugin_opts: Option<String>,
        /// sing-box has no SIP003 arg-string form for shadow-tls: it's a
        /// separate `shadowtls` outbound that this ss outbound detours
        /// through. Kept out of `plugin`/`plugin_opts` for that reason.
        #[serde(skip_serializing_if = "Option::is_none")]
        shadow_tls: Option<ShadowTlsOpts>,
    },
    Vmess {
        uuid: String,
        #[serde(default)]
        alter_id: u16,
        #[serde(default = "default_vmess_security")]
        security: String,
    },
    Vless {
        uuid: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        flow: Option<String>,
        #[serde(default = "default_vless_packet_encoding")]
        packet_encoding: String,
    },
    Trojan {
        password: String,
    },
    Hysteria2 {
        password: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        up_mbps: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        down_mbps: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        obfs: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        obfs_password: Option<String>,
    },
    Tuic {
        uuid: String,
        password: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        congestion_control: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        udp_relay_mode: Option<String>,
        #[serde(default)]
        zero_rtt_handshake: bool,
    },
    Socks5 {
        #[serde(skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        password: Option<String>,
    },
    Http {
        #[serde(skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        password: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Hysteria {
        auth: String,
        #[serde(default)]
        auth_base64: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        up_mbps: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        down_mbps: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        obfs: Option<String>,
    },
    ShadowTls {
        version: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        password: Option<String>,
    },
    Ssh {
        user: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        password: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        private_key: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        private_key_passphrase: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        host_key: Vec<String>,
    },
    Naive {
        username: String,
        password: String,
        #[serde(default)]
        quic: bool,
    },
    Tor {
        executable_path: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extra_args: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data_directory: Option<String>,
    },
    /// sing-box 1.13+ WireGuard endpoint represented as a selectable node.
    WireGuard {
        local_address: Vec<String>,
        private_key: String,
        peer_public_key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pre_shared_key: Option<String>,
        #[serde(default)]
        reserved: Vec<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        mtu: Option<u32>,
    },
    /// AnyTLS password (often a UUID in share links).
    AnyTls {
        password: String,
    },
    /// Snell PSK + version / obfs (Clash) or mode (v6).
    Snell {
        psk: String,
        /// Protocol version; sing-box accepts 4 or 6 (v5 ≈ v4 wire).
        version: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        userkey: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reuse: Option<bool>,
        /// v4 HTTP obfs: `http` / `none` / `tls` (mapped carefully for sing-box).
        #[serde(skip_serializing_if = "Option::is_none")]
        obfs_mode: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        obfs_host: Option<String>,
        /// v6 traffic shaping: default / unshaped / unsafe-raw.
        #[serde(skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
    },
}

fn default_vmess_security() -> String {
    "auto".into()
}

fn default_vless_packet_encoding() -> String {
    "xudp".into()
}

/// Normalized proxy node — intermediate model between subscription and sing-box config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyNode {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub server: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp: Option<bool>,
    pub config: ProtocolConfig,
    /// Original clash `type` string or uri scheme for debugging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Last measured latency in milliseconds (TCP connect or delay API).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u32>,
    /// Unix timestamp of last latency test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_at: Option<i64>,
}

impl ProxyNode {
    /// Stable id without subscription context (subscription layer may re-hash later).
    pub fn compute_id(name: &str, server: &str, port: u16, protocol: Protocol) -> String {
        let mut hasher = Sha256::new();
        hasher.update(name.as_bytes());
        hasher.update(b"|");
        hasher.update(server.as_bytes());
        hasher.update(b"|");
        hasher.update(port.to_string().as_bytes());
        hasher.update(b"|");
        hasher.update(protocol.as_str().as_bytes());
        let digest = hasher.finalize();
        hex::encode(&digest[..16])
    }

    pub fn with_computed_id(mut self) -> Self {
        self.id = Self::compute_id(&self.name, &self.server, self.port, self.protocol);
        self
    }

    /// Same outbound credentials — ignore display-name fragments.
    pub fn identity_key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.protocol.as_str(),
            self.server,
            self.port,
            config_identity(&self.config)
        )
    }

    /// One selectable node: same backend *and* same display name.
    /// Airport lists often reuse host/port/auth under different names.
    pub fn instance_key(&self) -> String {
        format!("{}|{}", self.identity_key(), self.name)
    }

    /// Ensure node ids are unique on the outbound-tag prefix (`id[..16]`).
    ///
    /// `compute_id` hashes `name|server|port|protocol` without credentials,
    /// while import dedupes by `instance_key` (credentials included) — so
    /// two same-named nodes that differ only by password/uuid survive as
    /// distinct instances yet hash to the same id, producing duplicate
    /// `node-<id[..16]>` outbound/endpoint tags and a `sing-box check`
    /// failure. Later occurrences are re-hashed with a deterministic salt;
    /// the first keeps its id (list order persists in the store, so this is
    /// stable across runs). Returns how many ids were rewritten.
    pub fn ensure_unique_ids<'a, I>(nodes: I) -> usize
    where
        I: Iterator<Item = &'a mut ProxyNode>,
    {
        let mut seen = std::collections::HashSet::new();
        let mut renamed = 0;
        for node in nodes {
            let mut salt = 1;
            while !seen.insert(tag_prefix(&node.id)) {
                salt += 1;
                node.id = Self::compute_id(
                    &format!("dup:{}:{salt}", node.id),
                    &node.server,
                    node.port,
                    node.protocol,
                );
                renamed += 1;
            }
        }
        renamed
    }
}

/// The slice of an id that `outbound_tag` renders into config tags.
fn tag_prefix(id: &str) -> String {
    id[..id.len().min(16)].to_string()
}

fn config_identity(config: &ProtocolConfig) -> String {
    match config {
        ProtocolConfig::Shadowsocks {
            password, method, ..
        } => {
            format!("{method}|{password}")
        }
        ProtocolConfig::Vmess { uuid, .. } | ProtocolConfig::Vless { uuid, .. } => uuid.clone(),
        ProtocolConfig::Trojan { password } | ProtocolConfig::AnyTls { password } => {
            password.clone()
        }
        ProtocolConfig::Hysteria2 { password, .. } => password.clone(),
        ProtocolConfig::Tuic { uuid, password, .. } => format!("{uuid}|{password}"),
        ProtocolConfig::Socks5 { username, password }
        | ProtocolConfig::Http {
            username, password, ..
        } => format!(
            "{}|{}",
            username.clone().unwrap_or_default(),
            password.clone().unwrap_or_default()
        ),
        ProtocolConfig::Hysteria { auth, .. } => auth.clone(),
        ProtocolConfig::ShadowTls { password, .. } => password.clone().unwrap_or_default(),
        ProtocolConfig::Ssh {
            user,
            password,
            private_key,
            ..
        } => format!(
            "{user}|{}|{}",
            password.clone().unwrap_or_default(),
            private_key.clone().unwrap_or_default()
        ),
        ProtocolConfig::Naive {
            username, password, ..
        } => format!("{username}|{password}"),
        ProtocolConfig::Tor {
            executable_path, ..
        } => executable_path.clone(),
        ProtocolConfig::WireGuard {
            private_key,
            peer_public_key,
            ..
        } => format!("{private_key}|{peer_public_key}"),
        ProtocolConfig::Snell { psk, version, .. } => format!("{version}|{psk}"),
    }
}

/// Compact node facts for the config list and add-form preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSummary {
    pub protocol: String,
    pub server: String,
    pub port: u16,
    pub tls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<String>,
}

impl NodeSummary {
    pub fn from_node(node: &ProxyNode) -> Self {
        let tls = node.tls.as_ref().is_some_and(|t| t.enabled);
        let transport = match &node.transport {
            Some(Transport::Ws { .. }) => Some("ws".into()),
            Some(Transport::Grpc { .. }) => Some("grpc".into()),
            Some(Transport::Http { .. }) => Some("http".into()),
            Some(Transport::HttpUpgrade { .. }) => Some("httpupgrade".into()),
            Some(Transport::Xhttp { .. }) => Some("xhttp".into()),
            Some(Transport::Tcp) | None => None,
        };
        let extra = node_extra(node);
        Self {
            protocol: node.protocol.as_str().to_string(),
            server: node.server.clone(),
            port: node.port,
            tls,
            transport,
            extra,
        }
    }
}

fn node_extra(node: &ProxyNode) -> Option<String> {
    if node
        .tls
        .as_ref()
        .and_then(|t| t.reality_public_key.as_ref())
        .is_some()
    {
        return Some("Reality".into());
    }
    match &node.config {
        ProtocolConfig::Shadowsocks { method, .. } => Some(method.clone()),
        ProtocolConfig::Vless { flow, .. } => flow.clone().filter(|s| !s.is_empty()),
        ProtocolConfig::Vmess { security, .. } => Some(security.clone()),
        ProtocolConfig::Snell { version, .. } => Some(format!("v{version}")),
        _ => None,
    }
}

/// Result of parsing a subscription body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseResult {
    pub nodes: Vec<ProxyNode>,
    /// Skipped entries (unsupported type / invalid fields).
    pub skipped: Vec<SkippedProxy>,
    pub format: SubscriptionFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedProxy {
    pub name: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionFormat {
    ClashYaml,
    UriList,
    Base64UriList,
    SingboxJson,
    Manual,
}

/// Flattened form payload for a manually entered node.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualNodeDraft {
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_opts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alter_id: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_encoding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_mbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down_mbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obfs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obfs_password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub congestion_control: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp_relay_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zero_rtt_handshake: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key_passphrase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_public_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_shared_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sni: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insecure: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reality_public_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reality_short_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ss_node(id: &str, name: &str, password: &str) -> ProxyNode {
        ProxyNode {
            id: id.to_string(),
            name: name.to_string(),
            protocol: Protocol::Shadowsocks,
            server: "example.com".into(),
            port: 8388,
            tls: None,
            transport: None,
            udp: None,
            config: ProtocolConfig::Shadowsocks {
                method: "aes-128-gcm".into(),
                password: password.into(),
                plugin: None,
                plugin_opts: None,
                shadow_tls: None,
            },
            source: None,
            latency_ms: None,
            latency_at: None,
        }
    }

    #[test]
    fn ensure_unique_ids_renames_only_duplicates() {
        let base = ProxyNode::compute_id("香港 01", "example.com", 8388, Protocol::Shadowsocks);
        let other = ProxyNode::compute_id("东京 01", "example.com", 8388, Protocol::Shadowsocks);
        let mut nodes = vec![
            ss_node(&base, "香港 01", "pass-a"),
            ss_node(&base, "香港 01", "pass-b"),
            ss_node(&other, "东京 01", "pass-c"),
        ];

        let renamed = ProxyNode::ensure_unique_ids(nodes.iter_mut());
        assert_eq!(renamed, 1);
        // First occurrence keeps its id; distinct node untouched.
        assert_eq!(nodes[0].id, base);
        assert_eq!(nodes[2].id, other);
        // Duplicate got a different id, distinct on the tag prefix.
        assert_ne!(nodes[1].id, base);
        let prefixes: Vec<String> = nodes.iter().map(|n| tag_prefix(&n.id)).collect();
        let mut unique = prefixes.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(prefixes.len(), unique.len());
    }

    #[test]
    fn ensure_unique_ids_is_deterministic() {
        let base = ProxyNode::compute_id("同号节点", "example.com", 8388, Protocol::Shadowsocks);
        let build = || {
            vec![
                ss_node(&base, "同号节点", "pass-a"),
                ss_node(&base, "同号节点", "pass-b"),
            ]
        };
        let mut first = build();
        let mut second = build();
        ProxyNode::ensure_unique_ids(first.iter_mut());
        ProxyNode::ensure_unique_ids(second.iter_mut());
        assert_eq!(first[1].id, second[1].id);
        assert_ne!(first[0].id, first[1].id);
    }
}
