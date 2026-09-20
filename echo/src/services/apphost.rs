//! App Host Service
//!
//! The App Host owns the lifecycle of installed GeckoOS applications:
//!
//!   - manifest discovery under `/usr/share/geckoOS/apps/<id>/manifest.json`
//!   - launch / close / kill / restart with PID + session tracking
//!   - lifecycle signal emission (`AppStarting`, `AppRunning`, `AppStopped`,
//!     `AppCrashed`) on the `org.geckoOS.appHost` interface
//!
//! This is the post-`AppManagerService` layer in the systems stack. The legacy
//! `AppManagerService` now delegates here so the existing `appManager.*`
//! IPC surface keeps working for older clients while shells (Pyxis) move to
//! the `appHost.*` interface.
//!
//! Wayland surface binding is real: `bind_surface`/`unbind_surface` (and
//! their `BindSurface`/`UnbindSurface` IPC wrappers) track compositor
//! surfaces per session, driven by evo's Echo connection
//! (`evo/src/enhancements/echo_ipc.rs`) as toplevels are created/destroyed.
//!
//! Intentionally NOT in this MVP (tracked as TODOs):
//!   - sandboxing / cgroups / namespaces / portals
//!   - permissions enforcement
//!   - tarball signature verification
//!
//! Crash detection: a single tokio task started in `start_child_reaper` listens
//! for SIGCHLD and calls `waitpid(-1, WNOHANG)` in a loop, mapping reaped pids
//! back to sessions. Non-zero exit / signal termination -> `AppCrashed`; clean
//! exit (or pre-recorded `Stopped` from CloseApp/KillApp) -> `AppStopped`.

use anyhow::{anyhow, Context, Result};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::SystemTime;

use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use uuid::Uuid;

use crate::IPCBus;

const APPS_ROOT: &str = "/usr/share/geckoOS/apps";
/// User-installed (writable) app root, checked after `APPS_ROOT` for both
/// manifest reads and directory scans, so apps Nova installs under
/// `/var/lib/pyxis/apps/<id>/` (see `services::nova`) are launchable
/// through this unchanged `AppHost::launch` path — "system-first-then-user"
/// resolution, per `nova-runtime-host-plan.md` §4.
const USER_APPS_ROOT: &str = "/var/lib/pyxis/apps";
/// Nova-derived manifests for opkg/ipk-installed apps (appinfo.json under
/// /usr/pyxis/applications — see `services::nova::NOVA_NATIVE_ROOT`'s doc
/// comment for why those manifests can't live in the app dir itself).
/// Checked last: system, then user, then ipk-derived.
const NOVA_NATIVE_ROOT: &str = "/var/lib/pyxis/nova/native";

/// Interface name for app host IPC + signals.
pub const APPHOST_IFACE: &str = "org.geckoOS.appHost";

// ---------- Public types ----------

/// Manifest describing a single installed app.
///
/// New optional fields (`category`, `entry`, `env`, `args`) carry forward the
/// existing schema (`id`, `name`, `exec`, `icon`, `version`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppManifest {
    pub id: String,
    pub name: String,
    pub exec: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Optional shell-side entry hint (e.g. component identifier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
}

/// Lifecycle state of a running session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppState {
    Starting,
    Running,
    /// Frozen via `SIGSTOP` — the webOS-style "card backgrounded" state
    /// (see `suspend`/`resume`), driven by Nova reacting to Evo's
    /// `WindowMinimized` signal. The process is alive but not scheduled;
    /// distinct from `Stopped` (process gone, session draining).
    Suspended,
    Stopped,
    Crashed,
}

impl AppState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AppState::Starting => "starting",
            AppState::Running => "running",
            AppState::Suspended => "suspended",
            AppState::Stopped => "stopped",
            AppState::Crashed => "crashed",
        }
    }
}

/// One launched instance of an app.
#[derive(Debug, Clone, Serialize)]
pub struct AppSession {
    pub session_id: String,
    pub app_id: String,
    pub pid: u32,
    /// Seconds since UNIX_EPOCH.
    pub started_at: u64,
    pub state: AppState,
    /// Compositor surfaces (Wayland window ids in evo) currently bound to
    /// this session. A session may have 0..N surfaces; 0 means the process
    /// is running but has not produced a window yet (e.g. headless tools or
    /// not-yet-mapped startup).
    #[serde(default)]
    pub surfaces: Vec<u32>,
}

impl AppSession {
    fn to_json(&self) -> Value {
        json!({
            "sessionId": self.session_id,
            "appId": self.app_id,
            "pid": self.pid,
            "startedAt": self.started_at,
            "state": self.state.as_str(),
            "surfaces": self.surfaces,
        })
    }
}

// ---------- AppHost ----------

