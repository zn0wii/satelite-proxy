//! Best-effort proxy discovery for the download path, used only while the
//! core isn't running yet (see `commands::core::current_download_proxy`).
//! Once the core is up, its own mixed inbound is always preferred — this
//! module exists purely for the bootstrap "first install" case where no
//! proxy loop exists yet: standard env vars first, then the OS's own proxy
//! setting.
//!
//! Same shape as `macos_net`: shell out to a standard OS tool, parse plain
//! text with a pure function so parsing is unit-testable without touching
//! the network or the system.

/// Standard proxy env vars, most specific first. Checked in both upper and
/// lower case since shells disagree on convention (curl et al. accept both).
const ENV_VARS: &[&str] = &[
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// First non-empty proxy env var, in `ENV_VARS` order. `None` if none are set —
/// GUI apps launched from Finder/Dock/launchd do not inherit a shell's
/// `export`s, so this only fires for terminal-launched or dev builds.
pub fn read_env_proxy() -> Option<String> {
    ENV_VARS.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

/// OS-level proxy setting (macOS: `scutil --proxy`; Windows: registry).
/// `None` on any other platform, or if the OS reports proxying disabled.
#[cfg(target_os = "macos")]
pub fn read_system_proxy() -> Option<String> {
    let out = std::process::Command::new("scutil")
        .arg("--proxy")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_scutil_proxy(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(target_os = "windows")]
pub fn read_system_proxy() -> Option<String> {
    let out = std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyEnable",
        ])
        .output()
        .ok()?;
    let enabled_text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || !reg_dword_is_one(&enabled_text, "ProxyEnable") {
        return None;
    }

    let out = std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyServer",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_reg_proxy_server(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn read_system_proxy() -> Option<String> {
    None
}

/// Parse `scutil --proxy` output. Prefers HTTPS; falls back to the plain
/// HTTP proxy entry if only that is enabled.
///
/// Expected shape:
/// ```text
/// <dictionary> {
///   HTTPEnable : 1
///   HTTPPort : 7890
///   HTTPProxy : 127.0.0.1
///   HTTPSEnable : 1
///   HTTPSPort : 7890
///   HTTPSProxy : 127.0.0.1
/// }
/// ```
#[cfg(target_os = "macos")]
fn parse_scutil_proxy(text: &str) -> Option<String> {
    let fields = scutil_fields(text);
    let build = |enable_key: &str, host_key: &str, port_key: &str| -> Option<String> {
        if fields.get(enable_key).map(String::as_str) != Some("1") {
            return None;
        }
        let host = fields.get(host_key)?;
        let port = fields.get(port_key)?;
        Some(format!("http://{host}:{port}"))
    };
    build("HTTPSEnable", "HTTPSProxy", "HTTPSPort").or_else(|| build("HTTPEnable", "HTTPProxy", "HTTPPort"))
}

#[cfg(target_os = "macos")]
fn scutil_fields(text: &str) -> std::collections::HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

/// Parse `reg query ... /v ProxyEnable` output for a DWORD value of `0x1`.
#[cfg(target_os = "windows")]
fn reg_dword_is_one(text: &str, name: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        line.starts_with(name) && line.trim_end().ends_with("0x1")
    })
}

/// Parse `reg query ... /v ProxyServer` output. `ProxyServer` is either a
/// single `host:port` (used for all schemes) or a `scheme=host:port;...`
/// list — HTTPS is preferred, then plain HTTP.
///
/// Expected shape:
/// ```text
///     ProxyServer    REG_SZ    127.0.0.1:7890
/// ```
/// or
/// ```text
///     ProxyServer    REG_SZ    http=127.0.0.1:7890;https=127.0.0.1:7890
/// ```
#[cfg(target_os = "windows")]
fn parse_reg_proxy_server(text: &str) -> Option<String> {
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with("ProxyServer"))?;
    let value = line.trim().strip_prefix("ProxyServer")?.trim();
    let value = value.strip_prefix("REG_SZ")?.trim();
    if value.is_empty() {
        return None;
    }

    if !value.contains('=') {
        return Some(format!("http://{value}"));
    }
    let mut fallback = None;
    for entry in value.split(';') {
        let Some((scheme, addr)) = entry.split_once('=') else {
            continue;
        };
        if addr.is_empty() {
            continue;
        }
        if scheme.eq_ignore_ascii_case("https") {
            return Some(format!("http://{addr}"));
        }
        if scheme.eq_ignore_ascii_case("http") {
            fallback = Some(format!("http://{addr}"));
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    mod macos {
        use super::*;

        #[test]
        fn parses_https_proxy_when_enabled() {
            let sample = "<dictionary> {\n  HTTPEnable : 0\n  HTTPSEnable : 1\n  HTTPSPort : 7890\n  HTTPSProxy : 127.0.0.1\n}\n";
            assert_eq!(
                parse_scutil_proxy(sample),
                Some("http://127.0.0.1:7890".to_string())
            );
        }

        #[test]
        fn falls_back_to_http_proxy_when_https_disabled() {
            let sample = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 8080\n  HTTPProxy : 10.0.0.1\n  HTTPSEnable : 0\n}\n";
            assert_eq!(
                parse_scutil_proxy(sample),
                Some("http://10.0.0.1:8080".to_string())
            );
        }

        #[test]
        fn returns_none_when_nothing_enabled() {
            let sample = "<dictionary> {\n  HTTPEnable : 0\n  HTTPSEnable : 0\n  SOCKSEnable : 0\n}\n";
            assert_eq!(parse_scutil_proxy(sample), None);
        }
    }

    #[cfg(target_os = "windows")]
    mod windows {
        use super::*;

        #[test]
        fn detects_proxy_enabled_dword() {
            let sample = "\nHKCU\\...\\Internet Settings\n    ProxyEnable    REG_DWORD    0x1\n\n";
            assert!(reg_dword_is_one(sample, "ProxyEnable"));
        }

        #[test]
        fn detects_proxy_disabled_dword() {
            let sample = "\nHKCU\\...\\Internet Settings\n    ProxyEnable    REG_DWORD    0x0\n\n";
            assert!(!reg_dword_is_one(sample, "ProxyEnable"));
        }

        #[test]
        fn parses_single_host_port_proxy_server() {
            let sample = "\n    ProxyServer    REG_SZ    127.0.0.1:7890\n";
            assert_eq!(
                parse_reg_proxy_server(sample),
                Some("http://127.0.0.1:7890".to_string())
            );
        }

        #[test]
        fn prefers_https_entry_in_scheme_list() {
            let sample = "\n    ProxyServer    REG_SZ    http=1.2.3.4:8080;https=1.2.3.4:8443\n";
            assert_eq!(
                parse_reg_proxy_server(sample),
                Some("http://1.2.3.4:8443".to_string())
            );
        }

        #[test]
        fn falls_back_to_http_entry_when_no_https() {
            let sample = "\n    ProxyServer    REG_SZ    http=1.2.3.4:8080;ftp=1.2.3.4:21\n";
            assert_eq!(
                parse_reg_proxy_server(sample),
                Some("http://1.2.3.4:8080".to_string())
            );
        }

        #[test]
        fn returns_none_for_empty_value() {
            let sample = "\n    ProxyServer    REG_SZ    \n";
            assert_eq!(parse_reg_proxy_server(sample), None);
        }
    }
}
