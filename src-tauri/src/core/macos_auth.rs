//! macOS privileged execution — prefer **Touch ID via `sudo` + pam_tid**.
//!
//! Background:
//! - `system.privilege.admin` (osascript / AuthorizationExecuteWithPrivileges)
//!   is defined as a password-only admin right — SecurityAgent will **not** offer
//!   fingerprint for that path (see `security authorizationdb read system.privilege.admin`).
//! - On Macs where `pam_tid.so` is enabled in `/etc/pam.d/sudo_local` (or the
//!   global sudo policy), **`sudo` with a real TTY** shows the Touch ID sheet.
//!   We allocate a PTY (via `script`) so a GUI app can use that path. When
//!   pam_tid is NOT configured, `sudo` would sit on that hidden PTY waiting for
//!   a password nobody can type, so we pass `-n` to fail fast into the
//!   password-based fallbacks below instead of stalling with zero UI feedback.
//!
//! Order: sudo+PTY (Touch ID / cached credentials) → AEWP → osascript.

use crate::error::{AppError, AppResult};
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
use std::process::{Command, Stdio};
use std::ptr;
use std::time::Duration;

type AuthorizationRef = *mut c_void;

const ERR_AUTH_SUCCESS: c_int = 0;
const ERR_AUTH_CANCELED: c_int = -60006;
const ERR_AUTH_DENIED: c_int = -60005;
const ERR_AUTH_INTERACTION_NOT_ALLOWED: c_int = -60007;

const FLAG_DEFAULTS: u32 = 0;
const FLAG_INTERACTION_ALLOWED: u32 = 1 << 0;
const FLAG_EXTEND_RIGHTS: u32 = 1 << 1;
const FLAG_PARTIAL_RIGHTS: u32 = 1 << 2;
const FLAG_DESTROY_RIGHTS: u32 = 1 << 3;
const FLAG_PRE_AUTHORIZE: u32 = 1 << 4;

#[repr(C)]
struct AuthorizationItem {
    name: *const c_char,
    value_length: usize,
    value: *mut c_void,
    flags: u32,
}

#[repr(C)]
struct AuthorizationRights {
    count: u32,
    items: *mut AuthorizationItem,
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    fn AuthorizationCreate(
        rights: *const AuthorizationRights,
        environment: *const c_void,
        flags: u32,
        authorization: *mut AuthorizationRef,
    ) -> c_int;

    fn AuthorizationCopyRights(
        authorization: AuthorizationRef,
        rights: *const AuthorizationRights,
        environment: *const c_void,
        flags: u32,
        authorized_rights: *mut *mut AuthorizationRights,
    ) -> c_int;

    fn AuthorizationFree(authorization: AuthorizationRef, flags: u32) -> c_int;
}

type AuthExecFn = unsafe extern "C" fn(
    authorization: AuthorizationRef,
    path_to_tool: *const c_char,
    options: u32,
    arguments: *const *const c_char,
    communications_pipe: *mut *mut libc::FILE,
) -> c_int;

fn auth_exec_fn() -> Option<AuthExecFn> {
    unsafe {
        let name = CString::new("AuthorizationExecuteWithPrivileges").ok()?;
        let sym = libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr());
        if sym.is_null() {
            None
        } else {
            Some(std::mem::transmute(sym))
        }
    }
}

fn auth_status_message(status: c_int) -> String {
    match status {
        ERR_AUTH_CANCELED => "已取消管理员授权".into(),
        ERR_AUTH_DENIED => "管理员授权被拒绝".into(),
        ERR_AUTH_INTERACTION_NOT_ALLOWED => "无法显示授权对话框（无 UI 会话）".into(),
        code => format!("授权失败 (OSStatus {code})"),
    }
}

fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn escape_applescript_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn build_tool_cmdline(tool: &Path, args: &[&str]) -> String {
    let mut cmd = shell_single_quote(&tool.to_string_lossy());
    for a in args {
        cmd.push(' ');
        cmd.push_str(&shell_single_quote(a));
    }
    cmd
}