/// Lifecycle-aware host for installed apps. Process-global; clone the `Arc`
/// to share between the daemon's handler closures.
pub struct AppHost {
    /// Keyed by `session_id`.
    sessions: DashMap<String, AppSession>,
    /// Foregrounded session id, if any. Stores `session_id`, not `app_id`,
    /// so multiple sessions of the same app can coexist later.
    active: RwLock<Option<String>>,
    /// Optional bus for signal emission. When `None` (e.g. tests), lifecycle
    /// transitions still happen but no signal is broadcast.
    bus: RwLock<Option<Arc<IPCBus>>>,
    /// pid -> session_id, populated on spawn, drained by the reaper or by
    /// CloseApp/KillApp. Used to map waitpid() results back to sessions.
    pid_index: RwLock<HashMap<u32, String>>,
    /// Sessions whose stop was user-initiated (CloseApp/KillApp). The reaper
    /// uses this to decide between `Stopped` and `Crashed` when the process
    /// finally exits, regardless of exit code.
    expected_stops: RwLock<std::collections::HashSet<String>>,
}

impl AppHost {
    fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            active: RwLock::new(None),
            bus: RwLock::new(None),
            pid_index: RwLock::new(HashMap::new()),
            expected_stops: RwLock::new(std::collections::HashSet::new()),
        }
    }

    /// Process-global handle — called by both the daemon's app host handlers
    /// and the legacy `AppManagerService` shim.
    pub fn global() -> &'static Arc<AppHost> {
        static HOST: OnceLock<Arc<AppHost>> = OnceLock::new();
        HOST.get_or_init(|| Arc::new(AppHost::new()))
    }

    /// Wire the IPC bus so lifecycle transitions emit signals. Idempotent;
    /// safe to call multiple times.
    pub fn set_bus(&self, bus: Arc<IPCBus>) {
        if let Ok(mut slot) = self.bus.write() {
            *slot = Some(bus);
        }
    }

    fn bus_snapshot(&self) -> Option<Arc<IPCBus>> {
        self.bus.read().ok().and_then(|g| g.clone())
    }

    // ---------- Manifest discovery ----------

    /// Locate `<root>/<app_id>` across both the system and user app roots,
    /// system first. Returns the containing root's app dir, not the
    /// manifest file itself.
    fn find_app_dir(app_id: &str) -> Option<PathBuf> {
        for root in [
            Self::apps_root(),
            Self::user_apps_root(),
            Self::nova_native_root(),
        ] {
            let dir = root.join(app_id);
            if dir.join("manifest.json").is_file() {
                return Some(dir);
            }
        }
        None
    }

    /// Read and validate a single app manifest by id.
    pub fn read_manifest(app_id: &str) -> Result<AppManifest> {
        let app_dir = Self::find_app_dir(app_id)
            .ok_or_else(|| anyhow!("no manifest.json found for app '{}'", app_id))?;
        let path = app_dir.join("manifest.json");
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("read manifest at {}", path.display()))?;
        let manifest: AppManifest = serde_json::from_str(&raw)
            .with_context(|| format!("parse manifest at {}", path.display()))?;
        if manifest.id != app_id {
            return Err(anyhow!(
                "manifest id mismatch: dir '{}' vs manifest '{}'",
                app_id,
                manifest.id
            ));
        }
        if manifest.exec.trim().is_empty() {
            return Err(anyhow!("manifest '{}' has empty exec", app_id));
        }
        Ok(manifest)
    }

    /// Scan both `/usr/share/geckoOS/apps/*/manifest.json` (system) and
    /// `/var/lib/pyxis/apps/*/manifest.json` (user-installed, e.g. by
    /// Nova) and return all valid manifests, user entries overriding system
    /// ones on id collision. Invalid/missing manifests are silently
    /// skipped — the caller gets the working subset.
    pub fn list_apps(&self) -> Result<Vec<AppManifest>> {
        let mut by_id: HashMap<String, AppManifest> = HashMap::new();
        for root in [
            Self::apps_root(),
            Self::user_apps_root(),
            Self::nova_native_root(),
        ] {
            if !root.exists() {
                continue;
            }
            let entries = match fs::read_dir(&root) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let p = entry.path();
                if !p.is_dir() {
                    continue;
                }
                let id = match p.file_name().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                if let Ok(m) = Self::read_manifest(&id) {
                    by_id.insert(id, m);
                }
            }
        }
        // Stable ordering across calls — easier to consume in shells/tests.
        let mut out: Vec<AppManifest> = by_id.into_values().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    // ---------- Lifecycle ----------

    /// Launch a new session for `app_id`. Always spawns; callers that want
    /// "raise existing" semantics should consult `find_session_by_app` first.
    pub fn launch(&self, app_id: &str, params: Option<Value>) -> Result<AppSession> {
        let manifest = Self::read_manifest(app_id)?;
        let app_dir = Self::find_app_dir(&manifest.id)
            .ok_or_else(|| anyhow!("no manifest.json found for app '{}'", manifest.id))?;

        // Resolve exec relative to the app directory when given as `./...`.
        let exec_path: PathBuf = if manifest.exec.starts_with("./") {
            app_dir.join(&manifest.exec[2..])
        } else if Path::new(&manifest.exec).is_absolute() {
            PathBuf::from(&manifest.exec)
        } else {
            app_dir.join(&manifest.exec)
        };

        let mut cmd = Command::new(&exec_path);
        cmd.current_dir(&app_dir);
        if let Some(args) = manifest.args.as_ref() {
            cmd.args(args);
        }
        if let Some(env) = manifest.env.as_ref() {
            for (k, v) in env {
                cmd.env(k, v);
            }
        }
        // `params` is forwarded as JSON in GECKOOS_LAUNCH_PARAMS so apps that
        // care can read it; most placeholders ignore it.
        if let Some(p) = params.as_ref() {
            cmd.env("GECKOOS_LAUNCH_PARAMS", p.to_string());
        }

        self.spawn_and_track(app_id, cmd)
    }

    /// Spawn an already-installed app with no `manifest.json` involved —
    /// for a process whose command line is composed ad hoc by the caller
    /// (e.g. Nova's external-browser launch: a real `vela <url>` window
    /// with no fixed app registry entry to read `exec`/`args`/`env` from).
    /// Tracked as a real session (PID, lifecycle signals, surface binding)
    /// exactly like `launch`, via the same `spawn_and_track` this delegates
    /// to — just skips the manifest-file read `launch` otherwise requires.
    pub fn launch_raw(
        &self,
        app_id: &str,
        exec_path: &Path,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<AppSession> {
        let mut cmd = Command::new(exec_path);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        self.spawn_and_track(app_id, cmd)
    }

    /// Shared by `launch` and `launch_raw`: spawn `cmd`, register it as a
    /// new session for `app_id`, and emit the usual Starting/Running/
    /// Crashed lifecycle signals.
    fn spawn_and_track(&self, app_id: &str, mut cmd: Command) -> Result<AppSession> {
        let session_id = Uuid::new_v4().to_string();
        let started_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Provisional Starting record so `list_running` reflects the launch
        // even if spawn() blocks briefly. We overwrite or remove it below.
        let starting = AppSession {
            session_id: session_id.clone(),
            app_id: app_id.to_string(),
            pid: 0,
            started_at,
            state: AppState::Starting,
            surfaces: Vec::new(),
        };
        self.sessions.insert(session_id.clone(), starting.clone());
        self.emit_lifecycle("AppStarting", &starting);

        // Put every app in its OWN process group, so it can later be signalled
        // as a group rather than as a single pid.
        //
        // Signalling one pid is not enough for vela: Gecko re-execs itself in
        // some startup paths, and the replacement process has a DIFFERENT pid
        // (observed on-device with its argv stripped bare — `/usr/lib/
        // pyxis-gecko/vela` with no arguments, but still carrying
        // PYXIS_APP_ID=files in its environment). `close()` then SIGTERMs the
        // pid AppHost recorded, which by that point is gone, the signal lands
        // nowhere, and the real process survives — reparented to init.
        //
        // The leak is user-visible: that orphan owns a window, and because it
        // lost its `--pyxis-app` argv it renders as a plain Firefox window with
        // the omnibar. Launch an app, close it, launch it again, and you get
        // the orphan's chrome window instead of a clean kiosk one — the
        // "subsequent launches don't respect kiosk mode" report.
        //
        // `process_group(0)` makes the child a process-group leader with pgid
        // == its own pid, so a re-exec'd descendant stays in the group and
        // `kill(-pgid)` reaches it. Same reasoning as the `setsid` fix in
        // create-rootfs.sh's start_daemon, one layer down.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                // Remove provisional row, surface a Crashed signal so
                // shells can show a launch-failure badge.
                let mut crashed = starting.clone();
                crashed.state = AppState::Crashed;
                self.sessions.insert(session_id.clone(), crashed.clone());
                self.emit_lifecycle("AppCrashed", &crashed);
                self.sessions.remove(&session_id);
                return Err(anyhow!(
                    "spawn '{}' for app '{}' failed: {}",
                    cmd.get_program().to_string_lossy(),
                    app_id,
                    e
                ));
            }
        };

        let pid = child.id();
        // We hand the OS the responsibility of reaping; the SIGCHLD task
        // (see `start_child_reaper`) will pick up exits via waitpid(WNOHANG).
        // The Child handle is forgotten so its Drop doesn't try to wait().
        std::mem::forget(child);
        // Track pid -> session_id so the reaper can look it up O(1).
        if let Ok(mut idx) = self.pid_index.write() {
            idx.insert(pid, session_id.clone());
        }

        let running = AppSession {
            session_id: session_id.clone(),
            app_id: app_id.to_string(),
            pid,
            started_at,
            state: AppState::Running,
            surfaces: Vec::new(),
        };
        self.sessions.insert(session_id.clone(), running.clone());
        self.emit_lifecycle("AppRunning", &running);

        // New session is foregrounded.
        if let Ok(mut slot) = self.active.write() {
            *slot = Some(session_id.clone());
        }

        Ok(running)
    }

    /// Find the most recent session for `app_id`, if any.
    pub fn find_session_by_app(&self, app_id: &str) -> Option<AppSession> {
        let mut latest: Option<AppSession> = None;
        for s in self.sessions.iter() {
            if s.value().app_id == app_id {
                let candidate = s.value().clone();
                latest = match latest {
                    Some(prev) if prev.started_at >= candidate.started_at => Some(prev),
                    _ => Some(candidate),
                };
            }
        }
        latest
    }

    /// SIGTERM a session; updates state to `Stopped`. The session stays in
    /// the map until the SIGCHLD reaper observes the process exit, so
    /// `list_running` accurately shows draining sessions.
    pub fn close(&self, session_id: &str) -> Result<AppSession> {
        let session = self
            .sessions
            .get(session_id)
            .map(|s| s.clone())
            .ok_or_else(|| anyhow!("session '{}' not found", session_id))?;
        if let Ok(mut set) = self.expected_stops.write() {
            set.insert(session_id.to_string());
        }
        if session.pid > 0 {
            // Signal the whole group, then escalate. A single SIGTERM to one
            // pid used to leave an orphaned vela owning a window (see
            // `signal_session`); escalation guarantees the app is actually gone
            // rather than merely asked to leave.
            Self::signal_session(session.pid, Signal::SIGTERM);

            let pid = session.pid;
            let sid = session_id.to_string();
            std::thread::spawn(move || {
                // Give it a beat to shut down cleanly, then insist.
                for _ in 0..20 {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    if Self::session_gone(pid) {
                        return;
                    }
                }
                tracing::warn!(
                    "session {} (pid {}) ignored SIGTERM after 5s; sending SIGKILL",
                    sid,
                    pid
                );
                Self::signal_session(pid, Signal::SIGKILL);
            });
        } else {
            // Foreign-synthesized or pre-spawn sessions never had a child;
            // finalize immediately since SIGCHLD won't fire.
            self.finalize_exit(session_id, AppState::Stopped);
        }
        let stopped = self.transition(session_id, AppState::Stopped);
        if let Ok(mut slot) = self.active.write() {
            if slot.as_deref() == Some(session_id) {
                *slot = None;
            }
        }
        Ok(stopped.unwrap_or(session))
    }

    /// SIGKILL a session.
    pub fn kill(&self, session_id: &str) -> Result<AppSession> {
        let session = self
            .sessions
            .get(session_id)
            .map(|s| s.clone())
            .ok_or_else(|| anyhow!("session '{}' not found", session_id))?;
        if let Ok(mut set) = self.expected_stops.write() {
            set.insert(session_id.to_string());
        }
        if session.pid > 0 {
            // Group-wide, same reasoning as `close`: a re-exec'd vela has a
            // different pid and would otherwise survive a kill aimed at the
            // recorded one.
            Self::signal_session(session.pid, Signal::SIGKILL);
        } else {
            self.finalize_exit(session_id, AppState::Crashed);
        }
        let crashed = self.transition(session_id, AppState::Crashed);
        if let Ok(mut slot) = self.active.write() {
            if slot.as_deref() == Some(session_id) {
                *slot = None;
            }
        }
        Ok(crashed.unwrap_or(session))
    }

    /// Freeze a running session with `SIGSTOP` — the webOS "card
    /// backgrounded" freeze/thaw model ported from `gaia`'s
    /// `suspend_app`/`resume_app` (which only flipped an in-memory status
    /// flag with no real process effect) into a genuine OS-level pause:
    /// `SIGSTOP` halts all of the process's threads until `SIGCONT`, which
    /// for a single-process Gecko instance (see `user.js`'s
    /// `dom.ipc.processCount = 1` in `scripts/create-rootfs.sh`) means its
    /// entire event loop, JS execution, and rendering genuinely stop
    /// consuming CPU while backgrounded — real behavior, not a status label.
    pub fn suspend(&self, session_id: &str) -> Result<AppSession> {
        let session = self
            .sessions
            .get(session_id)
            .map(|s| s.clone())
            .ok_or_else(|| anyhow!("session '{}' not found", session_id))?;
        if session.state != AppState::Running {
            return Err(anyhow!(
                "cannot suspend session '{}' in state '{}'",
                session_id,
                session.state.as_str()
            ));
        }
        if session.pid > 0 {
            Self::send_signal(session.pid, Signal::SIGSTOP)?;
        }
        let suspended = self.transition(session_id, AppState::Suspended);
        Ok(suspended.unwrap_or(session))
    }

    /// Thaw a suspended session with `SIGCONT`.
    pub fn resume(&self, session_id: &str) -> Result<AppSession> {
        let session = self
            .sessions
            .get(session_id)
            .map(|s| s.clone())
            .ok_or_else(|| anyhow!("session '{}' not found", session_id))?;
        if session.state != AppState::Suspended {
            return Err(anyhow!(
                "cannot resume session '{}' in state '{}'",
                session_id,
                session.state.as_str()
            ));
        }
        if session.pid > 0 {
            Self::send_signal(session.pid, Signal::SIGCONT)?;
        }
        let resumed = self.transition(session_id, AppState::Running);
        Ok(resumed.unwrap_or(session))
    }

    /// Close + relaunch.
    pub fn restart(&self, session_id: &str) -> Result<AppSession> {
        let app_id = self
            .sessions
            .get(session_id)
            .map(|s| s.app_id.clone())
            .ok_or_else(|| anyhow!("session '{}' not found", session_id))?;
        let _ = self.close(session_id);
        std::thread::sleep(std::time::Duration::from_millis(150));
        self.launch(&app_id, None)
    }

    /// Snapshot of all current sessions (excludes ones already removed).
    pub fn list_running(&self) -> Vec<AppSession> {
        let mut out: Vec<AppSession> =
            self.sessions.iter().map(|s| s.value().clone()).collect();
        out.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        out
    }

    /// Returns the live state of a session, refreshing `Crashed` if the PID
    /// has died since the last touch.
    pub fn get_state(&self, session_id: &str) -> Option<AppState> {
        let snapshot = self.sessions.get(session_id).map(|s| s.clone())?;
        if snapshot.state == AppState::Running && snapshot.pid > 0 && !Self::pid_alive(snapshot.pid)
        {
            // Process died unexpectedly — flip to Crashed and emit.
            let crashed = self.transition(session_id, AppState::Crashed);
            return crashed.map(|s| s.state).or(Some(AppState::Crashed));
        }
        Some(snapshot.state)
    }

    /// Get the active (foregrounded) session id.
    pub fn active_session(&self) -> Option<String> {
        self.active.read().ok().and_then(|g| g.clone())
    }

    pub fn set_active(&self, session_id: &str) -> Result<()> {
        if !self.sessions.contains_key(session_id) {
            return Err(anyhow!("session '{}' not found", session_id));
        }
        let mut slot = self
            .active
            .write()
            .map_err(|_| anyhow!("active lock poisoned"))?;
        *slot = Some(session_id.to_string());
        Ok(())
    }

    // ---------- Child reaping ----------

    /// Look up a session by pid (used by the SIGCHLD reaper).
    fn session_id_for_pid(&self, pid: u32) -> Option<String> {
        self.pid_index.read().ok()?.get(&pid).cloned()
    }

    /// Apply terminal lifecycle bookkeeping for `session_id`: emit the
    /// matching signal, drop pid_index + expected_stops entries, drop the
    /// session from the live map, and clear `active` if necessary.
    fn finalize_exit(&self, session_id: &str, terminal: AppState) {
        if let Some(mut s) = self.sessions.get_mut(session_id) {
            s.state = terminal;
        }
        let snapshot = self.sessions.get(session_id).map(|s| s.clone());
        if let Some(s) = snapshot.as_ref() {
            let signal = match terminal {
                AppState::Stopped => "AppStopped",
                AppState::Crashed => "AppCrashed",
                _ => return,
            };
            self.emit_lifecycle(signal, s);
            // Drop pid_index entry
            if let Ok(mut idx) = self.pid_index.write() {
                if s.pid > 0 {
                    idx.remove(&s.pid);
                }
            }
        }
        if let Ok(mut set) = self.expected_stops.write() {
            set.remove(session_id);
        }
        self.sessions.remove(session_id);
        if let Ok(mut slot) = self.active.write() {
            if slot.as_deref() == Some(session_id) {
                *slot = None;
            }
        }
    }

    /// Reap any zombie children non-blocking. Called from the SIGCHLD task
    /// (and exposed for tests). Returns the number of children reaped.
    pub fn reap_once(&self) -> usize {
        let mut reaped = 0usize;
        loop {
            match waitpid(Some(Pid::from_raw(-1)), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => break,
                Ok(WaitStatus::Exited(pid, code)) => {
                    let pid = pid.as_raw() as u32;
                    self.handle_reaped(pid, code == 0);
                    reaped += 1;
                }
                Ok(WaitStatus::Signaled(pid, _, _)) => {
                    let pid = pid.as_raw() as u32;
                    // Treat signal-termination as crash unless the user
                    // initiated it (CloseApp/KillApp set expected_stops).
                    self.handle_reaped(pid, false);
                    reaped += 1;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        reaped
    }

    fn handle_reaped(&self, pid: u32, clean_exit: bool) {
        let Some(session_id) = self.session_id_for_pid(pid) else {
            return;
        };
        let was_expected = self
            .expected_stops
            .read()
            .map(|s| s.contains(&session_id))
            .unwrap_or(false);
        let terminal = if was_expected || clean_exit {
            AppState::Stopped
        } else {
            AppState::Crashed
        };
        self.finalize_exit(&session_id, terminal);
    }

    /// Spawn the singleton SIGCHLD watcher. Idempotent: only the first call
    /// installs the listener. Safe to call without a tokio runtime; in that
    /// case it silently no-ops (tests rely on `reap_once` directly).
    pub fn start_child_reaper(self: &Arc<Self>) {
        static STARTED: OnceLock<()> = OnceLock::new();
        if STARTED.set(()).is_err() {
            return;
        }
        let host = Arc::clone(self);
        // If we're not in a tokio runtime, fall back to a std thread that
        // polls every 250ms. Compositor + daemon both run inside tokio, so
        // the fast path is the signal listener.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let Ok(mut sigchld) = tokio::signal::unix::signal(
                    tokio::signal::unix::SignalKind::child(),
                ) else {
                    return;
                };
                // Drain any pre-existing zombies (e.g. children that exited
                // between spawn and reaper start).
                host.reap_once();
                while sigchld.recv().await.is_some() {
                    host.reap_once();
                }
            });
        } else {
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                host.reap_once();
            });
        }
    }

    // ---------- Compositor surface binding ----------
    //
    // Evo (the GeckoOS Smithay compositor) holds the authoritative window
    // registry: every mapped XDG toplevel gets a numeric `window_id` and an
    // `app_id` lifted from the toplevel's `XdgToplevelSurfaceData`. We mirror
    // that mapping here so any consumer of `AppHost` (the shell, the dock,
    // future portals) can answer "which surfaces does this session own?"
    // without having to know about Wayland.
    //
    // Bind/unbind only mutates the session record and emits a lightweight
    // signal — it never spawns or kills a process. That keeps these calls
    // safe to issue from the compositor's hot path.

    /// Attach a compositor surface (Wayland window id) to a session.
    /// If `session_id` is `None`, the most recent session for `app_id` is
    /// used; if no session exists yet, a synthetic one is created in the
    /// `Running` state so external windows (e.g. system X11 clients started
    /// outside the host) can still appear in the registry.
    ///
    /// Returns the (possibly synthesized) session.
    pub fn bind_surface(
        &self,
        app_id: &str,
        surface_id: u32,
        session_id: Option<&str>,
        pid: Option<u32>,
    ) -> AppSession {
        let target_id = session_id
            .map(|s| s.to_string())
            .or_else(|| self.find_session_by_app(app_id).map(|s| s.session_id))
            .unwrap_or_else(|| {
                // Synthesize a foreign-process session so the compositor can
                // still surface the window in the lifecycle stream.
                let session_id = Uuid::new_v4().to_string();
                let started_at = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let synth = AppSession {
                    session_id: session_id.clone(),
                    app_id: app_id.to_string(),
                    pid: pid.unwrap_or(0),
                    started_at,
                    state: AppState::Running,
                    surfaces: Vec::new(),
                };
                self.sessions.insert(session_id.clone(), synth.clone());
                self.emit_lifecycle("AppRunning", &synth);
                session_id
            });

        let mut updated: Option<AppSession> = None;
        if let Some(mut entry) = self.sessions.get_mut(&target_id) {
            if !entry.surfaces.contains(&surface_id) {
                entry.surfaces.push(surface_id);
            }
            if let Some(p) = pid {
                if entry.pid == 0 {
                    entry.pid = p;
                }
            }
            updated = Some(entry.clone());
        }

        let session = updated.unwrap_or_else(|| AppSession {
            session_id: target_id.clone(),
            app_id: app_id.to_string(),
            pid: pid.unwrap_or(0),
            started_at: 0,
            state: AppState::Running,
            surfaces: vec![surface_id],
        });
        self.emit_lifecycle("AppSurfaceMapped", &session);
        session
    }

    /// Detach a surface from whichever session currently owns it. If the
    /// session was synthesized for a foreign-process window and now has no
    /// remaining surfaces, it is dropped to keep the registry tidy.
    pub fn unbind_surface(&self, surface_id: u32) -> Option<AppSession> {
        let mut owner: Option<String> = None;
        for entry in self.sessions.iter() {
            if entry.surfaces.contains(&surface_id) {
                owner = Some(entry.session_id.clone());
                break;
            }
        }
        let owner = owner?;

        let mut updated: Option<AppSession> = None;
        if let Some(mut entry) = self.sessions.get_mut(&owner) {
            entry.surfaces.retain(|s| *s != surface_id);
            updated = Some(entry.clone());
        }
        let session = updated?;
        self.emit_lifecycle("AppSurfaceUnmapped", &session);

        // Drop synthesized foreign-process sessions when their last surface
        // goes away. We detect those as pid==0 sessions that hold no surfaces.
        if session.surfaces.is_empty() && session.pid == 0 {
            self.sessions.remove(&owner);
        }
        Some(session)
    }

    /// All sessions that currently own at least one surface.
    pub fn list_surface_sessions(&self) -> Vec<AppSession> {
        let mut v: Vec<AppSession> = self
            .sessions
            .iter()
            .filter(|e| !e.surfaces.is_empty())
            .map(|e| e.clone())
            .collect();
        v.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        v
    }

    /// Look up the session owning a specific surface, if any.
    pub fn session_for_surface(&self, surface_id: u32) -> Option<AppSession> {
        for entry in self.sessions.iter() {
            if entry.surfaces.contains(&surface_id) {
                return Some(entry.clone());
            }
        }
        None
    }

    // ---------- Internal helpers ----------

    fn transition(&self, session_id: &str, next: AppState) -> Option<AppSession> {
        let mut updated: Option<AppSession> = None;
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.state = next;
            updated = Some(entry.clone());
        }
        if let Some(s) = updated.as_ref() {
            let signal = match next {
                AppState::Starting => "AppStarting",
                AppState::Running => "AppRunning",
                AppState::Suspended => "AppSuspended",
                AppState::Stopped => "AppStopped",
                AppState::Crashed => "AppCrashed",
            };
            self.emit_lifecycle(signal, s);
        }
        updated
    }

    fn emit_lifecycle(&self, signal_name: &'static str, session: &AppSession) {
        let bus = match self.bus_snapshot() {
            Some(b) => b,
            None => return,
        };
        let payload = session.to_json();
        // Fire-and-forget on the runtime so we don't block the caller.
        tokio::spawn(async move {
            if let Err(e) = bus.emit_signal(APPHOST_IFACE, signal_name, payload).await {
                tracing::warn!("AppHost signal '{}' failed: {}", signal_name, e);
            }
        });
    }

    fn apps_root() -> PathBuf {
        PathBuf::from(APPS_ROOT)
    }

    fn user_apps_root() -> PathBuf {
        PathBuf::from(USER_APPS_ROOT)
    }

    fn nova_native_root() -> PathBuf {
        PathBuf::from(NOVA_NATIVE_ROOT)
    }

    fn send_signal(pid: u32, signal: Signal) -> Result<()> {
        kill(Pid::from_raw(pid as i32), signal)
            .with_context(|| format!("kill({:?}) on pid {}", signal, pid))?;
        Ok(())
    }

    /// Signal a session's whole process GROUP, falling back to the bare pid.
    ///
    /// Apps are spawned with `process_group(0)` (see `spawn_and_track`), so the
    /// group id equals the pid AppHost recorded. Signalling `-pid` therefore
    /// reaches every process in that group — including one vela re-exec'd
    /// itself into, which has a different pid and is exactly what leaked before
    /// (an orphan Firefox window surviving `close`).
    ///
    /// The single-pid fallback covers sessions that predate the group (e.g. a
    /// synthesized foreign-process session), where no group was ever set up.
    fn signal_session(pid: u32, signal: Signal) {
        if pid == 0 {
            return;
        }
        // Negative pid == "the process group with this id".
        if kill(Pid::from_raw(-(pid as i32)), signal).is_err() {
            let _ = kill(Pid::from_raw(pid as i32), signal);
        }
    }

    /// True once nothing in the session's process group is alive.
    fn session_gone(pid: u32) -> bool {
        if pid == 0 {
            return true;
        }
        kill(Pid::from_raw(-(pid as i32)), None).is_err() && !Self::pid_alive(pid)
    }

    fn pid_alive(pid: u32) -> bool {
        kill(Pid::from_raw(pid as i32), None).is_ok()
    }
}

