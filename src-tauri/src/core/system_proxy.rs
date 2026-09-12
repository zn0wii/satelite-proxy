//! Best-effort proxy discovery for the download path, used only while the
//! core isn't running yet (see `commands::core::current_download_proxy`).
//! Once the core is up, its own mixed inbound is always preferred — this
//! module exists purely for the bootstrap "first install" case where no
//! proxy loop exists yet: standard env vars first, then the OS's own proxy
//! setting.
//!
//! Same shape as `macos_net`: macOS shells out to `scutil`; Windows reads the
//! registry through the `windows` crate API — never spawn `reg.exe` from this
//! GUI-subsystem process, console tools flash a visible window on every
//! settings-page update check. Both feed a pure parser that stays
//! unit-testable without touching the network or the system.

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
    use windows::core::{w, PCWSTR};
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };

    const SUBKEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings");

    let mut enabled: u32 = 0;
    let mut size = core::mem::size_of::<u32>() as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            SUBKEY,
            w!("ProxyEnable"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut enabled as *mut u32).cast()),
            Some(&mut size),
        )
    };
    if rc.is_err() || enabled != 1 {
        return None;
    }

    // Size probe first (pvdata=None). The probe rc is deliberately ignored:
    // depending on the Windows version it reports success or ERROR_MORE_DATA,
    // and either way a nonzero size means the value exists.
    let mut size = 0u32;
    let _ = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            SUBKEY,
            w!("ProxyServer"),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if size < 2 {
        return None; // missing, or nothing but the trailing NUL
    }
    let mut buffer = vec![0u16; (size as usize + 1) / 2];
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            SUBKEY,
            w!("ProxyServer"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if rc.is_err() {
        return None;
    }
    let len = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    parse_reg_proxy_server(&String::from_utf16_lossy(&buffer[..len]))
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
    build("HTTPSEnable", "HTTPSProxy", "HTTPSPort")
        .or_else(|| build("HTTPEnable", "HTTPProxy", "HTTPPort"))
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

/// Parse the raw `ProxyServer` registry value. It is either a single
/// `host:port` (used for all schemes) or a `scheme=host:port;...` list —
/// HTTPS is preferred, then plain HTTP.
///
/// Value shape:
/// ```text
/// 127.0.0.1:7890
/// ```
/// or
/// ```text
/// http=127.0.0.1:7890;https=127.0.0.1:7890
/// ```
#[cfg(target_os = "windows")]
fn parse_reg_proxy_server(value: &str) -> Option<String> {
    let value = value.trim();
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
            let sample =
                "<dictionary> {\n  HTTPEnable : 0\n  HTTPSEnable : 0\n  SOCKSEnable : 0\n}\n";
            assert_eq!(parse_scutil_proxy(sample), None);
        }
    }

    #[cfg(target_os = "windows")]
    mod windows {
        use super::*;

        #[test]
        fn parses_single_host_port_proxy_server() {
            assert_eq!(
                parse_reg_proxy_server("127.0.0.1:7890"),
                Some("http://127.0.0.1:7890".to_string())
            );
        }

        #[test]
        fn prefers_https_entry_in_scheme_list() {
            assert_eq!(
                parse_reg_proxy_server("http=1.2.3.4:8080;https=1.2.3.4:8443"),
                Some("http://1.2.3.4:8443".to_string())
            );
        }

        #[test]
        fn falls_back_to_http_entry_when_no_https() {
            assert_eq!(
                parse_reg_proxy_server("http=1.2.3.4:8080;ftp=1.2.3.4:21"),
                Some("http://1.2.3.4:8080".to_string())
            );
        }

        #[test]
        fn returns_none_for_empty_value() {
            assert_eq!(parse_reg_proxy_server("   "), None);
        }

        /// Read-only smoke test of the live registry path — the key exists on
        /// every Windows install, so this must return without panicking
        /// whether or not a proxy is configured. Guards the unsafe
        /// `RegGetValueW` plumbing end to end.
        #[test]
        fn read_system_proxy_runs_against_live_registry() {
            let _ = read_system_proxy();
        }
    }
}
