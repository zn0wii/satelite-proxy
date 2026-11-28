//! Linux system proxy via desktop settings.
//!
//! KDE Plasma is driven through `~/.config/kioslaverc` `[Proxy Settings]` via
//! kwriteconfig6/5 + kded reconfigure. Everything else is capability-detected:
//! if the `org.gnome.system.proxy` gsettings schema exists we write it — this
//! covers GNOME, Budgie, Pantheon, Cinnamon and any derivative shipping that
//! schema. Desktops with neither mechanism (XFCE, MATE, LXQt, Sway, i3, …)
//! are rejected with a clear error rather than silently writing config nothing
//! reads.

use super::{SystemProxy, SystemProxySnapshot};
use crate::error::{AppError, AppResult};
use std::process::Command;

pub struct LinuxSystemProxy;

fn run(cmd: &str, args: &[&str]) -> AppResult<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| AppError::Core(format!("{cmd}: {e}")))?;
    if !out.status.success() {
        return Err(AppError::Core(format!(
            "{cmd} {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `gsettings get` prints values quoted (`'manual'`, `uint32 8080`); strip the
/// decoration so callers compare/store plain text.
fn unquote(raw: &str) -> String {
    let t = raw.trim();
    if let Some(inner) = t.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return inner.to_string();
    }
    t.rsplit_once(' ').map(|(_, v)| v).unwrap_or(t).to_string()
}

fn is_kde_desktop() -> bool {
    let d = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();
    d.contains("kde") || d.contains("plasma")
}

/// Capability check, not a name check: the GNOME proxy schema only exists on
/// desktops that actually read it (GNOME, Budgie, Pantheon, Cinnamon…).
///XFCE/MATE ship their own settings daemons and correctly fall through to the
/// unsupported error instead of failing inside gsettings.
fn gnome_proxy_schema_available() -> bool {
    run("gsettings", &["list-schemas"])
        .map(|out| out.lines().any(|l| l.trim() == GNOME_SCHEMA))
        .unwrap_or(false)
}

const GNOME_SCHEMA: &str = "org.gnome.system.proxy";
const GNOME_HTTP_SCHEMA: &str = "org.gnome.system.proxy.http";
const GNOME_HTTPS_SCHEMA: &str = "org.gnome.system.proxy.https";
const GNOME_SOCKS_SCHEMA: &str = "org.gnome.system.proxy.socks";
const GNOME_FTP_SCHEMA: &str = "org.gnome.system.proxy.ftp";

fn gsettings_get(schema: &str, key: &str) -> AppResult<String> {
    run("gsettings", &["get", schema, key]).map(|v| unquote(&v))
}

fn gsettings_set(schema: &str, key: &str, value: &str) -> AppResult<()> {
    run("gsettings", &["set", schema, key, value]).map(|_| ())
}

fn kde_write(key: &str, value: &str) -> AppResult<()> {
    for tool in ["kwriteconfig6", "kwriteconfig5"] {
        if Command::new(tool).arg("--version").output().is_ok() {
            return run(
                tool,
                &[
                    "--file",
                    "kioslaverc",
                    "--group",
                    "Proxy Settings",
                    "--key",
                    key,
                    value,
                ],
            )
            .map(|_| ());
        }
    }
    Err(AppError::Core("kwriteconfig6/kwriteconfig5 not found".into()))
}

fn kde_read(key: &str) -> String {
    for tool in ["kreadconfig6", "kreadconfig5"] {
        if let Ok(out) = Command::new(tool)
            .args([
                "--file",
                "kioslaverc",
                "--group",
                "Proxy Settings",
                "--key",
                key,
            ])
            .output()
        {
            if out.status.success() {
                return String::from_utf8_lossy(&out.stdout).trim().to_string();
            }
        }
    }
    String::new()
}

fn kde_reconfigure() {
    // Best-effort: Plasma re-reads kioslaverc on kded reconfigure; failure is
    // harmless because apps pick the file up on next launch regardless.
    for (dbus, service) in [
        ("qdbus6", "org.kde.kded6"),
        ("qdbus", "org.kde.kded5"),
        ("qdbus-qt6", "org.kde.kded6"),
    ] {
        let _ = Command::new(dbus)
            .args([service, "/kded", &format!("{service}.reconfigure")])
            .status();
    }
}

/// GNOME splits proxy config across schemas: `org.gnome.system.proxy` holds
/// mode/use-same-proxy/ignore-hosts while each protocol has its own schema
/// (`…proxy.http` adds `enabled`, the others only host+port). Chrome reads the
/// per-protocol keys literally — leaving https/socks/ftp empty produces
/// ERR_PROXY_CONNECTION_FAILED even with use-same-proxy=true, so enable()
/// writes every protocol endpoint explicitly. Keys we never touch survive a
/// disable() that restores the snapshot.
#[derive(Default)]
struct GnomeState {
    mode: String,
    use_same: String,
    ignore: String,
    http_enabled: String,
    http_host: String,
    http_port: String,
    https_host: String,
    https_port: String,
    socks_host: String,
    socks_port: String,
    ftp_host: String,
    ftp_port: String,
}

fn gnome_proto_get(schema: &str) -> AppResult<(String, String)> {
    Ok((
        gsettings_get(schema, "host")?,
        gsettings_get(schema, "port")?,
    ))
}

fn gnome_read() -> AppResult<GnomeState> {
    let (https_host, https_port) = gnome_proto_get(GNOME_HTTPS_SCHEMA)?;
    let (socks_host, socks_port) = gnome_proto_get(GNOME_SOCKS_SCHEMA)?;
    let (ftp_host, ftp_port) = gnome_proto_get(GNOME_FTP_SCHEMA)?;
    Ok(GnomeState {
        mode: gsettings_get(GNOME_SCHEMA, "mode")?,
        use_same: gsettings_get(GNOME_SCHEMA, "use-same-proxy")?,
        // ignore-hosts is `as` (string array) — keep the raw GVariant text so
        // it round-trips through the snapshot untouched.
        ignore: run("gsettings", &["get", GNOME_SCHEMA, "ignore-hosts"])
            .map(|v| v.trim().to_string())?,
        http_enabled: gsettings_get(GNOME_HTTP_SCHEMA, "enabled")?,
        http_host: gsettings_get(GNOME_HTTP_SCHEMA, "host")?,
        http_port: gsettings_get(GNOME_HTTP_SCHEMA, "port")?,
        https_host,
        https_port,
        socks_host,
        socks_port,
        ftp_host,
        ftp_port,
    })
}

fn gnome_write(s: &GnomeState) -> AppResult<()> {
    gsettings_set(GNOME_SCHEMA, "mode", &format!("'{}'", s.mode))?;
    gsettings_set(GNOME_SCHEMA, "use-same-proxy", &s.use_same)?;
    gsettings_set(GNOME_SCHEMA, "ignore-hosts", &s.ignore)?;
    gsettings_set(GNOME_HTTP_SCHEMA, "enabled", &s.http_enabled)?;
    gsettings_set(GNOME_HTTP_SCHEMA, "host", &format!("'{}'", s.http_host))?;
    gsettings_set(GNOME_HTTP_SCHEMA, "port", &format!("uint32 {}", s.http_port))?;
    for (schema, host, port) in [
        (GNOME_HTTPS_SCHEMA, &s.https_host, &s.https_port),
        (GNOME_SOCKS_SCHEMA, &s.socks_host, &s.socks_port),
        (GNOME_FTP_SCHEMA, &s.ftp_host, &s.ftp_port),
    ] {
        gsettings_set(schema, "host", &format!("'{host}'"))?;
        gsettings_set(schema, "port", &format!("uint32 {port}"))?;
    }
    Ok(())
}

fn gnome_owned(host: &str, port: u16) -> AppResult<bool> {
    let s = gnome_read()?;
    let p = port.to_string();
    let ours = |h: &str, po: &str| h == host && po == p;
    Ok(s.mode == "manual"
        && s.http_enabled == "true"
        && ours(&s.http_host, &s.http_port)
        && ours(&s.https_host, &s.https_port)
        && ours(&s.socks_host, &s.socks_port)
        && ours(&s.ftp_host, &s.ftp_port))
}

fn detail_is_gnome(detail: &str) -> bool {
    detail.starts_with("gnome|")
}

/// `gnome|mode|use_same|ignore|http_enabled|http_host|http_port|https_host|https_port|socks_host|socks_port|ftp_host|ftp_port`
fn gnome_detail(s: &GnomeState) -> String {
    format!(
        "gnome|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        s.mode,
        s.use_same,
        s.ignore,
        s.http_enabled,
        s.http_host,
        s.http_port,
        s.https_host,
        s.https_port,
        s.socks_host,
        s.socks_port,
        s.ftp_host,
        s.ftp_port
    )
}

fn gnome_from_detail(detail: &str) -> Option<GnomeState> {
    let parts: Vec<&str> = detail.split('|').collect();
    if parts.len() != 13 {
        return None;
    }
    Some(GnomeState {
        mode: parts[1].into(),
        use_same: parts[2].into(),
        ignore: parts[3].into(),
        http_enabled: parts[4].into(),
        http_host: parts[5].into(),
        http_port: parts[6].into(),
        https_host: parts[7].into(),
        https_port: parts[8].into(),
        socks_host: parts[9].into(),
        socks_port: parts[10].into(),
        ftp_host: parts[11].into(),
        ftp_port: parts[12].into(),
    })
}

impl SystemProxy for LinuxSystemProxy {
    fn enable(&self, host: &str, port: u16) -> AppResult<SystemProxySnapshot> {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();
        let port_s = port.to_string();
        let endpoint = format!("http://{host}:{port}");

        if is_kde_desktop() {
            let prev = [
                kde_read("ProxyType"),
                kde_read("httpProxy"),
                kde_read("httpsProxy"),
                kde_read("socksProxy"),
                kde_read("NoProxyFor"),
            ]
            .join("\u{1f}");
            kde_write("ProxyType", "1")?;
            kde_write("httpProxy", &endpoint)?;
            kde_write("httpsProxy", &endpoint)?;
            kde_write("socksProxy", &endpoint)?;
            kde_write("NoProxyFor", "localhost,127.0.0.1")?;
            kde_reconfigure();
            return Ok(SystemProxySnapshot {
                detail: format!("kde|{prev}"),
            });
        }

        if gnome_proxy_schema_available() {
            let prev = gnome_read()?;
            let p = port_s.clone();
            let next = GnomeState {
                mode: "manual".into(),
                use_same: "true".into(),
                http_enabled: "true".into(),
                http_host: host.into(),
                http_port: p.clone(),
                https_host: host.into(),
                https_port: p.clone(),
                socks_host: host.into(),
                socks_port: p.clone(),
                ftp_host: host.into(),
                ftp_port: p,
                ignore: "['localhost', '127.0.0.1', '*.local', '<local>']".into(),
            };
            gnome_write(&next)?;
            return Ok(SystemProxySnapshot {
                detail: gnome_detail(&prev),
            });
        }

        Err(AppError::Core(format!(
            "system proxy: unsupported desktop '{desktop}' \
             (supported: GNOME-schema desktops, KDE Plasma)"
        )))
    }

    fn disable(&self, snapshot: Option<&SystemProxySnapshot>) -> AppResult<()> {
        let Some(snap) = snapshot else {
            // No snapshot: turn the proxy off entirely (best effort).
            if is_kde_desktop() {
                kde_write("ProxyType", "0")?;
                kde_reconfigure();
                return Ok(());
            }
            if gnome_proxy_schema_available() {
                gsettings_set(GNOME_SCHEMA, "mode", "'none'")?;
                return Ok(());
            }
            return Err(AppError::Core(
                "system proxy: unsupported desktop".into(),
            ));
        };
        let detail = &snap.detail;
        if detail_is_gnome(detail) {
            if let Some(s) = gnome_from_detail(detail) {
                gnome_write(&s)?;
                return Ok(());
            }
            gsettings_set(GNOME_SCHEMA, "mode", "'none'")?;
            return Ok(());
        }
        if let Some(rest) = detail.strip_prefix("kde|") {
            let vals: Vec<&str> = rest.split('\u{1f}').collect();
            if vals.len() >= 5 {
                let keys = ["ProxyType", "httpProxy", "httpsProxy", "socksProxy", "NoProxyFor"];
                for (k, v) in keys.iter().zip(vals.iter()) {
                    kde_write(k, v)?;
                }
                kde_reconfigure();
                return Ok(());
            }
            kde_write("ProxyType", "0")?;
            kde_reconfigure();
            return Ok(());
        }
        Err(AppError::Core("system proxy: unknown snapshot".into()))
    }

    fn detect_owned(&self, host: &str, port: u16) -> AppResult<Option<SystemProxySnapshot>> {
        if is_kde_desktop() {
            let p = format!("http://{host}:{port}");
            let owned = kde_read("ProxyType") == "1"
                && kde_read("httpProxy") == p
                && kde_read("httpsProxy") == p
                && kde_read("socksProxy") == p;
            if !owned {
                return Ok(None);
            }
            let prev = [
                "1".to_string(),
                kde_read("httpProxy"),
                kde_read("httpsProxy"),
                kde_read("socksProxy"),
                kde_read("NoProxyFor"),
            ]
            .join("\u{1f}");
            return Ok(Some(SystemProxySnapshot {
                detail: format!("kde|{prev}"),
            }));
        }
        if gnome_proxy_schema_available() && gnome_owned(host, port)? {
            return Ok(Some(SystemProxySnapshot {
                detail: gnome_detail(&gnome_read()?),
            }));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_enable_detect_disable_roundtrip() {
        if !gnome_proxy_schema_available() {
            return; // GNOME-schema path only; KDE covered separately
        }
        let proxy = LinuxSystemProxy;
        let before_mode = gsettings_get(GNOME_SCHEMA, "mode").unwrap();
        let before_same = gsettings_get(GNOME_SCHEMA, "use-same-proxy").unwrap();

        let snap = proxy.enable("127.0.0.1", 17890).expect("enable");
        assert!(gnome_owned("127.0.0.1", 17890).unwrap());
        assert!(proxy
            .detect_owned("127.0.0.1", 17890)
            .unwrap()
            .is_some());
        assert!(proxy
            .detect_owned("127.0.0.1", 19999)
            .unwrap()
            .is_none());
        assert!(proxy
            .detect_owned("10.0.0.1", 17890)
            .unwrap()
            .is_none());

        proxy.disable(Some(&snap)).expect("disable");
        assert_eq!(gsettings_get(GNOME_SCHEMA, "mode").unwrap(), before_mode);
        assert_eq!(
            gsettings_get(GNOME_SCHEMA, "use-same-proxy").unwrap(),
            before_same
        );
    }
}