fn parse_exit_marker(s: &str) -> Option<i32> {
    for line in s.lines().rev() {
        if let Some(rest) = line.trim().strip_prefix("__SATELITE_EXIT__:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn strip_exit_marker(s: &str) -> String {
    s.lines()
        .filter(|l| {
            let t = l.trim();
            !t.starts_with("__SATELITE_EXIT__:")
                // drop `script` noise
                && !t.starts_with("^D")
                && !t.contains("bash-3.")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// True when sudo is configured to try Touch ID first (`pam_tid.so`).
fn pam_tid_enabled() -> bool {
    for path in ["/etc/pam.d/sudo_local", "/etc/pam.d/sudo"] {
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let t = line.trim();
                if t.starts_with('#') {
                    continue;
                }
                if t.contains("pam_tid.so") {
                    return true;
                }
            }
        }
    }
    false
}

/// True when `core` is already setuid-root in the FlClash style:
/// owner `root`, group `admin`, and mode includes setuid (`rws`).
///
/// With this bit set, a normal user-spawned process runs with euid=root
/// (can create utun) while ruid stays the user (parent can still kill it).
pub fn core_has_setuid(core: &Path) -> bool {
    if !core.is_file() {
        return false;
    }
    let out = Command::new("stat")
        .args(["-f", "%Su:%Sg %Sp"])
        .arg(core)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim();
    // e.g. "root:admin -rwsr-xr-x"
    line.starts_with("root:admin") && line.contains("rws")
}

/// Ensure sing-box has setuid root:admin so TUN works without a LaunchDaemon.
/// One-time admin prompt (Touch ID when pam_tid is available); subsequent starts
/// are a normal spawn as long as the binary is not replaced.
pub fn ensure_core_setuid(core: &Path) -> AppResult<()> {
    if !core.is_file() {
        return Err(AppError::Core(format!(
            "sing-box not found for setuid: {}",
            core.display()
        )));
    }
    if core_has_setuid(core) {
        return Ok(());
    }

    // Best-effort clear quarantine so root/setuid exec is not blocked.
    let _ = Command::new("xattr").args(["-cr"]).arg(core).status();

    let path_q = shell_single_quote(&core.to_string_lossy());
    let core_name = core
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "core".into());
    // Only xattr cleanup may fail silently; a failed chown/chmod MUST surface
    // as a non-zero exit (the trailing `|| true` used to swallow both and made
    // the code report "authorization completed" for a no-op).
    let shell = format!(
        "chown root:admin {p} && chmod +sx {p} && {{ xattr -dr com.apple.quarantine {p} 2>/dev/null || true; }}",
        p = path_q
    );

    crate::app_log::info(
        "auth",
        format!(
            "authorizing setuid on {core_name} (Touch ID / password): {}",
            core.display()
        ),
    );

    let (code, output) = run_privileged(Path::new("/bin/sh"), &["-c", &shell])?;
    if code != 0 {
        let raw = output.trim();
        if raw.contains("已取消") || raw.contains("取消") {
            return Err(AppError::Core(
                "已取消管理员授权。TUN 模式需要一次性授权 sing-box（setuid）。".into(),
            ));
        }
        return Err(AppError::Core(if raw.is_empty() {
            "为 sing-box 设置 setuid 失败（无错误详情）".into()
        } else {
            format!("为 sing-box 设置 setuid 失败:\n{raw}")
        }));
    }

    if !core_has_setuid(core) {
        let hint = diagnose_setuid_failure(core);
        return Err(AppError::Core(format!(
            "管理员提权已执行，但 {core_name} 仍非 setuid root:admin。{hint}"
        )));
    }

    crate::app_log::info("auth", "sing-box setuid ok");
    Ok(())
}

/// Best-effort root cause for a chown/chmod that reported success but didn't
/// stick: checks the volume's "ignore ownership" setting and `nosuid` mount
/// option, both of which let chown/chmod return 0 while the bits are dropped.
fn diagnose_setuid_failure(core: &Path) -> String {
    let mount_point = Command::new("df")
        .args(["-P", "--"])
        .arg(core)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|out| {
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.lines()
                .last()
                .and_then(|line| line.split_whitespace().last().map(str::to_string))
        });

    let Some(mount_point) = mount_point else {
        return "请确认路径可写且未被安全软件还原权限。".into();
    };

    // "Ignore ownership on this volume" makes chown report success without
    // persisting root:admin — the classic false-positive for this check.
    let ignores_ownership = Command::new("diskutil")
        .args(["info", &mount_point])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            text.lines()
                .any(|l| l.trim_start().starts_with("Owners:") && l.contains("Disabled"))
        })
        .unwrap_or(false);
    if ignores_ownership {
        return format!(
            "所在磁盘卷「{mount_point}」已开启「忽略所有权」（Ignore ownership），\
             chown 会静默失败。请在「磁盘工具」中取消该卷的忽略所有权设置，或将程序数据目录\
             迁移到系统盘（如 ~/Library/Application Support）后重试。"
        );
    }

    // nosuid mount silently drops the setuid bit at exec time; chmod itself
    // still returns 0, so this also passes the write-error check above.
    let nosuid = Command::new("mount")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            text.lines()
                .any(|l| l.contains(&format!(" on {mount_point} ")) && l.contains("nosuid"))
        })
        .unwrap_or(false);
    if nosuid {
        return format!(
            "所在磁盘卷「{mount_point}」以 nosuid 方式挂载，setuid 位会被系统忽略。\
             请将程序数据目录迁移到系统盘（如 ~/Library/Application Support）后重试。"
        );
    }

    format!(
        "所在磁盘卷「{mount_point}」权限设置正常，仍可能被安全软件（如权限管理/杀毒工具）\
         还原了权限，或该卷不支持 setuid。建议将程序数据目录迁移到系统盘后重试。"
    )
}

