//! Core process lifecycle (sing-box / Xray).

use crate::core::kind::CoreKind;
use crate::error::{AppError, AppResult};
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Windows process creation flag: do not allocate a console window for the child.
/// sing-box.exe is a console subsystem program, so without this a black cmd window
/// flashes on screen every time we spawn it.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error,
}

/// How the core process is owned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RunMode {
    #[default]
    None,
    /// Direct child of the GUI process (macOS TUN: setuid binary, still our Child).
    Sidecar,
    /// Windows TUN: elevated process tracked by PID only.
    #[allow(dead_code)] // constructed only on Windows
    ElevatedPid,
}

#[derive(Debug)]
pub struct CoreManager {
    child: Option<Child>,
    /// Windows TUN elevated process, or legacy macOS elevated (should be unused).
    elevated_pid: Option<u32>,
    run_mode: RunMode,
    state: CoreState,
    last_error: Option<String>,
    config_path: Option<PathBuf>,
    binary_path: Option<PathBuf>,
    log_path: Option<PathBuf>,
    log_dir: Option<PathBuf>,
    /// Which core the current/last session runs (log prefix, CLI args).
    kind: CoreKind,
    /// Ports the current/last session bound (mixed + api + extras). `stop()`
    /// waits for these to actually release before returning, so a restart
    /// immediately after never races the outgoing process for the socket.
    owned_ports: Vec<u16>,
}

impl Default for CoreManager {
    fn default() -> Self {
        Self {
            child: None,
            elevated_pid: None,
            run_mode: RunMode::None,
            state: CoreState::Stopped,
            last_error: None,
            config_path: None,
            binary_path: None,
            log_path: None,
            log_dir: None,
            kind: CoreKind::SingBox,
            owned_ports: Vec::new(),
        }
    }
}

impl CoreManager {
    pub fn state(&self) -> CoreState {
        self.state
    }

    /// True when the current core session is a Windows UAC-elevated
    /// process. Exact by construction — `elevated_pid` is set only right
    /// after this app's own `run_elevated` call succeeds (see
    /// `start_elevated_windows`), so there's no stale-state risk the way a
    /// filesystem-persisted bit (macOS setuid) would have: macOS instead
    /// reads the running process' actual uid every poll (`core::memory`,
    /// `read_process_is_root`) rather than remembering how it got started,
    /// because a setuid-root sing-box can predate this app session (started
    /// under old code, left running across an app restart, etc.) — a
    /// remembered flag would miss exactly that case.
    #[allow(dead_code)] // read only on non-macOS (runtime.rs's core_elevated)
    pub fn is_windows_elevated(&self) -> bool {
        matches!(self.run_mode, RunMode::ElevatedPid) && self.is_running()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn log_path(&self) -> Option<&Path> {
        self.log_path.as_deref()
    }

    fn latest_log_path(&self) -> Option<PathBuf> {
        self.log_dir
            .as_ref()
            .map(|dir| crate::log_retention::hourly_path(dir, self.kind.log_prefix()))
            .filter(|path| path.exists())
            .or_else(|| self.log_path.clone())
    }

    /// Tail of the current/last core session's log — surfaced by the
    /// `get_core_log_tail` command (Xray-mode traffic page).
    pub fn core_log_tail(&self, limit: usize) -> Option<(PathBuf, Vec<String>)> {
        let path = self.latest_log_path()?;
        Some((path.clone(), read_file_tail_lines(&path, limit)))
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, CoreState::Running)
    }

    /// Kind of the current/last session — set by `start_with_ports`. Callers
    /// wanting the *actual* running core (e.g. custom sing-box profiles keep
    /// running sing-box even when `settings.core_type` is xray) read this.
    pub fn kind(&self) -> CoreKind {
        self.kind
    }

    /// PID of the running core process, however it's owned (sidecar child or elevated).
    pub fn pid(&self) -> Option<u32> {
        match self.run_mode {
            RunMode::Sidecar => self.child.as_ref().map(|c| c.id()),
            RunMode::ElevatedPid => self.elevated_pid,
            RunMode::None => None,
        }
    }

