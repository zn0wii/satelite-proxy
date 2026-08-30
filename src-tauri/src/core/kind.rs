//! Core kind descriptor — everything that differs between sing-box, Xray and
//! mihomo at the process-management level (binary name, release asset naming,
//! CLI arguments, version output, spawn environment). Config generation lives
//! separately: `config/builder.rs` (sing-box), `config/xray.rs` (Xray) and
//! `config/mihomo.rs` (mihomo, Clash YAML).

use crate::domain::Protocol;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreKind {
    SingBox,
    Xray,
    Mihomo,
}

impl CoreKind {
    pub fn binary_name(self) -> &'static str {
        match self {
            Self::SingBox => {
                if cfg!(windows) {
                    "sing-box.exe"
                } else {
                    "sing-box"
                }
            }
            Self::Xray => {
                if cfg!(windows) {
                    "xray.exe"
                } else {
                    "xray"
                }
            }
            Self::Mihomo => {
                if cfg!(windows) {
                    "mihomo.exe"
                } else {
                    "mihomo"
                }
            }
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::SingBox => "sing-box",
            Self::Xray => "Xray",
            Self::Mihomo => "mihomo",
        }
    }

    /// Stable token used by settings storage and the frontend.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SingBox => "singbox",
            Self::Xray => "xray",
            Self::Mihomo => "mihomo",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "xray" => Self::Xray,
            "mihomo" => Self::Mihomo,
            _ => Self::SingBox,
        }
    }

    /// `owner/repo` for GitHub release queries.
    pub fn repo(self) -> &'static str {
        match self {
            Self::SingBox => "SagerNet/sing-box",
            Self::Xray => "XTLS/Xray-core",
            Self::Mihomo => "MetaCubeX/mihomo",
        }
    }

    /// Pinned version used only when the GitHub API is unreachable.
    pub fn fallback_version(self) -> &'static str {
        match self {
            Self::SingBox => "v1.13.15",
            Self::Xray => "v26.3.27",
            Self::Mihomo => "v1.19.30",
        }
    }

    /// Release asset name for a platform. The naming schemes differ:
    /// sing-box `sing-box-{ver}-darwin-arm64.tar.gz`, Xray
    /// `Xray-macos-arm64-v8a.zip` (no version, `64` not `amd64`), mihomo
    /// `mihomo-{plat}-v{ver}.zip|.gz` (same plat suffixes as sing-box;
    /// Windows zips carry a versioned inner exe, darwin/linux ship a bare
    /// gzipped binary).
    pub fn asset_name(self, version: &str, platform_suffix: &str, is_windows: bool) -> String {
        let ver_num = version.trim_start_matches('v');
        match self {
            Self::SingBox => {
                let ext = if is_windows { "zip" } else { "tar.gz" };
                format!("sing-box-{ver_num}-{platform_suffix}.{ext}")
            }
            Self::Xray => format!("Xray-{platform_suffix}.zip"),
            Self::Mihomo => {
                let ext = if is_windows { "zip" } else { "gz" };
                format!("mihomo-{platform_suffix}-v{ver_num}.{ext}")
            }
        }
    }

    /// CLI arguments that print the version.
    pub fn version_args(self) -> &'static [&'static str] {
        match self {
            Self::SingBox => &["version"],
            Self::Xray => &["-version"],
            Self::Mihomo => &["-v"],
        }
    }

    /// Extract the version from `version_args` output. sing-box prints
    /// `sing-box version 1.13.15 (...)`; Xray prints
    /// `Xray 26.3.27 (Custom) ... (go1.24 ...)`; mihomo prints
    /// `Mihomo Meta v1.19.30 windows amd64 ...`.
    pub fn parse_version_output(self, out: &str) -> Option<String> {
        for line in out.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match self {
                Self::SingBox => {
                    if let Some(rest) = line.strip_prefix("sing-box version ") {
                        return Some(rest.split_whitespace().next()?.to_string());
                    }
                }
                Self::Xray => {
                    if let Some(rest) = line.strip_prefix("Xray ") {
                        return Some(rest.split_whitespace().next()?.to_string());
                    }
                }
                Self::Mihomo => {
                    if let Some(rest) = line.strip_prefix("Mihomo Meta ") {
                        return Some(
                            rest.trim_start_matches('v')
                                .split_whitespace()
                                .next()?
                                .to_string(),
                        );
                    }
                }
            }
            // Fallback: first token that starts with a digit.
            if let Some(token) = line
                .split_whitespace()
                .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))
            {
                return Some(token.to_string());
            }
            return None;
        }
        None
    }

    /// Full CLI argument vector that validates `config` without starting
    /// the server. sing-box: `check -c <file>`; Xray: `run -test -c
    /// <file>`; mihomo: `-t -f <file> -d <home>` (the home dir hosts its
    /// geodata and caches; config paths must be absolute under `-d`).
    pub fn check_command_args(self, config: &Path) -> Vec<String> {
        let config = config.display().to_string();
        match self {
            Self::SingBox => vec!["check".into(), "-c".into(), config],
            Self::Xray => vec!["run".into(), "-test".into(), "-c".into(), config],
            Self::Mihomo => {
                let mut args = vec!["-t".into(), "-f".into(), config.clone()];
                args.extend(mihomo_home_args(&config));
                args
            }
        }
    }

    /// Full CLI argument vector that runs the core with `config`. sing-box
    /// and Xray: `run -c <file>`; mihomo: `-f <file> -d <home>` — no `run`
    /// subcommand, the config alone starts the server.
    pub fn run_command_args(self, config: &Path) -> Vec<String> {
        let config = config.display().to_string();
        match self {
            Self::SingBox | Self::Xray => vec!["run".into(), "-c".into(), config],
            Self::Mihomo => {
                let mut args = vec!["-f".into(), config.clone()];
                args.extend(mihomo_home_args(&config));
                args
            }
        }
    }

    /// Infer the kind from a binary path's file stem (e.g. the Windows
    /// elevated-helper entry point receives only the binary path).
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn from_binary_path(path: &std::path::Path) -> Self {
        match path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref()
        {
            Some("xray") => Self::Xray,
            Some("mihomo") => Self::Mihomo,
            _ => Self::SingBox,
        }
    }

    /// Environment variables the child process needs. Xray resolves
    /// geosite.dat / geoip.dat (and TLS certs) relative to `bin_dir`.
    /// mihomo needs none — its wintun.dll is looked up next to the exe
    /// (`bin/wintun.dll`, shared with the Xray staging).
    pub fn spawn_env(self, bin_dir: &std::path::Path) -> Vec<(String, String)> {
        match self {
            Self::SingBox => Vec::new(),
            Self::Xray => {
                let dir = bin_dir.display().to_string();
                vec![
                    ("XRAY_LOCATION_ASSET".into(), dir.clone()),
                    ("XRAY_LOCATION_CERT".into(), dir),
                ]
            }
            Self::Mihomo => Vec::new(),
        }
    }

    /// Log file / display prefix.
    pub fn log_prefix(self) -> &'static str {
        match self {
            Self::SingBox => "sing-box",
            Self::Xray => "xray",
            Self::Mihomo => "mihomo",
        }
    }

    /// Version file name inside the app-data `bin/` directory. sing-box keeps
    /// the historical `version.txt`; the other cores use prefixed names so
    /// all three can coexist without clobbering each other's metadata.
    pub fn version_file_name(self) -> &'static str {
        match self {
            Self::SingBox => "version.txt",
            Self::Xray => "xray-version.txt",
            Self::Mihomo => "mihomo-version.txt",
        }
    }

    /// Whether this core can serve the given outbound protocol. Xray and
    /// mihomo each delegate to their `Protocol::*_supported` counterpart —
    /// the single source of truth lives on the protocol enum.
    pub fn supports(self, protocol: Protocol) -> bool {
        match self {
            Self::SingBox => true,
            Self::Xray => protocol.xray_supported(),
            Self::Mihomo => protocol.mihomo_supported(),
        }
    }

    /// Whether this core can serve the NODE as a whole — protocol plus the
    /// per-node shapes a core cannot represent. Xray rejects REALITY over
    /// transports other than tcp/grpc/xhttp and hysteria2 with obfs
    /// (generation-time rules); mihomo (canonical Clash Meta, proper uTLS)
    /// lacks our ss+shadow-tls detour shape and the xhttp transport;
    /// sing-box serves every protocol it parses except xhttp — those nodes
    /// stay listed and are force-delegated to the Xray sidecar when
    /// multi-core mode is on (the sing-box generator rejects them natively).
    pub fn supports_node(self, node: &crate::domain::ProxyNode) -> bool {
        if !self.supports(node.protocol) {
            return false;
        }
        if self == Self::Xray {
            let reality = node
                .tls
                .as_ref()
                .is_some_and(|t| t.enabled && t.reality_public_key.is_some());
            if reality
                && !matches!(
                    node.transport.as_ref(),
                    None | Some(crate::domain::Transport::Tcp)
                        | Some(crate::domain::Transport::Grpc { .. })
                        | Some(crate::domain::Transport::Xhttp { .. })
                )
            {
                return false;
            }
            // Xray's hysteria transport has no obfs field.
            if let crate::domain::ProtocolConfig::Hysteria2 { obfs, .. } = &node.config {
                if obfs.as_deref().is_some_and(|o| !o.is_empty()) {
                    return false;
                }
            }
        }
        if matches!(
            &node.config,
            crate::domain::ProtocolConfig::Shadowsocks {
                shadow_tls: Some(_),
                ..
            }
        ) && matches!(self, Self::Mihomo | Self::Xray)
        {
            return false;
        }
        if matches!(self, Self::Mihomo)
            && matches!(node.transport, Some(crate::domain::Transport::Xhttp { .. }))
        {
            return false;
        }
        true
    }
}

