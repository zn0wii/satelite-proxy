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
    /// Bundled `aether` companion (Cloudflare WARP / MASQUE). NEVER a main
    /// core — [`CoreKind::parse`] deliberately does not map `"aether"`, and
    /// every main-core surface (settings.core_type, downloads, updates)
    /// excludes it. It exists solely as a sidecar kind in `SidecarPort` so
    /// the delegation machinery (plan → start → poll → logs) works
    /// unchanged. `Protocol::Warp` nodes egress through its loopback socks
    /// listener on [`AETHER_SIDECAR_PORT`].
    Aether,
}

/// Fixed loopback port the aether sidecar binds its SOCKS5 listener on.
/// Deliberately NOT part of the sidecar port range: aether is a single
/// shared tunnel (one process serves every Warp node), so it needs exactly
/// one well-known port the generators can also fall back to when no plan
/// is in play.
pub const AETHER_SIDECAR_PORT: u16 = 18191;

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
            Self::Aether => {
                if cfg!(windows) {
                    "aether.exe"
                } else {
                    "aether"
                }
            }
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::SingBox => "sing-box",
            Self::Xray => "Xray",
            Self::Mihomo => "mihomo",
            Self::Aether => "Aether",
        }
    }

    /// Stable token used by settings storage and the frontend.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SingBox => "singbox",
            Self::Xray => "xray",
            Self::Mihomo => "mihomo",
            Self::Aether => "aether",
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
            // Upstream of the bundled core. The app never auto-downloads
            // aether (it is staged at package time by
            // `scripts/fetch-bundled-aether-*` from these releases); the
            // repo() getter only documents provenance.
            Self::Aether => "CluvexStudio/Aether",
        }
    }

    /// Pinned version used only when the GitHub API is unreachable.
    pub fn fallback_version(self) -> &'static str {
        match self {
            Self::SingBox => "v1.13.18",
            Self::Xray => "v26.3.27",
            Self::Mihomo => "v1.19.30",
            Self::Aether => "1.9.0",
        }
    }

    /// Release asset name for a platform. The naming schemes differ:
    /// sing-box `sing-box-{ver}-darwin-arm64.tar.gz`, Xray
    /// `Xray-macos-arm64-v8a.zip` (no version, `64` not `amd64`), mihomo
    /// `mihomo-{plat}-v{ver}.zip|.gz` (same plat suffixes as sing-box;
    /// Windows zips carry a platform-suffixed inner exe, darwin/linux ship
    /// a bare gzipped binary). On amd64, mihomo assets get the
    /// `-compatible` infix: upstream's plain amd64 builds are GOAMD64=v3
    /// (AVX2/BMI2) and fatal at startup on Rosetta 2 and pre-Haswell
    /// Intel CPUs — see AGENTS.md §9.17⑦. `compatible` == GOAMD64=v1,
    /// which costs nothing here (the crypto hot path is AES-NI, v1-level).
    pub fn asset_name(self, version: &str, platform_suffix: &str, is_windows: bool) -> String {
        let ver_num = version.trim_start_matches('v');
        match self {
            Self::SingBox => {
                let ext = if is_windows { "zip" } else { "tar.gz" };
                format!("sing-box-{ver_num}-{platform_suffix}.{ext}")
            }
            Self::Xray => format!("Xray-{platform_suffix}.zip"),
            // Upstream release asset naming (`fetch-bundled-aether-*`
            // scripts consume this shape directly).
            Self::Aether => format!("aether-{ver_num}-{platform_suffix}.tar.gz"),
            Self::Mihomo => {
                let ext = if is_windows { "zip" } else { "gz" };
                if platform_suffix.ends_with("amd64") {
                    format!("mihomo-{platform_suffix}-compatible-v{ver_num}.{ext}")
                } else {
                    format!("mihomo-{platform_suffix}-v{ver_num}.{ext}")
                }
            }
        }
    }

    /// CLI arguments that print the version.
    pub fn version_args(self) -> &'static [&'static str] {
        match self {
            Self::SingBox => &["version"],
            Self::Xray => &["-version"],
            Self::Mihomo => &["-v"],
            // No version flag in the CLI; version display is not surfaced
            // for the aether sidecar.
            Self::Aether => &["--help"],
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
                Self::Aether => return None,
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
            // No config-validate mode; `CoreManager::check_config` skips the
            // kind entirely — aether validates itself via its data-plane
            // check after start.
            Self::Aether => Vec::new(),
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
            // `config` is the identity file (`<data>/aether/aether.toml`,
            // created on first run). Protocol and log level arrive via
            // `spawn_env`; the SOCKS port is the fixed sidecar constant so
            // generated configs and the readiness wait agree without any
            // port-plumbing through this signature.
            Self::Aether => vec![
                "--config".into(),
                config.clone(),
                "--bind".into(),
                format!("127.0.0.1:{AETHER_SIDECAR_PORT}"),
                "--scan".into(),
                "turbo".into(),
                "--quick-reconnect".into(),
            ],
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
            Some("aether") => Self::Aether,
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
            // Headless mode: without AETHER_PROTOCOL the CLI drops into an
            // interactive stdin protocol menu and blocks forever. MASQUE is
            // the v1 pinned transport.
            Self::Aether => vec![
                ("AETHER_PROTOCOL".into(), "masque".into()),
                ("AETHER_LOG_LEVEL".into(), "info".into()),
                // The CLI also prompts (stdin TTY) for scan mode and IP
                // version — pin both; the manager additionally detaches
                // stdin so no prompt can ever block the child.
                ("AETHER_SCAN".into(), "turbo".into()),
                ("AETHER_IP".into(), "4".into()),
            ],
        }
    }

    /// Log file / display prefix.
    pub fn log_prefix(self) -> &'static str {
        match self {
            Self::SingBox => "sing-box",
            Self::Xray => "xray",
            Self::Mihomo => "mihomo",
            Self::Aether => "aether",
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
            Self::Aether => "aether-version.txt",
        }
    }

    /// Whether this core can serve the given outbound protocol. Xray and
    /// mihomo each delegate to their `Protocol::*_supported` counterpart —
    /// the single source of truth lives on the protocol enum.
    pub fn supports(self, protocol: Protocol) -> bool {
        match self {
            // sing-box serves every modeled protocol (its own rejects happen
            // at generation time, §9.21) — except raw-passthrough Unknown
            // nodes, which have no generator path at all.
            Self::SingBox => !matches!(protocol, Protocol::Unknown),
            Self::Xray => protocol.xray_supported(),
            Self::Mihomo => protocol.mihomo_supported(),
            // Never a main core; nothing is "served by aether" directly —
            // Warp nodes are served THROUGH it via loopback socks outbounds.
            Self::Aether => false,
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
        // Single source of truth lives in `node_unsupported_reason` — the
        // bool is just its negation, so the support panel's reasons can
        // never drift from the actual filter.
        self.node_unsupported_reason(node).is_none()
    }

    /// Why this core cannot serve the node (`None` = supported). Mirrors
    /// every rule of the old `supports_node` and feeds the core-support
    /// panel's report; static strings keep listing hot paths allocation-free.
    pub fn node_unsupported_reason(self, node: &crate::domain::ProxyNode) -> Option<&'static str> {
        // A raw Clash entry is authoritative for the mihomo kernel: mihomo
        // parses its own dialect natively, so the (lossy, sing-box-shaped)
        // model projection must not gate it — generation embeds the entry
        // verbatim. Kernel-impossible shapes stay excluded (Naive/Tor/
        // ShadowTls have no mihomo outbound). Every other core keeps the
        // model-based rules below, unchanged.
        if self == Self::Mihomo && node.raw.is_some() {
            return match node.protocol {
                crate::domain::Protocol::Naive => Some("mihomo 无 naive 类型出站（内核不支持）"),
                crate::domain::Protocol::Tor => Some("mihomo 无 tor 类型出站（内核不支持）"),
                crate::domain::Protocol::ShadowTls => {
                    Some("mihomo 无独立 shadow-tls 类型出站（仅可作 ss 插件，需原文透传）")
                }
                _ => None,
            };
        }
        if !self.supports(node.protocol) {
            return Some(match self {
                Self::SingBox => "未建模类型：仅 mihomo 内核支持原文透传",
                Self::Xray => "Xray 不支持该协议",
                Self::Mihomo => "mihomo 不支持该协议",
                Self::Aether => "aether 仅作为 WARP 副进程使用，不直接服务节点",
            });
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
                return Some("Xray 的 REALITY 仅支持 tcp/grpc/xhttp 传输");
            }
            // Xray's hysteria transport has no obfs field.
            if let crate::domain::ProtocolConfig::Hysteria2 { obfs, .. } = &node.config {
                if obfs.as_deref().is_some_and(|o| !o.is_empty()) {
                    return Some("Xray 的 hysteria2 传输无 obfs 字段");
                }
            }
            // Xray v26 removed the h2/http transport at config load — such
            // nodes can only be served by sing-box/mihomo (the generator
            // rejects them with the same rule; the list filter keeps them
            // hidden under Xray like any other unsupported shape).
            if matches!(node.transport, Some(crate::domain::Transport::Http { .. })) {
                return Some("Xray v26 已在配置层移除 h2/http 传输");
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
            return Some(if self == Self::Mihomo {
                // Model path only — raw ss+shadow-tls entries passed above.
                "ss+shadow-tls 组合需原文透传（模型重建暂不支持）"
            } else {
                "Xray 不支持 ss+shadow-tls 组合"
            });
        }
        // Xray dropped every non-AEAD shadowsocks stream cipher (aes-*-cfb,
        // rc4-md5, ...) at config load — only AEAD and SS2022 methods remain.
        // mihomo still serves the legacy ciphers, so fall back there instead
        // of generating a config Xray refuses to start.
        if self == Self::Xray {
            if let crate::domain::ProtocolConfig::Shadowsocks { method, .. } = &node.config {
                let method = method.to_ascii_lowercase();
                let aead_or_2022 = method.starts_with("2022-blake3-")
                    || matches!(
                        method.as_str(),
                        "aes-128-gcm"
                            | "aes-256-gcm"
                            | "chacha20-poly1305"
                            | "chacha20-ietf-poly1305"
                            | "aead_aes_128_gcm"
                            | "aead_aes_256_gcm"
                            | "aead_chacha20_poly1305"
                            | "none"
                            | "plain"
                    );
                if !aead_or_2022 {
                    return Some("Xray 仅支持 AEAD/SS2022 系 shadowsocks 加密");
                }
            }
        }
        if matches!(self, Self::Mihomo)
            && matches!(node.transport, Some(crate::domain::Transport::Xhttp { .. }))
        {
            // Model path only — raw xhttp entries passed the raw branch
            // above (mihomo v1.19.30 accepts `network: xhttp` verbatim).
            return Some("mihomo 的 xhttp 传输需原文透传（模型重建暂不支持）");
        }
        None
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
    use crate::domain::{Protocol, ProtocolConfig, ProxyNode};

    fn raw_node(protocol: Protocol) -> ProxyNode {
        ProxyNode {
            id: String::new(),
            name: "n".into(),
            protocol,
            server: "a.example.com".into(),
            port: 443,
            tls: None,
            transport: None,
            udp: None,
            config: ProtocolConfig::Unknown,
            source: None,
            raw: Some(
                "name: n
type: ss
"
                .into(),
            ),
            latency_ms: None,
            latency_at: None,
        }
    }

    #[test]
    fn raw_entries_pass_only_mihomo_except_kernel_impossible() {
        let unknown = raw_node(Protocol::Unknown);
        assert!(CoreKind::Mihomo.supports_node(&unknown));
        assert!(!CoreKind::SingBox.supports_node(&unknown));
        assert!(!CoreKind::Xray.supports_node(&unknown));

        // mihomo has no outbound for these types — raw must not resurrect them.
        for protocol in [Protocol::Naive, Protocol::Tor, Protocol::ShadowTls] {
            assert!(!CoreKind::Mihomo.supports_node(&raw_node(protocol)));
        }

        // ss+shadow-tls in raw form is mihomo-native (plugin shape kept
        // verbatim); the model-based exclusion only applies without raw.
        let mut ss_st = raw_node(Protocol::Shadowsocks);
        ss_st.config = ProtocolConfig::Shadowsocks {
            method: "aes-256-gcm".into(),
            password: "x".into(),
            plugin: None,
            plugin_opts: None,
            shadow_tls: Some(crate::domain::ShadowTlsOpts {
                host: "h".into(),
                password: "p".into(),
                version: 3,
                fingerprint: None,
            }),
        };
        assert!(CoreKind::Mihomo.supports_node(&ss_st));
        ss_st.raw = None;
        assert!(!CoreKind::Mihomo.supports_node(&ss_st));
    }

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
        // mihomo amd64 uses the -compatible (GOAMD64=v1) variant: the plain
        // amd64 asset is v3-compiled and fails the `-v` self-check on
        // Rosetta 2 / pre-Haswell Intel. arm64 has no microarch variants.
        assert_eq!(
            CoreKind::Mihomo.asset_name("1.19.30", "windows-amd64", true),
            "mihomo-windows-amd64-compatible-v1.19.30.zip"
        );
        assert_eq!(
            CoreKind::Mihomo.asset_name("1.19.30", "darwin-amd64", false),
            "mihomo-darwin-amd64-compatible-v1.19.30.gz"
        );
        assert_eq!(
            CoreKind::Mihomo.asset_name("1.19.30", "linux-amd64", false),
            "mihomo-linux-amd64-compatible-v1.19.30.gz"
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
        assert!(!CoreKind::Xray.supports(Protocol::Masque));
        assert!(CoreKind::SingBox.supports(Protocol::Hysteria2));
        // SingBox "supports" everything at listing level by design — masque
        // nodes stay visible under the sing-box main core so users can pin
        // them to a sidecar; generation filters what it can't emit.
        assert!(CoreKind::SingBox.supports(Protocol::Masque));
        // mihomo: canonical Clash Meta — near-full coverage.
        assert!(CoreKind::Mihomo.supports(Protocol::Hysteria2));
        assert!(CoreKind::Mihomo.supports(Protocol::AnyTls));
        assert!(CoreKind::Mihomo.supports(Protocol::Snell));
        assert!(CoreKind::Mihomo.supports(Protocol::Masque));
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
                raw: None,
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
            raw: None,
            latency_ms: None,
            latency_at: None,
        };
        assert!(!CoreKind::Xray.supports_node(&hy2_obfs));
        if let ProtocolConfig::Hysteria2 { obfs, .. } = &mut hy2_obfs.config {
            *obfs = None;
        }
        assert!(CoreKind::Xray.supports_node(&hy2_obfs));
        // Xray removed every non-AEAD shadowsocks stream cipher at config
        // load (github.com/XTLS/Xray-core/issues/1890): a node stuck on
        // aes-256-cfb must be hidden from Xray's node list rather than
        // reaching a generated config that fails to start. mihomo still
        // serves it (Clash Meta kept the legacy ciphers), so it must stay
        // available there.
        let mut ss_legacy = node(Protocol::Shadowsocks, None, None);
        ss_legacy.config = ProtocolConfig::Shadowsocks {
            method: "aes-256-cfb".into(),
            password: "pw".into(),
            plugin: None,
            plugin_opts: None,
            shadow_tls: None,
        };
        assert!(!CoreKind::Xray.supports_node(&ss_legacy));
        assert!(CoreKind::Mihomo.supports_node(&ss_legacy));
        assert!(CoreKind::SingBox.supports_node(&ss_legacy));
        // AEAD and SS2022 methods remain fine under Xray.
        let mut ss_2022 = node(Protocol::Shadowsocks, None, None);
        ss_2022.config = ProtocolConfig::Shadowsocks {
            method: "2022-blake3-aes-256-gcm".into(),
            password: "pw".into(),
            plugin: None,
            plugin_opts: None,
            shadow_tls: None,
        };
        assert!(CoreKind::Xray.supports_node(&ss_2022));
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