/// Remove a root-owned setuid core so the user can replace it (core update).
pub fn remove_setuid_core_if_needed(core: &Path) -> AppResult<()> {
    if !core.is_file() || !core_has_setuid(core) {
        return Ok(());
    }
    let path_q = shell_single_quote(&core.to_string_lossy());
    let shell = format!("rm -f {path_q}");
    let (code, output) = run_privileged(Path::new("/bin/sh"), &["-c", &shell])?;
    if code != 0 {
        return Err(AppError::Core(format!(
            "无法删除旧的 setuid sing-box: {}",
            output.trim()
        )));
    }
    Ok(())
}

/// Remove a `cache.db` left behind by a core session that ran as a
/// different euid (typically root, under setuid TUN). The file is pure
/// cache (fakeip mappings + rule-set cache) — safe to drop; the core
/// rebuilds it on next start. Tries a plain remove first (covers the
/// common case where it's just not writable, not root-owned); only prompts
/// for admin when that fails.
pub fn remove_stale_cache_db(cache_db: &Path) -> AppResult<()> {
    if !cache_db.is_file() {
        return Ok(());
    }
    if std::fs::remove_file(cache_db).is_ok() {
        return Ok(());
    }
    let path_q = shell_single_quote(&cache_db.to_string_lossy());
    let shell = format!("rm -f {path_q}");
    let (code, output) = run_privileged(Path::new("/bin/sh"), &["-c", &shell])?;
    if code != 0 || cache_db.is_file() {
        return Err(AppError::Core(format!(
            "无法清理旧的 cache.db: {}",
            output.trim()
        )));
    }
    Ok(())
}

/// Run `tool args` as root.
///
/// Prefers `sudo` over a PTY so **Touch ID** (pam_tid) can appear; falls back to
/// password-based Authorization / osascript only if sudo is unavailable.
pub fn run_privileged(tool: &Path, args: &[&str]) -> AppResult<(i32, String)> {
    if !tool.is_file() {
        return Err(AppError::Core(format!(
            "privileged tool not found: {}",
            tool.display()
        )));
    }

    // 1) sudo + PTY → Touch ID when pam_tid is enabled (this Mac has it).
    match run_privileged_sudo_pty(tool, args) {
        Ok(r) => return Ok(r),
        Err(e) => {
            crate::app_log::warn(
                "auth",
                format!(
                    "sudo/PTY path failed ({e}); pam_tid={} — trying password fallbacks",
                    pam_tid_enabled()
                ),
            );
        }
    }

    // 2) AEWP (password-only by system policy)
    match run_privileged_aewp(tool, args) {
        Ok(r) => return Ok(r),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("取消") {
                return Err(e);
            }
            crate::app_log::warn("auth", format!("AEWP failed ({msg}); trying osascript"));
        }
    }

    // 3) osascript (password-only)
    run_privileged_osascript(tool, args)
}

