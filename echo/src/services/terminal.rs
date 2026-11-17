//! Terminal service — a real PTY-backed interactive shell.
//!
//! Replaces Bloom's `TerminalApp.tsx`, which was a purely client-side
//! xterm.js instance running a hand-rolled fake command switch (`help`,
//! `ls` against a hardcoded `FAKE_FILESYSTEM`, etc.) with no real process
//! behind it at all. This service opens a genuine pseudo-terminal
//! (`nix::pty::openpty`), forks a real shell (`/bin/bash`) attached to the
//! slave side as its controlling terminal, and streams the master side's
//! output back over Echo as `TerminalOutput` signals, with keystrokes
//! written back in via `Write`. The shell now does everything a real shell
//! does — job control, `$PATH` resolution, pipes, real `ls`, tab
//! completion via bash's own line editing, etc. — because it *is* one.
//!
//! Kept in-process inside `echo` daemon, same convention as
//! [`crate::services::apphost::AppHost`]/[`crate::services::nova::Nova`].
//!
//! Known simplification (v1, not a correctness bug for typical shell use):
//! each PTY output chunk is converted with `String::from_utf8_lossy`
//! independently, so a multi-byte UTF-8 character split exactly across two
//! `read()`s could render as a replacement character. Buffering partial
//! multi-byte sequences across reads would fix this; not done here since
//! it's a rare cosmetic edge case (most PTY output is ASCII control
//! sequences + ASCII text) and shouldn't block landing a real terminal.

use anyhow::{anyhow, Context, Result};
use nix::pty::{openpty, Winsize};
use nix::unistd::setsid;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

use crate::services::accounts::{PasswdEntry, Users};
use crate::services::nova::{GECKO_GID, GECKO_HOME, GECKO_UID};
use crate::services::session::{current_session_user, SESSION_USER};
use crate::IPCBus;

pub const TERMINAL_IFACE: &str = "org.geckoOS.terminal";

struct TerminalSession {
    master: OwnedFd,
    child: Child,
}

pub struct TerminalService {
    sessions: RwLock<HashMap<String, TerminalSession>>,
    bus: RwLock<Option<Arc<IPCBus>>>,
    runtime: RwLock<Option<tokio::runtime::Handle>>,
}