// ---------- JSON helpers consumed by the daemon dispatcher ----------

impl AppHost {
    pub fn launch_json(&self, app_id: &str, params: Option<Value>) -> Result<Value> {
        let session = self.launch(app_id, params)?;
        Ok(session.to_json())
    }

    pub fn close_json(&self, session_id: &str) -> Result<Value> {
        let session = self.close(session_id)?;
        Ok(session.to_json())
    }

    pub fn kill_json(&self, session_id: &str) -> Result<Value> {
        let session = self.kill(session_id)?;
        Ok(session.to_json())
    }

    pub fn restart_json(&self, session_id: &str) -> Result<Value> {
        let session = self.restart(session_id)?;
        Ok(session.to_json())
    }

    pub fn list_apps_json(&self) -> Result<Value> {
        let manifests = self.list_apps()?;
        let running: Vec<AppSession> = self.list_running();
        let mut by_app: HashMap<String, Vec<&AppSession>> = HashMap::new();
        for s in &running {
            by_app.entry(s.app_id.clone()).or_default().push(s);
        }
        let apps: Vec<Value> = manifests
            .into_iter()
            .map(|m| {
                let sessions = by_app
                    .get(&m.id)
                    .map(|v| v.iter().map(|s| s.to_json()).collect::<Vec<_>>())
                    .unwrap_or_default();
                let running = !sessions.is_empty();
                json!({
                    "id": m.id,
                    "name": m.name,
                    "exec": m.exec,
                    "icon": m.icon,
                    "version": m.version,
                    "category": m.category,
                    "entry": m.entry,
                    "running": running,
                    "sessions": sessions,
                })
            })
            .collect();
        let count = apps.len();
        Ok(json!({ "apps": apps, "count": count }))
    }