/// `script` allocates a TTY so pam_tid can present the Touch ID UI.
fn run_privileged_sudo_pty(tool: &Path, args: &[&str]) -> AppResult<(i32, String)> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;

    let cmdline = build_tool_cmdline(tool, args);
    // Without pam_tid there is no Touch ID sheet; a bare `sudo -p ''` would sit
    // on this hidden PTY waiting for a password nobody can type (GUI apps have
    // no visible terminal) and the whole start flow stalled with zero UI
    // feedback until our own 120s deadline. `-n` makes sudo fail fast instead —
    // "a password is required" is already in interpret_sudo_output's fallback
    // list, so we drop straight to AEWP/osascript which surface real dialogs.
    let n_flag = if pam_tid_enabled() { "" } else { "-n " };
    // Keep sudo credential cache (default timestamp) for nicer repeat UX.
    let inner = format!(
        "sudo {n_flag}-p '' -- sh -c {q} 2>&1; printf '\\n__SATELITE_EXIT__:%d\\n' $?",
        q = shell_single_quote(&cmdline)
    );

    // macOS: script [-q] [typescript [command ...]]. Own process group so the
    // timeout kill below takes down script + sh + sudo together — killing only
    // the top `script` used to orphan the root sudo on the dead PTY forever.
    let mut child = Command::new("script")
        .args(["-q", "/dev/null", "sh", "-c", &inner])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::Core(format!("spawn script/sudo: {e}")))?;

    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                kill_child_process_group(child.id());
                let _ = child.wait();
                return Err(AppError::Core(
                    "sudo 授权超时（未在 120s 内完成指纹/密码）".into(),
                ));
            }
            Err(e) => return Err(AppError::Core(format!("sudo wait: {e}"))),
        }
    }

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut r) = child.stdout.take() {
        let _ = r.read_to_end(&mut stdout);
    }
    if let Some(mut r) = child.stderr.take() {
        let _ = r.read_to_end(&mut stderr);
    }
    let _ = child.wait();

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    interpret_sudo_output(&combined)
}