impl TerminalService {
    fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            bus: RwLock::new(None),
            runtime: RwLock::new(None),
        }
    }

    pub fn global() -> &'static Arc<TerminalService> {
        static SERVICE: OnceLock<Arc<TerminalService>> = OnceLock::new();
        SERVICE.get_or_init(|| Arc::new(TerminalService::new()))
    }

    /// Wire the bus AND capture a tokio runtime handle — the PTY reader
    /// runs on a plain OS thread (blocking `read()`, not async), so it
    /// needs a `Handle` to hop back onto the async runtime to actually
    /// call `bus.emit_signal`, the same pattern evo's `echo_ipc.rs` uses
    /// for its own sync-context signal emission.
    pub fn set_bus(&self, bus: Arc<IPCBus>) {
        if let Ok(mut slot) = self.bus.write() {
            *slot = Some(bus);
        }
        if let Ok(mut slot) = self.runtime.write() {
            *slot = Some(tokio::runtime::Handle::current());
        }
    }

    fn bus_snapshot(&self) -> Option<Arc<IPCBus>> {
        self.bus.read().ok().and_then(|g| g.clone())
    }

    fn runtime_snapshot(&self) -> Option<tokio::runtime::Handle> {
        self.runtime.read().ok().and_then(|g| g.clone())
    }

    fn emit(&self, signal_name: &'static str, payload: Value) {
        let (Some(bus), Some(handle)) = (self.bus_snapshot(), self.runtime_snapshot()) else {
            return;
        };
        handle.spawn(async move {
            if let Err(e) = bus.emit_signal(TERMINAL_IFACE, signal_name, payload).await {
                tracing::warn!("Terminal signal '{}' failed: {}", signal_name, e);
            }
        });
    }

    /// Open a new PTY-backed shell session. Returns the new `terminalId`.
    pub fn open(&self, cols: u16, rows: u16) -> Result<String> {
        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&winsize), None).context("openpty")?;
        let slave_raw: RawFd = pty.slave.as_raw_fd();

        // Command needs three independent fds for stdin/stdout/stderr, all
        // referring to the same pty slave — dup the slave twice more so
        // each Stdio::from(OwnedFd) below owns a distinct descriptor
        // (Stdio consumes/closes its fd after dup2'ing it into the child).
        let stdin_fd = pty.slave;
        let stdout_fd = dup_owned(slave_raw)?;
        let stderr_fd = dup_owned(slave_raw)?;

        let running_as_root = unsafe { libc::geteuid() } == 0;
        let users = Users::system();
        let active_user = current_session_user();
        let plan = build_shell_launch_plan(running_as_root, &users, &active_user);

        if let Some(ref target) = plan.drop_to {
            // Grant slave TTY ownership to the non-root user before dropping credentials.
            let rc = unsafe {
                libc::fchown(
                    slave_raw,
                    target.uid as libc::uid_t,
                    target.gid as libc::gid_t,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error()).context("fchown pty slave");
            }
        }

        let mut cmd = Command::new(&plan.shell);
        cmd.arg("-i")
            .stdin(Stdio::from(stdin_fd))
            .stdout(Stdio::from(stdout_fd))
            .stderr(Stdio::from(stderr_fd));

        configure_session_env(&mut cmd, plan.drop_to.as_ref(), &plan.shell);

        let drop_ids = plan
            .drop_to
            .as_ref()
            .map(|t| (t.uid as libc::uid_t, t.gid as libc::gid_t));

        // SAFETY: setsid(), ioctl(TIOCSCTTY), setgroups(), setresgid(),
        // setresuid(), get*id(), and setuid() are all async-signal-safe and
        // only operate on this child process's credentials/session/fd state
        // between fork and exec without touching heap allocation or locks.
        unsafe {
            cmd.pre_exec(move || {
                setsid().map_err(|e| io::Error::from_raw_os_error(e as i32))?;
                if libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                if let Some((uid, gid)) = drop_ids {
                    if libc::setgroups(0, std::ptr::null()) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::setresgid(gid, gid, gid) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::setresuid(uid, uid, uid) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::geteuid() != uid
                        || libc::getuid() != uid
                        || libc::getegid() != gid
                        || libc::getgid() != gid
                    {
                        return Err(io::Error::from_raw_os_error(libc::EPERM));
                    }
                    if libc::setuid(0) == 0 {
                        return Err(io::Error::from_raw_os_error(libc::EPERM));
                    }
                }
                Ok(())
            });
        }

        // `Command::spawn()` dup2's each `Stdio::from(OwnedFd)` onto the
        // child's 0/1/2 post-fork and closes the *parent's* copy of that
        // exact fd automatically once spawn() returns — so all three
        // stdin_fd/stdout_fd/stderr_fd are already closed on our side here;
        // only the child's dup'd descriptors (kept alive across its exec)
        // still reference the pty slave.
        let child = cmd.spawn().context("spawn shell")?;

        let terminal_id = Uuid::new_v4().to_string();
        let master_raw = pty.master.as_raw_fd();

        {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| anyhow!("sessions lock poisoned"))?;
            sessions.insert(
                terminal_id.clone(),
                TerminalSession {
                    master: pty.master,
                    child,
                },
            );
        }

        self.spawn_reader(terminal_id.clone(), master_raw);
        self.emit("TerminalOpened", json!({ "terminalId": terminal_id }));
        Ok(terminal_id)
    }

    fn spawn_reader(&self, terminal_id: String, master_raw: RawFd) {
        let service = TerminalService::global().clone();
        std::thread::Builder::new()
            .name(format!("pty-reader-{terminal_id}"))
            .spawn(move || {
                let mut buf = [0u8; 4096];
                let mut pending = Vec::<u8>::with_capacity(4100);
                loop {
                    match nix::unistd::read(master_raw, &mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            pending.extend_from_slice(&buf[..n]);
                            let text = drain_utf8_prefix(&mut pending);
                            if !text.is_empty() {
                                service.emit(
                                    "TerminalOutput",
                                    json!({ "terminalId": terminal_id, "data": text }),
                                );
                            }
                        }
                        Err(_) => break,
                    }
                }
                if !pending.is_empty() {
                    let trailing = String::from_utf8_lossy(&pending).to_string();
                    service.emit(
                        "TerminalOutput",
                        json!({ "terminalId": terminal_id, "data": trailing }),
                    );
                }
                service.emit("TerminalClosed", json!({ "terminalId": terminal_id }));
                let session = service
                    .sessions
                    .write()
                    .ok()
                    .and_then(|mut sessions| sessions.remove(&terminal_id));
                // The pty closed, so the shell has exited or is about to.
                // Reap it here: nothing else waits on it.
                if let Some(mut session) = session {
                    let _ = session.child.wait();
                }
            })
            .expect("spawn pty reader thread");
    }

    /// Write keystrokes/input to the shell's stdin (the pty master).
    pub fn write(&self, terminal_id: &str, data: &str) -> Result<()> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| anyhow!("sessions lock poisoned"))?;
        let session = sessions
            .get(terminal_id)
            .ok_or_else(|| anyhow!("terminal '{}' not found", terminal_id))?;
        nix::unistd::write(session.master.as_raw_fd(), data.as_bytes())
            .context("write to pty master")?;
        Ok(())
    }

    /// Update the PTY's window size (so `$COLUMNS`/`$LINES`-aware programs,
    /// and the shell's own line-wrapping, match the actual xterm.js size).
    pub fn resize(&self, terminal_id: &str, cols: u16, rows: u16) -> Result<()> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| anyhow!("sessions lock poisoned"))?;
        let session = sessions
            .get(terminal_id)
            .ok_or_else(|| anyhow!("terminal '{}' not found", terminal_id))?;
        let winsize = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let ret = unsafe {
            libc::ioctl(session.master.as_raw_fd(), libc::TIOCSWINSZ as _, &winsize)
        };
        if ret < 0 {
            return Err(io::Error::last_os_error()).context("ioctl TIOCSWINSZ");
        }
        Ok(())
    }

    /// Terminate the shell and clean up. The reader thread notices the
    /// master fd going away (read() error) and removes the session/emits
    /// `TerminalClosed` itself, so this only needs to signal the child.
    pub fn close(&self, terminal_id: &str) -> Result<()> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| anyhow!("sessions lock poisoned"))?;
        let session = sessions
            .get_mut(terminal_id)
            .ok_or_else(|| anyhow!("terminal '{}' not found", terminal_id))?;
        let _ = session.child.kill();
        Ok(())
    }
}

