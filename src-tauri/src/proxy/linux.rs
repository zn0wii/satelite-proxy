//! Linux system proxy via desktop settings.
//!
//! GNOME-family desktops (GNOME/Cinnamon/Budgie/Pantheon/XFCE-with-gsettings)
//! are driven through `org.gnome.system.proxy` gsettings keys; KDE Plasma is
//! driven through `~/.config/kioslaverc` `[Proxy Settings]` via
//! kwriteconfig/kreadconfig. The desktop is picked from
//! `XDG_CURRENT_DESKTOP`; anything else is rejected with a clear error rather
//! than silently writing config nothing reads.

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

const GNOME_SCHEMA: &str = "org.gnome.system.proxy";
const GNOME_HTTP_SCHEMA: &str = "org.gnome.system.proxy.http";

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
/// mode/use-same-proxy/ignore-hosts while `org.gnome.system.proxy.http` holds
/// the single http endpoint (enabled/host/port). With `use-same-proxy=true`
/// every protocol (https/socks/ftp) rides on that one endpoint — exactly what
/// a mixed-port core needs. Per-protocol keys we never touch survive a
/// disable() that restores use-same-proxy=false.
#[derive(Default)]
struct GnomeState {
    mode: String,
    use_same: String,
    ignore: String,
    http_enabled: String,
    http_host: String,
    http_port: String,
}

fn gnome_read() -> AppResult<GnomeState> {
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
    })
}

fn gnome_write(s: &GnomeState) -> AppResult<()> {
    gsettings_set(GNOME_SCHEMA, "mode", &format!("'{}'", s.mode))?;
    gsettings_set(GNOME_SCHEMA, "use-same-proxy", &s.use_same)?;
    gsettings_set(GNOME_SCHEMA, "ignore-hosts", &s.ignore)?;
    gsettings_set(GNOME_HTTP_SCHEMA, "enabled", &s.http_enabled)?;
    gsettings_set(GNOME_HTTP_SCHEMA, "host", &format!("'{}'", s.http_host))?;
    gsettings_set(GNOME_HTTP_SCHEMA, "port", &format!("uint32 {}", s.http_port))
}

fn gnome_owned(host: &str, port: u16) -> AppResult<bool> {
    let s = gnome_read()?;
    Ok(s.mode == "manual"
        && s.use_same == "true"
        && s.http_enabled == "true"
        && s.http_host == host
        && s.http_port == port.to_string())
}

fn detail_is_gnome(detail: &str) -> bool {
    detail.starts_with("gnome|")
}

/// `gnome|mode|use_same|ignore|http_enabled|http_host|http_port`
fn gnome_detail(s: &GnomeState) -> String {
    format!(
        "gnome|{}|{}|{}|{}|{}|{}",
        s.mode, s.use_same, s.ignore, s.http_enabled, s.http_host, s.http_port
    )
}

fn gnome_from_detail(detail: &str) -> Option<GnomeState> {
    let parts: Vec<&str> = detail.split('|').collect();
    if parts.len() != 7 {
        return None;
    }
    Some(GnomeState {
        mode: parts[1].into(),
        use_same: parts[2].into(),
        ignore: parts[3].into(),
        http_enabled: parts[4].into(),
        http_host: parts[5].into(),
        http_port: parts[6].into(),
    })
}

impl SystemProxy for LinuxSystemProxy {
    fn enable(&self, host: &str, port: u16) -> AppResult<SystemProxySnapshot> {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();
        let port_s = port.to_string();
        let endpoint = format!("http://{host}:{port}");

        if desktop.contains("kde") || desktop.contains("plasma") {
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

        if desktop.contains("gnome")
            || desktop.contains("cinnamon")
            || desktop.contains("budgie")
            || desktop.contains("pantheon")
            || desktop.contains("xfce")
            || desktop.contains("mate")
            || desktop.is_empty()
        {
            let prev = gnome_read()?;
            let next = GnomeState {
                mode: "manual".into(),
                use_same: "true".into(),
                http_enabled: "true".into(),
                http_host: host.into(),
                http_port: port_s,
                ignore: "['localhost', '127.0.0.1', '*.local', '<local>']".into(),
            };
            gnome_write(&next)?;
            return Ok(SystemProxySnapshot {
                detail: gnome_detail(&prev),
            });
        }

        Err(AppError::Core(format!(
            "system proxy: unsupported desktop '{desktop}'"
        )))
    }

    fn disable(&self, snapshot: Option<&SystemProxySnapshot>) -> AppResult<()> {
        let Some(snap) = snapshot else {
            // No snapshot: turn the proxy off entirely (best effort).
            let desktop = std::env::var("XDG_CURRENT_DESKTOP")
                .unwrap_or_default()
                .to_lowercase();
            if desktop.contains("kde") || desktop.contains("plasma") {
                kde_write("ProxyType", "0")?;
                kde_reconfigure();
                return Ok(());
            }
            gsettings_set(GNOME_SCHEMA, "mode", "'none'")?;
            return Ok(());
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
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();
        if desktop.contains("kde") || desktop.contains("plasma") {
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
        if gnome_owned(host, port)? {
            return Ok(Some(SystemProxySnapshot {
                detail: gnome_detail(&gnome_read()?),
            }));
        }
        Ok(None)
    }
}