/// SIGKILL the whole process group of a child spawned with
/// `.process_group(0)` (group leader pid == pgid). Used on the auth timeout so
/// no root `sudo` survives on an orphaned PTY.
fn kill_child_process_group(pid: u32) {
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

fn interpret_sudo_output(combined: &str) -> AppResult<(i32, String)> {
    let lower = combined.to_ascii_lowercase();
    if lower.contains("a password is required")
        || lower.contains("no tty")
        || lower.contains("a terminal is required")
        || lower.contains("sorry, try again") && !combined.contains("__SATELITE_EXIT__:")
    {
        // Fall through to password path
        return Err(AppError::Core(format!("sudo needs fallback: {combined}")));
    }
    if lower.contains("user not allowed") || lower.contains("not in the sudoers") {
        return Err(AppError::Core(
            "当前用户不在 sudoers 中，无法使用 sudo/指纹提权".into(),
        ));
    }

    let exit_code = parse_exit_marker(combined).unwrap_or_else(|| {
        if combined.trim().is_empty() {
            1
        } else if lower.contains("error:") {
            1
        } else {
            0
        }
    });
    let cleaned = strip_exit_marker(combined);
    if exit_code != 0 && cleaned.is_empty() {
        return Err(AppError::Core(
            "sudo 提权失败且无输出（可尝试在终端执行: sudo -v 测试指纹）".into(),
        ));
    }
    Ok((exit_code, cleaned))
}

fn run_privileged_aewp(tool: &Path, args: &[&str]) -> AppResult<(i32, String)> {
    let exec = auth_exec_fn()
        .ok_or_else(|| AppError::Core("AuthorizationExecuteWithPrivileges unavailable".into()))?;

    let cmdline = build_tool_cmdline(tool, args);
    let shell = format!("{cmdline} 2>&1; printf '\\n__SATELITE_EXIT__:%d\\n' $?");
    let path_c = CString::new("/bin/sh").map_err(|e| AppError::Core(e.to_string()))?;
    let arg_c = CString::new("-c").map_err(|e| AppError::Core(e.to_string()))?;
    let cmd_c = CString::new(shell).map_err(|e| AppError::Core(e.to_string()))?;

    unsafe {
        let mut auth: AuthorizationRef = ptr::null_mut();
        let st = AuthorizationCreate(ptr::null(), ptr::null(), FLAG_DEFAULTS, &mut auth);
        if st != ERR_AUTH_SUCCESS {
            return Err(AppError::Core(auth_status_message(st)));
        }

        let right_name = CString::new("system.privilege.admin").unwrap();
        let mut item = AuthorizationItem {
            name: right_name.as_ptr(),
            value_length: 0,
            value: ptr::null_mut(),
            flags: 0,
        };
        let rights = AuthorizationRights {
            count: 1,
            items: &mut item,
        };
        let flags = FLAG_DEFAULTS
            | FLAG_INTERACTION_ALLOWED
            | FLAG_EXTEND_RIGHTS
            | FLAG_PRE_AUTHORIZE
            | FLAG_PARTIAL_RIGHTS;
        let st = AuthorizationCopyRights(auth, &rights, ptr::null(), flags, ptr::null_mut());
        if st != ERR_AUTH_SUCCESS {
            AuthorizationFree(auth, FLAG_DESTROY_RIGHTS);
            return Err(AppError::Core(auth_status_message(st)));
        }

        let mut argv_ptrs: Vec<*const c_char> = vec![arg_c.as_ptr(), cmd_c.as_ptr(), ptr::null()];
        let mut pipe: *mut libc::FILE = ptr::null_mut();
        let st = exec(
            auth,
            path_c.as_ptr(),
            FLAG_DEFAULTS,
            argv_ptrs.as_mut_ptr(),
            &mut pipe,
        );
        if st != ERR_AUTH_SUCCESS {
            AuthorizationFree(auth, FLAG_DESTROY_RIGHTS);
            return Err(AppError::Core(auth_status_message(st)));
        }

        let mut output = String::new();
        if !pipe.is_null() {
            let mut buf = [0u8; 4096];
            loop {
                let n = libc::fread(buf.as_mut_ptr() as *mut c_void, 1, buf.len(), pipe);
                if n == 0 {
                    break;
                }
                output.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
            libc::fclose(pipe);
        }
        AuthorizationFree(auth, FLAG_DESTROY_RIGHTS);

        let exit_code = parse_exit_marker(&output).unwrap_or(if output.is_empty() { 1 } else { 0 });
        let cleaned = strip_exit_marker(&output);
        if exit_code != 0 && cleaned.is_empty() {
            return Err(AppError::Core("AEWP 提权无输出".into()));
        }
        Ok((exit_code, cleaned))
    }
}

fn run_privileged_osascript(tool: &Path, args: &[&str]) -> AppResult<(i32, String)> {
    let cmdline = build_tool_cmdline(tool, args);
    let shell = format!("{cmdline} 2>&1; printf '\\n__SATELITE_EXIT__:%d\\n' $?");
    let inner = format!(
        "do shell script \"{}\" with administrator privileges with prompt \"Satelite 需要管理员权限以授权 TUN（为 sing-box 设置 setuid）。系统已启用 pam_tid 时会优先走 sudo 指纹；若看到密码框说明指纹路径不可用。\"",
        escape_applescript_string(&shell)
    );

    let output = Command::new("osascript")
        .arg("-e")
        .arg(&inner)
        .output()
        .map_err(|e| AppError::Core(format!("请求管理员权限失败: {e}")))?;

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    if !output.status.success() {
        let raw = combined.trim();
        if raw.contains("User canceled") || raw.contains("(-128)") || raw.contains("-128") {
            return Err(AppError::Core("已取消管理员授权".into()));
        }
        if let Some(code) = parse_exit_marker(&combined) {
            return Ok((code, strip_exit_marker(&combined)));
        }
        return Ok((1, strip_exit_marker(&combined)));
    }

    let exit_code = parse_exit_marker(&combined).unwrap_or(0);
    Ok((exit_code, strip_exit_marker(&combined)))
}