    pub fn list_running_json(&self) -> Result<Value> {
        let sessions: Vec<Value> = self.list_running().iter().map(|s| s.to_json()).collect();
        let count = sessions.len();
        Ok(json!({ "sessions": sessions, "count": count }))
    }

    pub fn get_state_json(&self, session_id: &str) -> Result<Value> {
        match self.get_state(session_id) {
            Some(state) => Ok(json!({
                "sessionId": session_id,
                "state": state.as_str(),
            })),
            None => Err(anyhow!("session '{}' not found", session_id)),
        }
    }

    pub fn bind_surface_json(
        &self,
        app_id: &str,
        surface_id: u32,
        session_id: Option<&str>,
        pid: Option<u32>,
    ) -> Result<Value> {
        let session = self.bind_surface(app_id, surface_id, session_id, pid);
        Ok(session.to_json())
    }

    pub fn unbind_surface_json(&self, surface_id: u32) -> Result<Value> {
        match self.unbind_surface(surface_id) {
            Some(session) => Ok(session.to_json()),
            None => Ok(json!({ "surfaceId": surface_id, "unbound": false })),
        }
    }

    pub fn list_surface_sessions_json(&self) -> Result<Value> {
        let sessions: Vec<Value> = self
            .list_surface_sessions()
            .iter()
            .map(|s| s.to_json())
            .collect();
        let count = sessions.len();
        Ok(json!({ "sessions": sessions, "count": count }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip() {
        let raw = r#"{
            "id": "demo",
            "name": "Demo",
            "exec": "./launch",
            "icon": "icon.png",
            "version": "0.1.0",
            "category": "tools"
        }"#;
        let m: AppManifest = serde_json::from_str(raw).unwrap();
        assert_eq!(m.id, "demo");
        assert_eq!(m.category.as_deref(), Some("tools"));
        assert!(m.env.is_none());
    }

    #[test]
    fn state_strings_stable() {
        assert_eq!(AppState::Starting.as_str(), "starting");
        assert_eq!(AppState::Running.as_str(), "running");
        assert_eq!(AppState::Stopped.as_str(), "stopped");
        assert_eq!(AppState::Crashed.as_str(), "crashed");
    }

    #[test]
    fn list_running_is_empty_initially() {
        let host = AppHost::new();
        assert!(host.list_running().is_empty());
        assert!(host.active_session().is_none());
    }

    #[test]
    fn bind_surface_synthesizes_foreign_session() {
        let host = AppHost::new();
        let session = host.bind_surface("some.app", 42, None, Some(1234));
        assert_eq!(session.surfaces, vec![42]);
        assert_eq!(session.app_id, "some.app");
        assert_eq!(session.pid, 1234);
        assert_eq!(host.list_surface_sessions().len(), 1);
        assert_eq!(
            host.session_for_surface(42).map(|s| s.session_id),
            Some(session.session_id.clone())
        );
    }

    #[test]
    fn unbind_surface_drops_foreign_session_when_last() {
        let host = AppHost::new();
        let s = host.bind_surface("foreign.app", 7, None, None);
        assert_eq!(host.list_surface_sessions().len(), 1);
        let unbound = host.unbind_surface(7).expect("should unbind");
        assert!(unbound.surfaces.is_empty());
        // pid==0 + 0 surfaces should reap the synthesized session.
        assert!(host.list_surface_sessions().is_empty());
        assert!(host.get_state(&s.session_id).is_none());
    }

    #[test]
    fn bind_then_unbind_emits_no_panic_without_bus() {
        // Without `set_bus`, emit_lifecycle is a no-op — exercise the path.
        let host = AppHost::new();
        let _ = host.bind_surface("a", 1, None, None);
        let _ = host.bind_surface("a", 2, None, None);
        let _ = host.unbind_surface(1);
        assert_eq!(host.list_surface_sessions()[0].surfaces, vec![2]);
    }

    /// Register an already-spawned pid as a running session on `host`.
    fn track_session(host: &AppHost, sid: &str, app_id: &str, pid: u32) {
        host.sessions.insert(
            sid.to_string(),
            AppSession {
                session_id: sid.to_string(),
                app_id: app_id.into(),
                pid,
                started_at: 0,
                state: AppState::Running,
                surfaces: vec![],
            },
        );
        host.pid_index.write().unwrap().insert(pid, sid.to_string());
    }

    /// Spawn a child that exits immediately with `code`, and leak the handle so
    /// `Child::drop` can't reap it out from under `reap_once`.
    fn spawn_exiting_child(code: u8) -> u32 {
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("exit {code}")])
            .spawn()
            .expect("spawn /bin/sh");
        let pid = child.id();
        std::mem::forget(child);
        pid
    }