/// `-d <home>` argument pair for mihomo. The home dir is derived from the
/// config location: active.yaml lives in `<app_data>/config/`, so the
/// mihomo home (holding `Country.mmdb` + `geosite.dat`, see `core::assets`)
/// is the sibling directory `<app_data>/mihomo/`. Keeping it separate from
/// `bin/` is deliberate — mihomo's `geosite.dat` (MetaCubeX .mrs) would
/// collide with Xray's v2ray-format `bin/geosite.dat` of the same name.
fn mihomo_home_args(config: &str) -> Vec<String> {
    let home = Path::new(config)
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("mihomo"))
        .unwrap_or_else(|| Path::new("mihomo").to_path_buf());
    vec!["-d".into(), home.display().to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_output() {
        assert_eq!(
            CoreKind::SingBox
                .parse_version_output("sing-box version 1.13.15 (go1.24)\n")
                .as_deref(),
            Some("1.13.15")
        );
        assert_eq!(
            CoreKind::Xray
                .parse_version_output("Xray 26.3.27 (Custom) 1234560 (go1.24)\n")
                .as_deref(),
            Some("26.3.27")
        );
        assert_eq!(
            CoreKind::Mihomo
                .parse_version_output("Mihomo Meta 0.21.0\n")
                .as_deref(),
            Some("0.21.0")
        );
        // Fallback: first digit-leading token.
        assert_eq!(
            CoreKind::Xray
                .parse_version_output("Xray 1.2.3 weird output\n")
                .as_deref(),
            Some("1.2.3")
        );
        assert_eq!(CoreKind::Xray.parse_version_output(""), None);
    }

    #[test]
    fn asset_names_match_release_schemes() {
        assert_eq!(
            CoreKind::SingBox.asset_name("1.13.15", "windows-amd64", true),
            "sing-box-1.13.15-windows-amd64.zip"
        );
        assert_eq!(
            CoreKind::SingBox.asset_name("1.13.15", "darwin-arm64", false),
            "sing-box-1.13.15-darwin-arm64.tar.gz"
        );
        assert_eq!(
            CoreKind::Xray.asset_name("26.3.27", "windows-64", true),
            "Xray-windows-64.zip"
        );
        assert_eq!(
            CoreKind::Xray.asset_name("26.3.27", "macos-arm64-v8a", false),
            "Xray-macos-arm64-v8a.zip"
        );
        assert_eq!(
            CoreKind::Mihomo.asset_name("1.19.30", "windows-amd64", true),
            "mihomo-windows-amd64-v1.19.30.zip"
        );
        assert_eq!(
            CoreKind::Mihomo.asset_name("1.19.30", "darwin-arm64", false),
            "mihomo-darwin-arm64-v1.19.30.gz"
        );
    }

    #[test]
    fn mihomo_command_args_include_home_dir() {
        let config = Path::new("/data/config/active.yaml");
        let expect_args = |mut head: Vec<String>, config: &str| {
            head.push("-f".into());
            head.push(config.to_string());
            head.push("-d".into());
            head.push(
                Path::new(config)
                    .parent()
                    .and_then(|p| p.parent())
                    .map(|root| root.join("mihomo"))
                    .unwrap_or_else(|| Path::new("mihomo").to_path_buf())
                    .display()
                    .to_string(),
            );
            head
        };
        let cfg = "/data/config/active.yaml";
        assert_eq!(
            CoreKind::Mihomo.check_command_args(config),
            expect_args(vec!["-t".into()], cfg)
        );
        assert_eq!(
            CoreKind::Mihomo.run_command_args(config),
            expect_args(vec![], cfg)
        );
        // JSON cores keep the historical shape.
        assert_eq!(
            CoreKind::SingBox.check_command_args(config),
            ["check", "-c", "/data/config/active.yaml"].map(String::from)
        );
        assert_eq!(
            CoreKind::Xray.check_command_args(config),
            ["run", "-test", "-c", "/data/config/active.yaml"].map(String::from)
        );
        assert_eq!(
            CoreKind::SingBox.run_command_args(config),
            ["run", "-c", "/data/config/active.yaml"].map(String::from)
        );
    }

    #[test]
    fn mihomo_home_falls_back_for_relative_config() {
        let args = mihomo_home_args("active.yaml");
        assert_eq!(args, vec!["-d".to_string(), "mihomo".to_string()]);
    }

    #[test]
    fn protocol_support_matrix() {
        assert!(CoreKind::Xray.supports(Protocol::Vless));
        assert!(CoreKind::Xray.supports(Protocol::Vmess));
        assert!(CoreKind::Xray.supports(Protocol::WireGuard));
        assert!(CoreKind::Xray.supports(Protocol::Hysteria2));
        assert!(!CoreKind::Xray.supports(Protocol::Tuic));
        assert!(CoreKind::SingBox.supports(Protocol::Hysteria2));
        // mihomo: canonical Clash Meta — near-full coverage.
        assert!(CoreKind::Mihomo.supports(Protocol::Hysteria2));
        assert!(CoreKind::Mihomo.supports(Protocol::AnyTls));
        assert!(CoreKind::Mihomo.supports(Protocol::Snell));
        assert!(CoreKind::Mihomo.supports(Protocol::Tuic));
        assert!(CoreKind::Mihomo.supports(Protocol::WireGuard));
        assert!(CoreKind::Mihomo.supports(Protocol::Hysteria));
        assert!(!CoreKind::Mihomo.supports(Protocol::Naive));
        assert!(!CoreKind::Mihomo.supports(Protocol::Tor));
    }

    #[test]
    fn mihomo_node_level_support_matrix() {
        use crate::domain::{ProtocolConfig, ProxyNode, TlsConfig, Transport};

        fn node(
            protocol: Protocol,
            tls: Option<TlsConfig>,
            transport: Option<Transport>,
        ) -> ProxyNode {
            ProxyNode {
                id: String::new(),
                name: "n".into(),
                protocol,
                server: "example.com".into(),
                port: 443,
                tls,
                transport,
                udp: None,
                config: match protocol {
                    Protocol::Vmess => ProtocolConfig::Vmess {
                        uuid: "u".into(),
                        alter_id: 0,
                        security: "auto".into(),
                    },
                    _ => ProtocolConfig::Shadowsocks {
                        method: "aes-256-gcm".into(),
                        password: "pw".into(),
                        plugin: None,
                        plugin_opts: None,
                        shadow_tls: None,
                    },
                },
                source: None,
                latency_ms: None,
                latency_at: None,
            }
            .with_computed_id()
        }

        let reality = Some(TlsConfig {
            enabled: true,
            server_name: Some("www.microsoft.com".into()),
            insecure: None,
            alpn: None,
            utls_fingerprint: Some("chrome".into()),
            reality_public_key: Some("pk".into()),
            reality_short_id: Some("abcd".into()),
        });
        let plain_tls = Some(TlsConfig {
            enabled: true,
            ..Default::default()
        });

        // mihomo (canonical Clash Meta, uTLS): REALITY and Vision both fine.
        assert!(CoreKind::Mihomo.supports_node(&node(Protocol::Vless, reality.clone(), None)));
        let mut vision = node(Protocol::Vless, plain_tls.clone(), None);
        vision.config = ProtocolConfig::Vless {
            uuid: "u".into(),
            flow: Some("xtls-rprx-vision".into()),
            packet_encoding: "xudp".into(),
        };
        assert!(CoreKind::Mihomo.supports_node(&vision));
        // vmess over any transport (grpc here) is fine.
        assert!(CoreKind::Mihomo.supports_node(&node(
            Protocol::Vmess,
            plain_tls.clone(),
            Some(Transport::Grpc { service_name: None })
        )));
        // ss + shadow-tls detour shape not representable (yet); plain ss fine.
        let mut ss_stls = node(Protocol::Shadowsocks, None, None);
        ss_stls.config = ProtocolConfig::Shadowsocks {
            method: "aes-256-gcm".into(),
            password: "pw".into(),
            plugin: None,
            plugin_opts: None,
            shadow_tls: Some(crate::domain::ShadowTlsOpts {
                host: "h".into(),
                password: "p".into(),
                version: 3,
                fingerprint: None,
            }),
        };
        assert!(!CoreKind::Mihomo.supports_node(&ss_stls));
        assert!(CoreKind::Mihomo.supports_node(&node(Protocol::Shadowsocks, None, None)));
        // Protocol-level: naive/tor/shadowtls standalone excluded.
        assert!(!CoreKind::Mihomo.supports(Protocol::Naive));
        assert!(!CoreKind::Mihomo.supports(Protocol::Tor));
        // Xray: REALITY over ws rejected, over tcp fine (unchanged).
        let reality_ws = node(
            Protocol::Vless,
            reality.clone(),
            Some(Transport::Ws {
                path: None,
                headers: None,
                max_early_data: None,
            }),
        );
        assert!(!CoreKind::Xray.supports_node(&reality_ws));
        assert!(CoreKind::Xray.supports_node(&node(
            Protocol::Vless,
            reality,
            Some(Transport::Tcp)
        )));
        // Xray: hysteria2 with obfs rejected (no obfs field in Xray's
        // hysteria transport); plain hysteria2 fine.
        let mut hy2_obfs = ProxyNode {
            id: String::new(),
            name: "hy2".into(),
            protocol: Protocol::Hysteria2,
            server: "example.com".into(),
            port: 443,
            tls: Some(TlsConfig {
                enabled: true,
                server_name: None,
                insecure: None,
                alpn: None,
                utls_fingerprint: None,
                reality_public_key: None,
                reality_short_id: None,
            }),
            transport: None,
            udp: Some(true),
            config: ProtocolConfig::Hysteria2 {
                password: "pw".into(),
                up_mbps: None,
                down_mbps: None,
                obfs: Some("salamander".into()),
                obfs_password: Some("obfspw".into()),
            },
            source: None,
            latency_ms: None,
            latency_at: None,
        };
        assert!(!CoreKind::Xray.supports_node(&hy2_obfs));
        if let ProtocolConfig::Hysteria2 { obfs, .. } = &mut hy2_obfs.config {
            *obfs = None;
        }
        assert!(CoreKind::Xray.supports_node(&hy2_obfs));
        // sing-box accepts everything.
        assert!(CoreKind::SingBox.supports_node(&ss_stls));
        assert!(CoreKind::SingBox.supports_node(&vision));
    }

    #[test]
    fn settings_roundtrip() {
        assert_eq!(CoreKind::parse("xray"), CoreKind::Xray);
        assert_eq!(CoreKind::parse("mihomo"), CoreKind::Mihomo);
        assert_eq!(CoreKind::parse("singbox"), CoreKind::SingBox);
        assert_eq!(CoreKind::parse("garbage"), CoreKind::SingBox);
        assert_eq!(CoreKind::Xray.as_str(), "xray");
        assert_eq!(CoreKind::Mihomo.as_str(), "mihomo");
    }
}