    /// Reap child if it exited; update state with log tail when possible.
    pub fn poll(&mut self) {
        if let Some(pid) = self.elevated_pid {
            if !pid_alive(pid) {
                self.elevated_pid = None;
                self.run_mode = RunMode::None;
                if self.state == CoreState::Stopping {
                    self.state = CoreState::Stopped;
                } else if self.state != CoreState::Stopped {
                    self.state = CoreState::Error;
                    let detail = self
                        .latest_log_path()
                        .as_ref()
                        .and_then(|p| read_log_tail(p, 4000))
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| {
                            format!("elevated {} (pid {pid}) exited", self.kind.display_name())
                        });
                    crate::app_log::warn(
                        "core",
                        format!(
                            "{} exited unexpectedly (elevated pid {pid})",
                            self.kind.display_name()
                        ),
                    );
                    self.last_error = Some(map_tun_permission_hint(&strip_ansi(&detail)));
                }
            }
            return;
        }

        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.child = None;
                    self.run_mode = RunMode::None;
                    if self.state == CoreState::Stopping {
                        self.state = CoreState::Stopped;
                    } else {
                        self.state = CoreState::Error;
                        crate::app_log::warn(
                            "core",
                            format!(
                                "{} exited unexpectedly ({status})",
                                self.kind.display_name()
                            ),
                        );
                        let detail = self
                            .latest_log_path()
                            .as_ref()
                            .and_then(|p| read_log_tail(p, 4000))
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or_else(|| {
                                format!("{} exited: {status}", self.kind.display_name())
                            });
                        self.last_error = Some(map_tun_permission_hint(&strip_ansi(&detail)));
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    self.child = None;
                    self.run_mode = RunMode::None;
                    self.state = CoreState::Error;
                    self.last_error = Some(e.to_string());
                }
            }
        }
    }

    pub fn check_config(kind: CoreKind, binary: &Path, config: &Path) -> AppResult<()> {
        let mut cmd = Command::new(binary);
        cmd.args(kind.check_command_args(config));
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let out = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| AppError::Core(format!("check spawn failed: {e}")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let out_s = String::from_utf8_lossy(&out.stdout);
            let mut detail = String::new();
            let e = strip_ansi(err.trim());
            let o = strip_ansi(out_s.trim());
            if !e.is_empty() {
                detail.push_str(&e);
            }
            if !o.is_empty() {
                if !detail.is_empty() {
                    detail.push('\n');
                }
                detail.push_str(&o);
            }

            // SIGKILL / no message ⇒ process killed externally (not a JSON/DNS parse error).
            let status_s = out.status.to_string();
            let killed = status_s.contains("SIGKILL")
                || status_s.contains("signal: 9")
                || out.status.code().is_none() && detail.is_empty();
            if detail.is_empty() {
                detail = if killed {
                    "进程被系统强制结束 (SIGKILL)，通常不是配置/DNS 语法错误。\n\
                     常见原因：\n\
                     1) 路径未加引号：Application Support 含空格，须写成\n\
                        core check -c \"/Users/…/Application Support/…/active.json\"\n\
                     2) 从 target/debug/resources 直接跑内置内核可能被 macOS 杀掉\n\
                        （应用会复制到 Application Support/…/bin/ 再执行）\n\
                     3) 内存不足 / 安全软件拦截\n\
                     请用:  \"…/bin/core\" check -c \"…/active.json\""
                        .into()
                } else {
                    format!("exit status {status_s}")
                };
            } else if killed {
                detail = format!(
                    "{detail}\n(进程随后被 SIGKILL；若仅有此信号，优先排查路径空格/二进制路径，而非 DNS)"
                );
            }

            let message = format!(
                "{} check failed ({status_s})\nconfig: {}\nbinary: {}\n{detail}",
                kind.display_name(),
                config.display(),
                binary.display(),
            );
            crate::app_log::error("core", message.clone());
            return Err(AppError::Core(message));
        }
        Ok(())
    }

    /// True if nothing is listening on 127.0.0.1:port.
    pub fn is_port_free(port: u16) -> bool {
        TcpListener::bind(("127.0.0.1", port)).is_ok()
    }

    pub fn has_port_listener(port: u16) -> bool {
        port_has_listener(port)
    }

    /// Force-free a TCP listen port: kill listeners + short wait.
    ///
    /// Important: if nothing is in LISTEN, return immediately (or after one short
    /// settle). A false `bind` failure without a listener used to spin ~2s and
    /// made settings restarts feel stuck (e.g. changing route.final).
    pub fn force_free_port(port: u16) -> AppResult<()> {
        if Self::is_port_free(port) {
            return Ok(());
        }
        let mut killed = kill_listeners_on_port(port);

        // No server socket → do not busy-wait (CLOSE_WAIT / TIME_WAIT / bind flake).
        if !port_has_listener(port) {
            std::thread::sleep(Duration::from_millis(40));
            if Self::is_port_free(port) || !port_has_listener(port) {
                return Ok(());
            }
        }

        // Real LISTEN holder: wait briefly for kill to take effect (~360ms max).
        for i in 0..12 {
            if Self::is_port_free(port) {
                return Ok(());
            }
            if !port_has_listener(port) {
                return Ok(());
            }
            if i == 4 || i == 8 {
                killed = kill_listeners_on_port(port);
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        if Self::is_port_free(port) || !port_has_listener(port) {
            return Ok(());
        }
        let manual = if cfg!(windows) {
            format!("netstat -ano | findstr :{port}")
        } else {
            format!("sudo lsof -iTCP:{port} -sTCP:LISTEN")
        };
        let snapshot = port_socket_snapshot(port);
        Err(AppError::Core(format!(
            "端口 {port} 仍被占用（已尝试结束监听进程: {killed}）。可手动: {manual}\n当前端口状态: {snapshot}"
        )))
    }

    /// Ensure mixed + API ports are free (kill leftovers from previous runs).
    pub fn ensure_ports_free(ports: &[u16]) -> AppResult<()> {
        let list: Vec<u16> = ports.iter().copied().filter(|p| *p != 0).collect();
        if list.is_empty() {
            return Ok(());
        }
        for &p in &list {
            Self::force_free_port(p)?;
        }
        Ok(())
    }

    /// Start core.
    ///
    /// When `elevated` is true (TUN):
    /// - **macOS**: one-time setuid on the core binary (`chown root:admin` + `chmod +sx`),
    ///   then normal sidecar spawn (euid root, ruid user — parent can kill).
    /// - **Windows**: UAC-elevate the core directly.
    pub fn start_with_ports(
        &mut self,
        kind: CoreKind,
        binary: &Path,
        config: &Path,
        log_dir: &Path,
        mixed_port: u16,
        api_port: Option<u16>,
        extra_ports: &[u16],
        elevated: bool,
        resource_dir: Option<&Path>,
    ) -> AppResult<()> {
        match self.start_with_ports_inner(
            kind,
            binary,
            config,
            log_dir,
            mixed_port,
            api_port,
            extra_ports,
            elevated,
            resource_dir,
        ) {
            Err(e) if is_stale_cache_db_error(&e.to_string()) => {
                // A previous session (usually setuid-root under TUN) left
                // cache.db owned by another user/euid; this session's core
                // can no longer open it for writing. The file is pure cache
                // (fakeip mappings + rule-set cache) — safe to drop and let
                // the core rebuild it. Removing a root-owned file needs the
                // same one-time admin prompt as setuid, so reuse it here
                // instead of surfacing a dead-end "permission denied".
                #[cfg(target_os = "macos")]
                if let Some(dir) = config.parent() {
                    let cache_path = dir.join("cache.db");
                    crate::app_log::warn(
                        "core",
                        format!(
                            "stale cache.db blocked start ({e}); removing with admin prompt: {}",
                            cache_path.display()
                        ),
                    );
                    super::macos_auth::remove_stale_cache_db(&cache_path)?;
                    return self.start_with_ports_inner(
                        kind,
                        binary,
                        config,
                        log_dir,
                        mixed_port,
                        api_port,
                        extra_ports,
                        elevated,
                        resource_dir,
                    );
                }
                Err(e)
            }
            other => other,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn start_with_ports_inner(
        &mut self,
        kind: CoreKind,
        binary: &Path,
        config: &Path,
        log_dir: &Path,
        mixed_port: u16,
        api_port: Option<u16>,
        extra_ports: &[u16],
        elevated: bool,
        _resource_dir: Option<&Path>,
    ) -> AppResult<()> {
        self.poll();
        if matches!(self.state, CoreState::Running | CoreState::Starting) {
            return Ok(());
        }

        // Drop our own child first if still tracked.
        let _ = self.stop();
        let mut ports = Vec::new();
        if mixed_port != 0 {
            ports.push(mixed_port);
        }
        if let Some(api) = api_port {
            if api != 0 && api != mixed_port {
                ports.push(api);
            }
        }
        for &p in extra_ports {
            if p != 0 && !ports.contains(&p) {
                ports.push(p);
            }
        }
        if !ports.is_empty() {
            Self::ensure_ports_free(&ports)?;
        }
        self.owned_ports = ports.clone();

        #[cfg(target_os = "macos")]
        if elevated {
            if let Err(e) = super::macos_auth::ensure_core_setuid(binary) {
                self.state = CoreState::Error;
                let msg = map_tun_permission_hint(&e.to_string());
                self.last_error = Some(msg.clone());
                return Err(AppError::Core(msg));
            }
        } else if let Some(prev) = self.binary_path.as_deref() {
            // Switching away from a core that was setuid-elevated for TUN (its
            // binary is still root-owned setuid): a non-elevated bind on the
            // just-vacated ports can race that root socket's teardown even
            // after the process and its listener are gone. Elevating the new
            // core too clears that race — mirrors what happens whenever the
            // new core is later toggled into TUN itself.
            if prev != binary && super::macos_auth::core_has_setuid(prev) {
                if let Err(e) = super::macos_auth::ensure_core_setuid(binary) {
                    crate::app_log::warn(
                        "core",
                        format!("setuid carry-over for {} failed: {e}", kind.display_name()),
                    );
                }
            }
        }

        // sing-box `check -c` is pure JSON validation, but Xray's
        // `run -test -c` actually creates the tun adapter for tun configs —
        // which needs admin. Running it unelevated fails with access-denied
        // before the elevated start path is ever reached, so elevated Xray
        // sessions skip the pre-check; the elevated run surfaces config
        // errors through the log tail instead.
        let skip_check = elevated && kind == CoreKind::Xray;
        if !skip_check {
            Self::check_config(kind, binary, config)?;
        }
        // Light re-check only (first ensure_ports_free already waited if needed).
        for &p in &ports {
            if !Self::is_port_free(p) && port_has_listener(p) {
                Self::force_free_port(p)?;
            }
        }

        fs::create_dir_all(log_dir).map_err(|e| AppError::Core(format!("create log dir: {e}")))?;
        // One file per wall-clock hour. Core restarts within that hour append,
        // preserving the sequence around TUN and capture-mode transitions.
        let log_path = crate::log_retention::hourly_path(log_dir, kind.log_prefix());
        crate::log_retention::cleanup_current_hour(log_dir)
            .map_err(|e| AppError::Core(format!("clean logs: {e}")))?;
        let _ = open_hourly_log(&log_path)
            .map_err(|e| AppError::Core(format!("open log: {e}")))?;

        self.state = CoreState::Starting;
        self.last_error = None;
        self.log_path = Some(log_path.clone());
        self.log_dir = Some(log_dir.to_path_buf());
        self.config_path = Some(config.to_path_buf());
        self.binary_path = Some(binary.to_path_buf());
        self.kind = kind;
        self.elevated_pid = None;
        self.child = None;
        self.run_mode = RunMode::None;

        #[cfg(target_os = "windows")]
        if elevated {
            return self.start_elevated_windows(kind, binary, config, &log_path, mixed_port);
        }

        let run_args = kind.run_command_args(config);
        crate::app_log::info(
            "core",
            format!(
                "starting {}: {}",
                kind.display_name(),
                format_command_line(binary, &run_args)
            ),
        );
        let mut cmd = Command::new(binary);
        cmd.args(&run_args);
        // sing-box writes cache.db (fakeip/rule-set cache) relative to its cwd.
        // GUI apps launched from Finder/Dock inherit cwd "/" (read-only), which
        // makes cache_file init FATAL as soon as it's enabled. Anchor cwd to the
        // config's own directory (always writable — active.json lives there).
        if let Some(dir) = config.parent() {
            cmd.current_dir(dir);
        }
        if let Some(bin_dir) = binary.parent() {
            cmd.envs(kind.spawn_env(bin_dir));
        }
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                let msg = format!("spawn {} failed: {e}", kind.display_name());
                crate::app_log::error("core", msg.clone());
                self.state = CoreState::Error;
                self.last_error = Some(e.to_string());
                AppError::Core(msg)
            })?;

        // Tie the child to the parent's lifetime via a Job Object: if this
        // process dies for any reason (crash, installer kill, Task Manager),
        // Windows reaps sing-box too — preventing orphaned ports on next launch.
        #[cfg(target_os = "windows")]
        {
            if let Err(e) = super::job::ensure_child_killed_on_parent_exit(child.id()) {
                crate::app_log::warn(
                    "core",
                    format!("job-object bind failed (orphan possible on crash): {e}"),
                );
            }
        }

        #[cfg(target_os = "macos")]
        if elevated {
            crate::app_log::info(
                "core",
                format!(
                    "started setuid sing-box as sidecar pid={} (TUN)",
                    child.id()
                ),
            );
        }

        let mut child = child;
        let writer = std::sync::Arc::new(std::sync::Mutex::new(RotatingCoreWriter::new(
            log_dir.to_path_buf(),
            kind.log_prefix(),
        )));
        if let Some(stdout) = child.stdout.take() {
            spawn_rotating_log_copy(stdout, std::sync::Arc::clone(&writer));
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_rotating_log_copy(stderr, writer);
        }
        self.child = Some(child);
        self.run_mode = RunMode::Sidecar;

        self.wait_until_ready(mixed_port)
    }

    /// Start the core elevated via UAC (Windows). Needed for TUN to create the
    /// virtual adapter. stdout/stderr are appended to `log_path` directly.
    /// The kind is re-derived inside the elevated helper from the binary path,
    /// so it is not forwarded again here.
    #[cfg(target_os = "windows")]
    fn start_elevated_windows(
        &mut self,
        kind: CoreKind,
        binary: &Path,
        config: &Path,
        _log_path: &Path,
        mixed_port: u16,
    ) -> AppResult<()> {
        crate::app_log::info(
            "core",
            format!(
                "starting {} (elevated): {}",
                kind.display_name(),
                format_command_line(binary, &kind.run_command_args(config))
            ),
        );
        let helper = std::env::current_exe()
            .map_err(|e| AppError::Core(format!("resolve log helper: {e}")))?;
        let log_dir = self
            .log_dir
            .as_ref()
            .ok_or_else(|| AppError::Core("log directory missing".into()))?;
        let args = format!(
            "--satelite-core-helper \"{}\" \"{}\" \"{}\"",
            escape_windows_arg(binary),
            escape_windows_arg(config),
            escape_windows_arg(log_dir)
        );

        let _elevated = match super::elevate::run_elevated(&helper, &args, None) {
            Ok(c) => c,
            Err(e) => {
                self.state = CoreState::Error;
                self.last_error = Some(e.to_string());
                return Err(e);
            }
        };
        // run_elevated returns an ElevatedChild that closes the handle on drop;
        // we only need the PID — we poll via OpenProcess later (elevate::pid_alive)
        // and kill via taskkill. Dropping here is fine: closing the handle does
        // NOT terminate the process.
        let pid = _elevated.pid;

        self.elevated_pid = Some(pid);
        self.run_mode = RunMode::ElevatedPid;
        self.wait_until_ready(mixed_port)
    }

    /// Common failure tail for `wait_until_ready`: the process died while we
    /// were waiting for it to bind. Logs the core's own output (via
    /// `last_error`, populated by `poll` from the log tail) so startup
    /// failures land in the app log, not just the returned error.
    fn start_failed_error(&mut self) -> AppError {
        let err = self
            .last_error
            .clone()
            .unwrap_or_else(|| "process exited immediately".into());
        crate::app_log::error(
            "core",
            format!("{} failed to start: {err}", self.kind.display_name()),
        );
        self.state = CoreState::Error;
        self.run_mode = RunMode::None;
        AppError::Core(map_tun_permission_hint(&err))
    }

    fn wait_until_ready(&mut self, mixed_port: u16) -> AppResult<()> {
        // wait a bit for immediate FATAL
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            self.poll();
            if !self.process_tracked_alive() {
                return Err(self.start_failed_error());
            }
            if !Self::is_port_free(mixed_port) {
                break;
            }
        }

        self.poll();
        if !self.process_tracked_alive() {
            return Err(self.start_failed_error());
        }

        self.state = CoreState::Running;
        Ok(())
    }

    fn process_tracked_alive(&self) -> bool {
        match self.run_mode {
            RunMode::ElevatedPid => self.elevated_pid.map(pid_alive).unwrap_or(false),
            RunMode::Sidecar => self.child.is_some(),
            RunMode::None => false,
        }
    }

    pub fn stop(&mut self) -> AppResult<()> {
        self.poll();

        if let Some(pid) = self.elevated_pid {
            self.state = CoreState::Stopping;
            elevated_kill(pid);
            let deadline = std::time::Instant::now() + Duration::from_secs(4);
            while pid_alive(pid) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(80));
            }
            if pid_alive(pid) {
                elevated_kill_force(pid);
            }
            if pid_alive(pid) {
                // Do NOT forget the PID — caller must know stop failed.
                self.state = CoreState::Error;
                self.last_error = Some(format!(
                    "无法结束 elevated sing-box (pid {pid})；可能需要管理员权限"
                ));
                return Err(AppError::Core(self.last_error.clone().unwrap_or_default()));
            }
            self.elevated_pid = None;
            self.run_mode = RunMode::None;
            self.state = CoreState::Stopped;
            self.last_error = None;
            return Ok(());
        }

        let Some(mut child) = self.child.take() else {
            self.run_mode = RunMode::None;
            self.state = CoreState::Stopped;
            return Ok(());
        };

        self.state = CoreState::Stopping;

        #[cfg(unix)]
        {
            let _ = Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status();
        }
        #[cfg(not(unix))]
        {
            let _ = child.kill();
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                Err(_) => break,
            }
        }

        self.run_mode = RunMode::None;
        self.state = CoreState::Stopped;
        self.last_error = None;
        Ok(())
    }

    /// Wait for the ports the just-stopped session owned to actually clear
    /// (process-exited and socket-released are not the same instant — the OS
    /// can lag a moment after `wait()`/`try_wait()` returns). Bounded so a
    /// stuck/leaked port never hangs a restart; the next start's own
    /// `ensure_ports_free` sweep is the final backstop either way.
    ///
    /// Callers opt in explicitly (rather than this running inside `stop()`
    /// itself) because it spawns `lsof`/`netstat` to probe each port, which
    /// `force_shutdown` must never do during app-exit shutdown (see there).
    pub fn await_owned_ports_released(&mut self) {
        let ports = std::mem::take(&mut self.owned_ports);
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        for port in ports {
            while Self::has_port_listener(port) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    /// Hard-stop the managed core process during application exit.
    ///
    /// Do not run the port-orphan sweep here. On Windows, the OS rejects new
    /// process creation once shutdown is in progress, so spawning `netstat`
    /// from the exit callback produces a visible "netstat failed to start"
    /// error. The managed sidecar/elevated PID is stopped directly above; any
    /// stale listener is handled by `ensure_ports_free` on the next startup.
    pub fn force_shutdown(&mut self) {
        let _ = self.stop();
        self.state = CoreState::Stopped;
        self.child = None;
        self.elevated_pid = None;
        self.run_mode = RunMode::None;
        self.last_error = None;
    }
}

#[cfg(target_os = "windows")]
fn escape_windows_arg(path: &Path) -> String {
    path.to_string_lossy().replace('"', "\\\"")
}

/// Elevated helper entry point. It owns the real core child, captures both
/// output streams through the same hourly writer, and binds the child to a Job
/// Object so killing the helper also kills the core.
#[cfg(target_os = "windows")]
pub fn try_run_elevated_log_helper() -> Option<i32> {
    let args: Vec<_> = std::env::args_os().collect();
    let marker = args
        .iter()
        .position(|value| value == "--satelite-core-helper")?;
    if args.len() <= marker + 3 {
        return Some(2);
    }
    let binary = PathBuf::from(&args[marker + 1]);
    let config = PathBuf::from(&args[marker + 2]);
    let log_dir = PathBuf::from(&args[marker + 3]);
    let kind = CoreKind::from_binary_path(&binary);
    if fs::create_dir_all(&log_dir).is_err() {
        return Some(3);
    }
    let mut command = Command::new(&binary);
    command
        .args(kind.run_command_args(&config))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    // See start_with_ports: anchor cwd to the config dir so cache.db has a
    // writable, predictable location regardless of the launcher's own cwd.
    if let Some(dir) = config.parent() {
        command.current_dir(dir);
    }
    if let Some(bin_dir) = binary.parent() {
        command.envs(kind.spawn_env(bin_dir));
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return Some(4),
    };
    if super::job::ensure_child_killed_on_parent_exit(child.id()).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Some(5);
    }
    let writer = std::sync::Arc::new(std::sync::Mutex::new(RotatingCoreWriter::new(
        log_dir,
        kind.log_prefix(),
    )));
    if let Some(stdout) = child.stdout.take() {
        spawn_rotating_log_copy(stdout, std::sync::Arc::clone(&writer));
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_rotating_log_copy(stderr, writer);
    }
    Some(
        child
            .wait()
            .ok()
            .and_then(|status| status.code())
            .unwrap_or(1),
    )
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(target_os = "windows")]
    {
        super::elevate::pid_alive(pid)
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = pid;
        false
    }
}