    /// Both reaper outcomes are asserted in **one** test on purpose.
    ///
    /// `reap_once` calls `waitpid(-1)`, which is process-wide, and cargo runs
    /// tests as parallel threads of a single process. As two separate tests
    /// these raced: whichever called `reap_once` first consumed *both*
    /// children's exit statuses, so the loser's session was never finalized and
    /// it failed on every full-suite run while passing in isolation. One test,
    /// one reap, both assertions -- no cross-test interference to lose to.
    #[test]
    fn reaper_reports_crashed_and_stopped_exits_distinctly() {
        let host = AppHost::new();

        // Non-zero exit with nothing in expected_stops -> Crashed.
        let crashed_pid = spawn_exiting_child(7);
        track_session(&host, "sess-crash", "crashy", crashed_pid);

        // Clean exit that the user asked for -> Stopped.
        let stopped_pid = spawn_exiting_child(0);
        track_session(&host, "sess-stop", "stoppy", stopped_pid);
        host.expected_stops
            .write()
            .unwrap()
            .insert("sess-stop".to_string());

        // Both children must have actually exited before WNOHANG sees them;
        // poll rather than sleeping a fixed beat so a loaded machine can't
        // turn this into a spurious failure.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            host.reap_once();
            if host.get_state("sess-crash").is_none() && host.get_state("sess-stop").is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(
            host.get_state("sess-crash").is_none(),
            "crashed session should be removed after reap"
        );
        assert!(
            host.get_state("sess-stop").is_none(),
            "stopped session should be removed after reap"
        );
    }
}