struct ShellLaunchPlan {
    shell: String,
    drop_to: Option<PasswdEntry>,
}

fn default_unprivileged_user() -> PasswdEntry {
    PasswdEntry {
        name: SESSION_USER.to_string(),
        uid: GECKO_UID,
        gid: GECKO_GID,
        full_name: String::new(),
        home: GECKO_HOME.to_string(),
    }
}

/// Resolve the non-root session user account whose interactive shell should
/// run under the terminal PTY when `echo-daemon` runs as `root`.
fn resolve_terminal_user(users: &Users, session_user: &str) -> PasswdEntry {
    let candidate = users
        .require_real(session_user)
        .ok()
        .or_else(|| users.require_real(SESSION_USER).ok())
        .or_else(|| users.real_users().ok().and_then(|mut all| all.drain(..).next()))
        .unwrap_or_else(default_unprivileged_user);
    if candidate.uid == 0 || candidate.gid == 0 {
        default_unprivileged_user()
    } else {
        candidate
    }
}

fn resolve_user_shell(users: &Users, username: &str) -> String {
    if let Ok(text) = fs::read_to_string(users.passwd_path()) {
        if let Some(shell) = text.lines().find_map(|line| {
            let mut fields = line.split(':');
            if fields.next() != Some(username) {
                return None;
            }
            fields
                .nth(5)
                .map(str::trim)
                .filter(|s| {
                    s.starts_with('/')
                        && !s.ends_with("/nologin")
                        && !s.ends_with("/false")
                        && Path::new(s).is_file()
                })
                .map(str::to_string)
        }) {
            return shell;
        }
    }
    if Path::new("/bin/bash").is_file() {
        "/bin/bash".to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

fn build_shell_launch_plan(
    running_as_root: bool,
    users: &Users,
    session_user: &str,
) -> ShellLaunchPlan {
    if !running_as_root {
        return ShellLaunchPlan {
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string()),
            drop_to: None,
        };
    }
    let target = resolve_terminal_user(users, session_user);
    let shell = resolve_user_shell(users, &target.name);
    ShellLaunchPlan {
        shell,
        drop_to: Some(target),
    }
}

/// PATH for every interactive shell the daemon opens.
const SESSION_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Build the shell's environment from nothing: the daemon's own (root's, under
/// systemd) is never inherited. Only what a shell needs is set.
///
/// With a drop (the root daemon, the normal case), `configure_unprivileged_env`
/// supplies HOME, USER, LOGNAME, SHELL and XDG_RUNTIME_DIR for the target user.
/// Without one (a daemon run as an ordinary user), the same variables are
/// carried over from the daemon, since that user is the session's user.
fn configure_session_env(cmd: &mut Command, target: Option<&PasswdEntry>, shell: &str) {
    cmd.env_clear()
        .env("TERM", "xterm-256color")
        .env("PATH", SESSION_PATH);
    if let Some(lang) = std::env::var_os("LANG") {
        cmd.env("LANG", lang);
    }
    match target {
        Some(target) => configure_unprivileged_env(cmd, target, shell),
        None => {
            cmd.env("SHELL", shell)
                .env("HOME", std::env::var_os("HOME").unwrap_or_else(|| "/".into()));
            for key in ["USER", "LOGNAME", "XDG_RUNTIME_DIR"] {
                if let Some(value) = std::env::var_os(key) {
                    cmd.env(key, value);
                }
            }
        }
    }
}

fn configure_unprivileged_env(cmd: &mut Command, target: &PasswdEntry, shell: &str) {
    cmd.env("HOME", &target.home)
        .env("USER", &target.name)
        .env("LOGNAME", &target.name)
        .env("SHELL", shell);
    let target_runtime_dir = format!("/run/user/{}", target.uid);
    if Path::new(&target_runtime_dir).is_dir() {
        cmd.env("XDG_RUNTIME_DIR", target_runtime_dir);
    } else {
        cmd.env_remove("XDG_RUNTIME_DIR");
    }
    if Path::new(&target.home).is_dir() {
        cmd.current_dir(&target.home);
    }
}

fn dup_owned(fd: RawFd) -> Result<OwnedFd> {
    // Plain libc::dup (already a workspace dep) rather than pulling in
    // nix's separate "fs" feature just for this one call.
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        return Err(io::Error::last_os_error()).context("dup pty slave fd");
    }
    Ok(unsafe { OwnedFd::from_raw_fd(dup) })
}

/// Drain decodable UTF-8 from `pending`, retaining at most 3 trailing bytes
/// when the buffer ends mid-codepoint so the next `read()` completes it.
fn drain_utf8_prefix(pending: &mut Vec<u8>) -> String {
    let mut out = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(valid) => {
                out.push_str(valid);
                pending.clear();
                break;
            }
            Err(err) => {
                let valid_up_to = err.valid_up_to();
                if valid_up_to > 0 {
                    // SAFETY: `from_utf8` verified `..valid_up_to` is valid UTF-8.
                    let valid = unsafe { std::str::from_utf8_unchecked(&pending[..valid_up_to]) };
                    out.push_str(valid);
                    pending.drain(..valid_up_to);
                }
                match err.error_len() {
                    None => {
                        // Incomplete multi-byte sequence at the end of the buffer;
                        // retain the trailing bytes (1..=3) for the next read().
                        break;
                    }
                    Some(invalid_len) => {
                        out.push('\u{FFFD}');
                        pending.drain(..invalid_len);
                    }
                }
            }
        }
    }
    out
}