/// Terminate an elevated (non-service) sing-box process.
/// Windows: parent retains PROCESS_TERMINATE. macOS legacy path: osascript.
fn elevated_kill(pid: u32) {
    #[cfg(target_os = "macos")]
    {
        let shell = format!("kill -TERM {pid} 2>/dev/null || true");
        let script = format!(
            "do shell script \"{}\" with administrator privileges",
            shell.replace('\\', "\\\\").replace('"', "\\\"")
        );
        let _ = Command::new("osascript").arg("-e").arg(&script).status();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = super::elevate::terminate_pid(pid);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
}

fn elevated_kill_force(pid: u32) {
    #[cfg(target_os = "macos")]
    {
        let shell = format!("kill -KILL {pid} 2>/dev/null || true");
        let script = format!(
            "do shell script \"{}\" with administrator privileges",
            shell.replace('\\', "\\\\").replace('"', "\\\"")
        );
        let _ = Command::new("osascript").arg("-e").arg(&script).status();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = super::elevate::terminate_pid(pid);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
}

/// Format a core command line the way a shell would need it: binary and any
/// argument containing spaces (config paths live under "Application Support")
/// get quoted, so the logged line can be copied into a terminal as-is.
fn format_command_line(binary: &Path, args: &[String]) -> String {
    let quote = |arg: &str| {
        if arg.contains(' ') {
            format!("\"{arg}\"")
        } else {
            arg.to_string()
        }
    };
    std::iter::once(quote(&binary.display().to_string()))
        .chain(args.iter().map(|a| quote(a)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// True when `err` is sing-box's `cache_file` init dying on a `cache.db`
/// left behind by a *different* euid — typically a setuid-root TUN session
/// from before a core update dropped the setuid bit. Deliberately narrower
/// than the generic TUN-permission match below: this is a stale-file
/// problem, not a missing-privilege one, and gets its own recovery path
/// (delete-and-retry in `start_with_ports`) instead of the TUN hint, which
/// would send the user to toggle a switch that has nothing to do with it.
fn is_stale_cache_db_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("cache.db") && lower.contains("permission denied")
}

fn map_tun_permission_hint(err: &str) -> String {
    let lower = err.to_ascii_lowercase();
    if lower.contains("operation not permitted")
        || lower.contains("configure tun")
        || lower.contains("permission denied")
        || lower.contains("access is denied")
        // Chinese Windows error text / wintun HRESULT, e.g. from Xray's tun
        // adapter creation ("拒绝访问。 (Code 0x00000005)").
        || err.contains("拒绝访问")
        || err.contains("0x00000005")
    {
        let platform_hint = if cfg!(target_os = "windows") {
            "TUN 模式需要管理员权限以创建虚拟网卡。开启 TUN 时应用会弹出 UAC 授权框并以管理员身份运行内核。\n\
             请在 UAC 弹窗中点「是」；若点了「否」，请关闭 TUN 开关后重试，或以管理员身份运行本程序。"
        } else {
            "TUN 需要更高权限才能创建虚拟网卡 (utun)。\n\
             macOS：首次开启 TUN 会为当前内核设置 setuid（一次指纹/密码授权），之后启停不再弹窗。\n\
             若刚更新过内核，可能需重新授权一次。"
        };
        format!("{err}\n\n{platform_hint}")
    } else {
        err.to_string()
    }
}

fn port_has_listener(port: u16) -> bool {
    !listener_pids_on_port(port).is_empty()
}

/// Snapshot of every socket on `port` (any TCP state, any owner) for diagnostics
/// when `force_free_port` gives up. Unlike `listener_pids_on_port`, this is not
/// filtered to LISTEN, so it also surfaces TIME_WAIT/CLOSE_WAIT stragglers and
/// the owning user, to tell "not really free" apart from "kernel still cooling
/// the port down" without asking the user to re-run `lsof` by hand.
fn port_socket_snapshot(port: u16) -> String {
    #[cfg(unix)]
    {
        let out = Command::new("lsof")
            .args(["-nP", &format!("-iTCP:{port}")])
            .output();
        match out {
            Ok(out) if !out.stdout.is_empty() => {
                let text = String::from_utf8_lossy(&out.stdout);
                let lines: Vec<&str> = text.lines().collect();
                if lines.len() <= 1 {
                    "无 lsof 记录".into()
                } else {
                    lines.join(" | ")
                }
            }
            Ok(_) => "无 lsof 记录".into(),
            Err(e) => format!("lsof 不可用: {e}"),
        }
    }
    #[cfg(not(unix))]
    {
        let mut cmd = Command::new("netstat");
        cmd.args(["-ano"]);
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let needle = format!(":{port}");
        match cmd.output() {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                let matches: Vec<&str> = text.lines().filter(|l| l.contains(&needle)).collect();
                if matches.is_empty() {
                    "无 netstat 记录".into()
                } else {
                    matches.join(" | ")
                }
            }
            Err(e) => format!("netstat 不可用: {e}"),
        }
    }
}

fn listener_pids_on_port(port: u16) -> Vec<u32> {
    #[cfg(unix)]
    {
        let out = Command::new("lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
            .output();
        let Ok(out) = out else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let mut pids: Vec<u32> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
        pids.sort_unstable();
        pids.dedup();
        let self_pid = std::process::id();
        pids.retain(|p| *p != self_pid);
        pids
    }
    #[cfg(not(unix))]
    {
        let _ = port;
        Vec::new()
    }
}

/// Kill PIDs listening on `port` (TCP LISTEN). Returns a short summary string.
fn kill_listeners_on_port(port: u16) -> String {
    #[cfg(unix)]
    {
        let pids = listener_pids_on_port(port);
        if pids.is_empty() {
            return "未找到监听进程".into();
        }
        let mut killed = Vec::new();
        for pid in pids {
            // setuid core keeps ruid=user so TERM/KILL from the app works.
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
            std::thread::sleep(Duration::from_millis(80));
            let still = Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if still {
                let _ = Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .status();
            }
            killed.push(pid.to_string());
        }
        format!("已结束 PID {}", killed.join(","))
    }
    #[cfg(not(unix))]
    {
        // netstat -ano lists every TCP row with the owning PID in the last column.
        // We find rows whose local address ends with ":<port>" in LISTENING state,
        // then taskkill each owning PID.
        let mut cmd = Command::new("netstat");
        cmd.args(["-ano"]);
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let out = match cmd.output() {
            Ok(o) => o,
            Err(e) => return format!("netstat 不可用: {e}"),
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let needle = format!(":{port}");
        let mut pids: Vec<u32> = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            // Row shape: "  TCP    127.0.0.1:2080     0.0.0.0:0    LISTENING    10528"
            if !trimmed.to_ascii_uppercase().contains("LISTENING") {
                continue;
            }
            if !trimmed.contains(&needle) {
                continue;
            }
            // PID is the last whitespace-delimited token.
            if let Some(pid) = trimmed
                .split_whitespace()
                .last()
                .and_then(|s| s.parse().ok())
            {
                pids.push(pid);
            }
        }
        pids.sort_unstable();
        pids.dedup();
        // Don't kill ourselves
        let self_pid = std::process::id();
        pids.retain(|p| *p != self_pid);
        if pids.is_empty() {
            return "未找到监听进程".into();
        }
        let mut killed = Vec::new();
        for pid in pids {
            // taskkill /F /T: force-kill the process tree (sing-box may have children).
            let mut k = Command::new("taskkill");
            k.args(["/F", "/T", "/PID", &pid.to_string()]);
            #[cfg(target_os = "windows")]
            k.creation_flags(CREATE_NO_WINDOW);
            match k.status() {
                Ok(s) if s.success() => killed.push(pid.to_string()),
                _ => killed.push(format!("{pid}?(失败)")),
            }
        }
        format!("已结束 PID {}", killed.join(","))
    }
}

/// Last `limit` lines of a log file, read efficiently from the end (a 256 KB
/// window covers ~2000 typical core log lines). The first line is dropped
/// when the window sliced mid-line; invalid UTF-8 bytes become U+FFFD.
fn read_file_tail_lines(path: &Path, limit: usize) -> Vec<String> {
    let Ok(mut f) = File::open(path) else {
        return Vec::new();
    };
    let Ok(len) = f.metadata().map(|m| m.len()) else {
        return Vec::new();
    };
    const WINDOW: u64 = 256 * 1024;
    let window = len.min(WINDOW);
    if std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(len - window)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::with_capacity(window as usize);
    if std::io::Read::read_to_end(&mut f, &mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if window < len && !lines.is_empty() {
        lines.remove(0); // possibly sliced mid-line
    }
    if lines.len() > limit {
        lines.drain(..lines.len() - limit);
    }
    lines
}

fn read_log_tail(path: &Path, max_bytes: u64) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    // prefer last FATAL/ERROR line
    let useful: Vec<&str> = buf
        .lines()
        .filter(|l| {
            let u = l.to_ascii_uppercase();
            u.contains("FATAL") || u.contains("ERROR") || u.contains("FAILED")
        })
        .collect();
    if let Some(last) = useful.last() {
        return Some((*last).to_string());
    }
    Some(buf.trim().to_string())
}

/// Open (create if missing) an hourly core log file for append. A setuid-TUN
/// session can leave that hourly file owned by root without a user write bit,
/// so a same-hour relaunch fails here with EACCES and takes the whole core
/// launch down ("open log: Permission denied"). The file is a disposable
/// console tail, not user data — and the parent dir is writable — so on
/// PermissionDenied we just unlink it and retry.
fn open_hourly_log(path: &Path) -> std::io::Result<File> {
    let try_open = || OpenOptions::new().create(true).append(true).open(path);
    match try_open() {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            crate::app_log::warn(
                "core",
                format!(
                    "log file not writable (stale root-owned?), recreating: {}",
                    path.display()
                ),
            );
            fs::remove_file(path)?;
            try_open()
        }
        Err(e) => Err(e),
    }
}

struct RotatingCoreWriter {
    log_dir: PathBuf,
    log_prefix: &'static str,
    file_hour: Option<u64>,
    file: Option<File>,
    file_bytes: u64,
    bytes_since_cleanup: u64,
}

impl RotatingCoreWriter {
    fn new(log_dir: PathBuf, log_prefix: &'static str) -> Self {
        Self {
            log_dir,
            log_prefix,
            file_hour: None,
            file: None,
            file_bytes: 0,
            bytes_since_cleanup: 0,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        let hour = crate::log_retention::current_hour();
        if self.file_hour != Some(hour) || self.file.is_none() {
            let path = crate::log_retention::hourly_path_for(&self.log_dir, self.log_prefix, hour);
            match open_hourly_log(&path) {
                Ok(opened) => {
                    self.file_bytes = opened.metadata().map(|m| m.len()).unwrap_or(0);
                    self.bytes_since_cleanup = 0;
                    self.file = Some(opened);
                    self.file_hour = Some(hour);
                    let _ = crate::log_retention::cleanup_current_hour(&self.log_dir);
                }
                Err(error) => {
                    crate::app_log::error("core_log", format!("open {}: {error}", path.display()));
                    self.file = None;
                    self.file_hour = None;
                    self.file_bytes = 0;
                    return;
                }
            }
        }
        if self.file_bytes >= crate::log_retention::CORE_ACTIVE_MAX_BYTES {
            return;
        }
        let allowed = crate::log_retention::CORE_ACTIVE_MAX_BYTES - self.file_bytes;
        let write_count = bytes.len().min(allowed as usize);
        let Some(active) = self.file.as_mut() else {
            return;
        };
        if let Err(error) = active
            .write_all(&bytes[..write_count])
            .and_then(|_| active.flush())
        {
            crate::app_log::error("core_log", format!("write: {error}"));
            self.file = None;
            self.file_hour = None;
            self.file_bytes = 0;
            return;
        }
        self.file_bytes = self.file_bytes.saturating_add(write_count as u64);
        self.bytes_since_cleanup = self.bytes_since_cleanup.saturating_add(write_count as u64);
        if self.bytes_since_cleanup >= 1024 * 1024 {
            let _ = crate::log_retention::cleanup_current_hour(&self.log_dir);
            self.bytes_since_cleanup = 0;
        }
    }
}

/// Copy one core output stream into the active hourly file. Reading in chunks
/// avoids blocking sing-box on a full pipe; each chunk is flushed so a crash
/// still leaves a useful final log tail.
fn spawn_rotating_log_copy<R>(
    mut reader: R,
    writer: std::sync::Arc<std::sync::Mutex<RotatingCoreWriter>>,
) where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) => {
                    crate::app_log::warn("core_log", format!("read core output: {error}"));
                    break;
                }
            };
            writer
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .write(&buffer[..count]);
        }
    });
}

