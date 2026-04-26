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
//! Intentionally NOT in this MVP (tracked as TODOs):
//!   - real Wayland surface registration (compositor work)
//!   - sandboxing / cgroups / namespaces / portals
//!   - permissions enforcement
//!   - SIGCHLD-driven async crash detection (we currently use poll-on-read)
//!   - tarball signature verification

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
use nix::unistd::Pid;
use uuid::Uuid;

use crate::IPCBus;

const APPS_ROOT: &str = "/usr/share/geckoOS/apps";

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
    Stopped,
    Crashed,
}

impl AppState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AppState::Starting => "starting",
            AppState::Running => "running",
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
}

impl AppSession {
    fn to_json(&self) -> Value {
        json!({
            "sessionId": self.session_id,
            "appId": self.app_id,
            "pid": self.pid,
            "startedAt": self.started_at,
            "state": self.state.as_str(),
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
}

impl AppHost {
    fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            active: RwLock::new(None),
            bus: RwLock::new(None),
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

    /// Read and validate a single app manifest by id.
    pub fn read_manifest(app_id: &str) -> Result<AppManifest> {
        let path = Self::manifest_path(app_id);
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

    /// Scan `/usr/share/geckoOS/apps/*/manifest.json` and return all valid
    /// manifests. Invalid/missing manifests are silently skipped — the caller
    /// gets the working subset.
    pub fn list_apps(&self) -> Result<Vec<AppManifest>> {
        let mut out = Vec::new();
        let root = Self::apps_root();
        if !root.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(&root)
            .with_context(|| format!("read_dir {}", root.display()))?
        {
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
                out.push(m);
            }
        }
        // Stable ordering across calls — easier to consume in shells/tests.
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    // ---------- Lifecycle ----------

    /// Launch a new session for `app_id`. Always spawns; callers that want
    /// "raise existing" semantics should consult `find_session_by_app` first.
    pub fn launch(&self, app_id: &str, params: Option<Value>) -> Result<AppSession> {
        let manifest = Self::read_manifest(app_id)?;
        let app_dir = Self::apps_root().join(&manifest.id);

        // Resolve exec relative to the app directory when given as `./...`.
        let exec_path: PathBuf = if manifest.exec.starts_with("./") {
            app_dir.join(&manifest.exec[2..])
        } else if Path::new(&manifest.exec).is_absolute() {
            PathBuf::from(&manifest.exec)
        } else {
            app_dir.join(&manifest.exec)
        };

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
        };
        self.sessions.insert(session_id.clone(), starting.clone());
        self.emit_lifecycle("AppStarting", &starting);

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
                    exec_path.display(),
                    app_id,
                    e
                ));
            }
        };

        let pid = child.id();
        // Drop the Child intentionally — see TODO at top of file. Switching
        // to a global Child registry + SIGCHLD handler is followup work.
        std::mem::forget(child);

        let running = AppSession {
            session_id: session_id.clone(),
            app_id: app_id.to_string(),
            pid,
            started_at,
            state: AppState::Running,
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

    /// SIGTERM a session; updates state to `Stopped`.
    pub fn close(&self, session_id: &str) -> Result<AppSession> {
        let session = self
            .sessions
            .get(session_id)
            .map(|s| s.clone())
            .ok_or_else(|| anyhow!("session '{}' not found", session_id))?;
        if session.pid > 0 {
            let _ = Self::send_signal(session.pid, Signal::SIGTERM);
        }
        let stopped = self.transition(session_id, AppState::Stopped);
        self.sessions.remove(session_id);
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
        if session.pid > 0 {
            let _ = Self::send_signal(session.pid, Signal::SIGKILL);
        }
        let crashed = self.transition(session_id, AppState::Crashed);
        self.sessions.remove(session_id);
        if let Ok(mut slot) = self.active.write() {
            if slot.as_deref() == Some(session_id) {
                *slot = None;
            }
        }
        Ok(crashed.unwrap_or(session))
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

    fn manifest_path(app_id: &str) -> PathBuf {
        Self::apps_root().join(app_id).join("manifest.json")
    }

    fn send_signal(pid: u32, signal: Signal) -> Result<()> {
        kill(Pid::from_raw(pid as i32), signal)
            .with_context(|| format!("kill({:?}) on pid {}", signal, pid))?;
        Ok(())
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
}