// ---------- JSON helpers consumed by the daemon dispatcher ----------

impl TerminalService {
    pub fn open_json(&self, cols: u16, rows: u16) -> Result<Value> {
        let id = self.open(cols, rows)?;
        Ok(json!({ "terminalId": id }))
    }

    pub fn write_json(&self, terminal_id: &str, data: &str) -> Result<Value> {
        self.write(terminal_id, data)?;
        Ok(json!({ "success": true }))
    }

    pub fn resize_json(&self, terminal_id: &str, cols: u16, rows: u16) -> Result<Value> {
        self.resize(terminal_id, cols, rows)?;
        Ok(json!({ "success": true }))
    }

    pub fn close_json(&self, terminal_id: &str) -> Result<Value> {
        self.close(terminal_id)?;
        Ok(json!({ "success": true }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises the real syscall path end to end (openpty, setsid +
    /// TIOCSCTTY in `pre_exec`, spawning an actual shell, writing to its
    /// stdin, resizing, killing it) with no bus wired up — `emit()` no-ops
    /// safely without one, so this validates the PTY mechanics themselves
    /// without needing a live Echo signal subscriber. A real regression
    /// here (wrong ioctl request constant, wrong pre_exec ordering causing
    /// the child to hang instead of exiting, etc.) would fail this test
    /// with a real `Err` surfaced through `?`, not a guess.
    #[tokio::test]
    async fn opens_writes_resizes_and_closes_a_real_shell() {
        let service = TerminalService::new();
        let id = service.open(80, 24).expect("open real pty + shell");
        assert!(!id.is_empty());

        // Give the shell a moment to actually start before writing to it.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        service
            .write(&id, "echo PTYMARKER_OK\n")
            .expect("write to real pty master");
        service.resize(&id, 100, 30).expect("resize real pty");

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        service.close(&id).expect("close real session");
    }

    #[test]
    fn reassembles_multibyte_utf8_split_across_reads() {
        let mut pending = Vec::new();
        // Split 3-byte box-drawing char "─" ([0xE2, 0x94, 0x80]) and 4-byte "🦎" ([0xF0, 0x9F, 0xA6, 0x8E]).
        pending.extend_from_slice(b"hi\xE2\x94");
        assert_eq!(drain_utf8_prefix(&mut pending), "hi");
        assert_eq!(pending, vec![0xE2, 0x94]);

        pending.extend_from_slice(b"\x80\xF0\x9F");
        assert_eq!(drain_utf8_prefix(&mut pending), "─");
        assert_eq!(pending, vec![0xF0, 0x9F]);

        pending.extend_from_slice(b"\xA6\x8E!");
        assert_eq!(drain_utf8_prefix(&mut pending), "🦎!");
        assert!(pending.is_empty());

        // Genuinely invalid byte between valid ASCII emits U+FFFD without dropping later data.
        pending.extend_from_slice(b"a\xFFb");
        assert_eq!(drain_utf8_prefix(&mut pending), "a\u{FFFD}b");
        assert!(pending.is_empty());
    }

    #[test]
    fn root_launch_plan_drops_to_active_non_root_user_with_shell_env() {
        let dir = tempfile::TempDir::new().unwrap();
        let home_alice = dir.path().join("home/alice");
        fs::create_dir_all(&home_alice).unwrap();
        fs::create_dir_all(dir.path().join("etc")).unwrap();
        fs::write(
            dir.path().join("etc/passwd"),
            format!(
                "root:x:0:0:root:/root:/bin/bash\n\
                 geckouser:x:1000:1000:Gecko:/home/geckouser:/bin/bash\n\
                 pyxisapp:x:1001:1001:Apps:/home/pyxisapp:/usr/sbin/nologin\n\
                 alice:x:1002:1002:Alice:{}:/bin/sh\n",
                home_alice.display()
            ),
        )
        .unwrap();

        let users = Users::with_root(dir.path());

        // Active secondary user is selected when present.
        let plan_alice = build_shell_launch_plan(true, &users, "alice");
        let drop_alice = plan_alice.drop_to.expect("root must drop privileges");
        assert_eq!(drop_alice.name, "alice");
        assert_eq!(drop_alice.uid, 1002);
        assert_eq!(drop_alice.gid, 1002);
        assert_eq!(drop_alice.home, home_alice.to_string_lossy());
        assert_eq!(plan_alice.shell, "/bin/sh");

        // Unknown/root session user falls back safely to geckouser (uid 1000), never root/pyxisapp.
        let plan_fallback = build_shell_launch_plan(true, &users, "root");
        let drop_fallback = plan_fallback.drop_to.expect("root fallback must drop privileges");
        assert_eq!(drop_fallback.name, "geckouser");
        assert_eq!(drop_fallback.uid, 1000);
        assert_eq!(drop_fallback.gid, 1000);
        assert_eq!(drop_fallback.home, "/home/geckouser");

        // Verify env propagation on the constructed Command.
        let mut cmd = Command::new(&plan_alice.shell);
        configure_session_env(&mut cmd, Some(&drop_alice), &plan_alice.shell);
        let envs: HashMap<_, _> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .collect();
        assert_eq!(envs.get("USER").map(String::as_str), Some("alice"));
        assert_eq!(envs.get("LOGNAME").map(String::as_str), Some("alice"));
        assert_eq!(envs.get("HOME").map(String::as_str), Some(drop_alice.home.as_str()));
        assert_eq!(envs.get("SHELL").map(String::as_str), Some("/bin/sh"));

        // Non-root daemon test mode leaves drop_to = None cleanly.
        let plan_non_root = build_shell_launch_plan(false, &users, "alice");
        assert!(plan_non_root.drop_to.is_none());
    }

    #[test]
    fn session_env_drops_inherited_variables() {
        // A variable set on the Command before the env is built stands in for
        // one the daemon might carry; it must not reach the shell.
        let mut cmd = Command::new("/bin/sh");
        cmd.env("ECHO_LEAK_CANARY", "root-secret");
        configure_session_env(&mut cmd, None, "/bin/sh");
        let envs: HashMap<String, Option<String>> = cmd
            .get_envs()
            .map(|(k, v)| {
                (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))
            })
            .collect();
        assert!(!envs.contains_key("ECHO_LEAK_CANARY"));
        assert_eq!(envs.get("PATH").cloned().flatten().as_deref(), Some(SESSION_PATH));
        assert_eq!(envs.get("TERM").cloned().flatten().as_deref(), Some("xterm-256color"));
        assert_eq!(envs.get("SHELL").cloned().flatten().as_deref(), Some("/bin/sh"));
        assert!(envs.get("HOME").cloned().flatten().is_some());
    }

    #[test]
    fn session_env_for_a_dropped_user_is_built_from_nothing_too() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = dir.path().join("home/bob");
        fs::create_dir_all(&home).unwrap();
        let target = PasswdEntry {
            name: "bob".into(),
            uid: 1234,
            gid: 1234,
            full_name: String::new(),
            home: home.to_string_lossy().into_owned(),
        };
        let mut cmd = Command::new("/bin/sh");
        cmd.env("ECHO_LEAK_CANARY", "root-secret");
        configure_session_env(&mut cmd, Some(&target), "/bin/sh");
        let envs: HashMap<String, Option<String>> = cmd
            .get_envs()
            .map(|(k, v)| {
                (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))
            })
            .collect();
        assert!(!envs.contains_key("ECHO_LEAK_CANARY"));
        assert_eq!(envs.get("HOME").cloned().flatten().as_deref(), Some(target.home.as_str()));
        assert_eq!(envs.get("USER").cloned().flatten().as_deref(), Some("bob"));
        assert_eq!(envs.get("PATH").cloned().flatten().as_deref(), Some(SESSION_PATH));
    }
}