fn strip_ansi(s: &str) -> String {
    // remove simple ANSI color sequences
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for x in chars.by_ref() {
                    if x.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod cwd_tests {
    //! Regression coverage for the sing-box child process cwd (bug: GUI apps
    //! launched from Finder/Dock inherit cwd "/", which is read-only and
    //! makes `cache_file` init FATAL as soon as fakeip persistence is on).

    /// The real fix is `Command::current_dir(config.parent())` in
    /// `start_with_ports`/`try_run_elevated_log_helper`. We can't spawn the
    /// real sing-box binary here, but we can prove the child process actually
    /// inherits the directory we set — i.e. that `current_dir` on
    /// `std::process::Command` does what the fix relies on, using the same
    /// "config file lives in a directory, anchor cwd to its parent" shape.
    #[test]
    fn child_process_cwd_follows_config_parent_dir() {
        let tmp = std::env::temp_dir().join(format!(
            "satelite-cwd-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let config = tmp.join("active.json");
        std::fs::write(&config, "{}").unwrap();

        let mut cmd = std::process::Command::new("pwd");
        if let Some(dir) = config.parent() {
            cmd.current_dir(dir);
        }
        let out = cmd.output().expect("spawn pwd");
        let printed = String::from_utf8_lossy(&out.stdout).trim().to_string();

        // Canonicalize both sides: macOS temp dirs are often symlinks
        // (/tmp -> /private/tmp) and `pwd` prints the resolved path.
        let expected = tmp.canonicalize().unwrap_or(tmp.clone());
        let actual = std::path::PathBuf::from(&printed)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(&printed));
        assert_eq!(actual, expected, "child cwd did not follow config parent");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// In practice `config` is always the absolute `active_config_path()`
    /// (see `config::active_config_path`), so `.parent()` is always `Some`
    /// with a real directory. This just documents that the fix's
    /// `if let Some(dir) = config.parent()` guard never panics building the
    /// command, even for a degenerate path — it stays a no-op instead.
    #[test]
    fn missing_parent_does_not_panic_building_the_command() {
        let config = std::path::PathBuf::from("active.json");
        let mut cmd = std::process::Command::new("true");
        if let Some(dir) = config.parent() {
            cmd.current_dir(dir);
        }
        // Building/inspecting the command must not panic regardless of what
        // `parent()` returned for this degenerate path.
        let _ = format!("{cmd:?}");
    }
}

#[cfg(test)]
mod stale_cache_db_tests {
    //! `is_stale_cache_db_error` must fire on the exact FATAL sing-box emits
    //! when `cache.db` is owned by a different euid (stale setuid-root
    //! leftover) — the case that needs delete-and-retry, not the TUN-setuid
    //! hint. It must NOT fire on a generic TUN "permission denied" (adapter
    //! creation denied), which needs the setuid prompt instead: routing that
    //! case here would retry a delete that can't fix a missing privilege
    //! and never show the user the hint that actually gets them unstuck.
    use super::is_stale_cache_db_error;

    #[test]
    fn matches_the_real_cache_file_fatal() {
        let err =
            "FATAL[0001] start service: initialize cache-file: open cache.db: permission denied";
        assert!(is_stale_cache_db_error(err));
    }

    #[test]
    fn does_not_match_generic_tun_permission_denied() {
        let err = "operation not permitted: configure tun";
        assert!(!is_stale_cache_db_error(err));
    }

    #[test]
    fn does_not_match_permission_denied_without_cache_db() {
        // Some other file under the config dir could also hit EACCES; only
        // cache.db has the known stale-euid failure mode and a safe fix
        // (delete — it's rebuildable cache, not user data).
        let err = "open active.json: permission denied";
        assert!(!is_stale_cache_db_error(err));
    }
}

#[cfg(test)]
mod is_windows_elevated_tests {
    //! `is_windows_elevated()` backs the dashboard's ROOT badge on Windows.
    //! It must reflect BOTH "the tracked run mode is the elevated one" AND
    //! "the process is actually still running" — a stale `ElevatedPid` from
    //! a session that already exited should not light the badge (the
    //! sidecar `Sidecar` mode, or the default `None`, must not either).
    use super::{CoreManager, CoreState, RunMode};

    #[test]
    fn false_by_default() {
        let mgr = CoreManager::default();
        assert!(!mgr.is_windows_elevated());
    }

    #[test]
    fn false_when_elevated_pid_mode_but_not_running() {
        let mut mgr = CoreManager::default();
        mgr.run_mode = RunMode::ElevatedPid;
        mgr.elevated_pid = Some(4242);
        mgr.state = CoreState::Stopped;
        assert!(!mgr.is_windows_elevated());
    }

    #[test]
    fn false_when_running_as_plain_sidecar() {
        let mut mgr = CoreManager::default();
        mgr.run_mode = RunMode::Sidecar;
        mgr.state = CoreState::Running;
        assert!(!mgr.is_windows_elevated());
    }

    #[test]
    fn true_when_elevated_pid_mode_and_running() {
        let mut mgr = CoreManager::default();
        mgr.run_mode = RunMode::ElevatedPid;
        mgr.elevated_pid = Some(4242);
        mgr.state = CoreState::Running;
        assert!(mgr.is_windows_elevated());
    }
}
