//! Linux privileged execution — setuid root via polkit (`pkexec`).
//!
//! Same model as macOS (`macos_auth`): the core binary is chowned to root
//! and given the setuid bit once, after which TUN starts need no further
//! authorization. Linux deliberately does **not** use the finer-grained
//! `setcap cap_net_admin` alternative:
//!
//! - Caps don't change the uid, but polkit authorizes by uid. sing-tun
//!   (used by both sing-box and mihomo) spawns `resolvectl` on
//!   systemd-resolved systems to configure DNS hijacking — each call a
//!   separate `auth_admin` password prompt on start *and* stop. That DNS
//!   hand-off is architecturally required (the 127.0.0.53 stub resolver is
//!   loopback, which a TUN can never capture), so it can't be configured
//!   away. Root, unlike caps, is implicitly authorized by polkit — zero
//!   prompts, no polkit rules to install.
//! - File capabilities proved fragile across real desktop systems in
//!   practice (nosuid mounts strip them; xattr/SELinux interactions can
//!   silently drop them). setuid is uniform across distros and across all
//!   three cores — Xray's TUN never touches resolvectl in the first place.
//!
//! The binary lives in the user's own data directory, so core upgrades can
//! still replace it freely (directory write permission is what matters) —
//! the replacement carries no setuid bit and the next TUN start re-prompts,
//! exactly like macOS.

use crate::error::{AppError, AppResult};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

/// True when `core` is owned by root and carries the setuid bit — cheap
/// metadata read, safe to call on hot paths.
pub fn core_has_setuid(core: &Path) -> bool {
    let Ok(meta) = core.metadata() else {
        return false;
    };
    meta.is_file() && meta.uid() == 0 && meta.mode() & 0o4000 != 0
}

/// Ensure the core binary is root-owned setuid so TUN starts without
/// further authorization. Runs a single `pkexec` round (GUI password
/// dialog via polkit). Errors surface verbatim so callers can append the
/// platform guidance (`map_tun_permission_hint`).
pub fn ensure_core_setuid(core: &Path) -> AppResult<()> {
    if !core.is_file() {
        return Err(AppError::Core(format!(
            "core binary not found for setuid: {}",
            core.display()
        )));
    }
    if core_has_setuid(core) {
        return Ok(());
    }

    let core_name = core
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "core".into());
    crate::app_log::info(
        "auth",
        format!(
            "authorizing setuid on {core_name} (pkexec): {}",
            core.display()
        ),
    );

    // Stage the elevate script in the user's temp dir — writing it to a file
    // sidesteps nested quoting through `pkexec sh -c`. The script itself
    // needs no special mount properties: pkexec runs it as root regardless.
    let uniq = format!("satelite-elevate-{}", std::process::id());
    let script_tmp = std::env::temp_dir().join(format!("{uniq}.sh"));
    // `setcap -r` first: a leftover file-capability xattr CONSTRAINS a
    // setuid-root process to exactly those caps (kernel rule), stripping
    // cap_dac_override — the core then fails to read the config tree under
    // the user's 0700 home. Not fatal when absent, hence `|| true`.
    let script = format!(
        "#!/bin/sh\nset -e\nsetcap -r '{}' 2>/dev/null || true\nchown root:root '{}'\nchmod u+s '{}'\n",
        shell_escape(&core.to_string_lossy()),
        shell_escape(&core.to_string_lossy()),
        shell_escape(&core.to_string_lossy()),
    );
    let write_stage = (|| -> AppResult<()> {
        std::fs::write(&script_tmp, &script)
            .map_err(|e| AppError::Core(format!("无法写入临时提权脚本: {e}")))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| AppError::Core(format!("无法设置提权脚本权限: {e}")))?;
        Ok(())
    })();
    if let Err(e) = write_stage {
        let _ = std::fs::remove_file(&script_tmp);
        return Err(e);
    }

    let out = Command::new("pkexec")
        .arg(&script_tmp)
        .output()
        .map_err(|e| {
            let _ = std::fs::remove_file(&script_tmp);
            AppError::Core(format!(
                "无法执行 pkexec（polkit 授权工具缺失？）: {e}。\n\
                 可手动授权：sudo chown root:root '{0}' && sudo chmod u+s '{0}'",
                core.display()
            ))
        })?;
    let _ = std::fs::remove_file(&script_tmp);

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        // pkexec exits 126 when the dialog is dismissed/cancelled.
        if out.status.code() == Some(126) {
            return Err(AppError::Core(
                "已取消管理员授权。TUN 模式需要一次性授权内核（setuid）。".into(),
            ));
        }
        return Err(AppError::Core(if stderr.is_empty() {
            format!("为 {core_name} 设置 setuid 失败（无错误详情）")
        } else {
            format!("为 {core_name} 设置 setuid 失败:\n{stderr}")
        }));
    }

    if !core_has_setuid(core) {
        return Err(AppError::Core(format!(
            "管理员提权已执行，但 {core_name} 仍未是 root 所有的 setuid。\n\
             可手动检查：ls -l '{}'",
            core.display()
        )));
    }

    crate::app_log::info("auth", format!("{core_name} setuid ok"));
    Ok(())
}

/// Minimal single-quote escaping for embedding a path in the shell script.
fn shell_escape(s: &str) -> String {
    s.replace('\'', r#"'\''"#)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_file_is_not_setuid() {
        let f = std::env::temp_dir().join("satelite-linux-auth-test-plain");
        std::fs::write(&f, b"x").unwrap();
        assert!(!core_has_setuid(&f));
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn missing_file_is_not_setuid() {
        assert!(!core_has_setuid(Path::new(
            "/nonexistent/satelite-test-binary"
        )));
    }

    #[test]
    fn shell_escape_quotes() {
        assert_eq!(shell_escape("/a/b c"), "/a/b c");
        assert_eq!(shell_escape("/a/b'c"), r"/a/b'\''c");
    }
}
