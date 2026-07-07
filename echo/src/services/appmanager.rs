//! Application Manager Service (compatibility shim).
//!
//! Historical name for the per-app launcher. As of the App Hosting MVP, the
//! real lifecycle, manifest discovery, and signal emission live in
//! [`crate::services::apphost::AppHost`]. This module is now a thin shim that:
//!
//!   - keeps the existing `org.geckoOS.appManager` IPC method shape intact
//!     so older clients and daemon dispatch code keep working
//!   - resolves `app_id` -> latest session and forwards to the new host
//!   - delegates manifest install/uninstall to the new host where applicable
//!
//! New shells should target the `org.geckoOS.appHost` interface directly,
//! which exposes session-aware `LaunchApp`, `CloseApp`, `KillApp`, etc.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;
use std::path::PathBuf;

use crate::services::apphost::AppHost;

const APPS_ROOT: &str = "/usr/share/geckoOS/apps";

pub struct AppManagerService;

fn apps_root() -> PathBuf {
    PathBuf::from(APPS_ROOT)
}

impl AppManagerService {
    /// Launch by app id. Behaviour preserved from the old service: if the app
    /// is already running, surface the existing session as `alreadyRunning`.
    pub fn launch(app_id: &str, params: Option<Value>) -> Result<Value> {
        let host = AppHost::global();
        if let Some(existing) = host.find_session_by_app(app_id) {
            // Treat anything currently in the registry as "still alive" — the
            // host transitions to Crashed/Stopped on its own.
            return Ok(json!({
                "success": true,
                "appId": app_id,
                "sessionId": existing.session_id,
                "pid": existing.pid,
                "alreadyRunning": true,
            }));
        }

        let session = host.launch(app_id, params)?;
        Ok(json!({
            "success": true,
            "appId": app_id,
            "sessionId": session.session_id,
            "pid": session.pid,
        }))
    }

    pub fn close(app_id: &str) -> Result<Value> {
        let host = AppHost::global();
        let session = match host.find_session_by_app(app_id) {
            Some(s) => s,
            None => {
                return Ok(json!({
                    "success": true,
                    "appId": app_id,
                    "wasRunning": false,
                }));
            }
        };
        let closed = host.close(&session.session_id)?;
        Ok(json!({
            "success": true,
            "appId": app_id,
            "sessionId": closed.session_id,
            "pid": closed.pid,
        }))
    }

    /// List installed apps with running flag — same shape as before.
    pub fn list() -> Result<Value> {
        AppHost::global().list_apps_json()
    }

    pub fn get_info(app_id: &str) -> Result<Value> {
        let manifest = AppHost::read_manifest(app_id)?;
        let running = AppHost::global().find_session_by_app(app_id);
        Ok(json!({
            "id": manifest.id,
            "name": manifest.name,
            "icon": manifest.icon,
            "version": manifest.version,
            "exec": manifest.exec,
            "running": running.is_some(),
            "pid": running.as_ref().map(|s| s.pid),
            "sessionId": running.as_ref().map(|s| s.session_id.clone()),
        }))
    }

    pub fn install(app_path: &str) -> Result<Value> {
        // Tarball install — same logic as the previous AppManagerService.
        // No integrity verification here: this is the LEGACY native-app
        // path (a tarball containing manifest.json + an already-native
        // exec), kept only for old-style installs. The real, verified
        // install path for actual pyxis:// web apps is now
        // `services::nova::Nova::install` (SHA-256 `integrity.sha256` check
        // against `manifest.webapp`, atomic staging + rename — see that
        // module's doc comment) — new callers should prefer
        // `org.geckoOS.nova` -> `Install`, not this shim.
        let path = Path::new(app_path);
        if !path.exists() {
            return Err(anyhow!("install source not found: {}", app_path));
        }

        let root = apps_root();
        fs::create_dir_all(&root)
            .with_context(|| format!("create {}", root.display()))?;

        let status = Command::new("tar")
            .arg("-xf")
            .arg(path)
            .arg("-C")
            .arg(&root)
            .status()
            .with_context(|| format!("invoke tar on {}", path.display()))?;
        if !status.success() {
            return Err(anyhow!("tar exited with {} extracting {}", status, app_path));
        }

        let mut newest: Option<(SystemTime, PathBuf)> = None;
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            match &newest {
                Some((t, _)) if modified <= *t => {}
                _ => newest = Some((modified, p)),
            }
        }
        let dir = newest
            .map(|(_, p)| p)
            .ok_or_else(|| anyhow!("no app directory found after extraction"))?;
        let app_id = dir
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("invalid app dir name"))?
            .to_string();

        // Validate
        let _ = AppHost::read_manifest(&app_id)
            .with_context(|| format!("invalid manifest after install for {}", app_id))?;
        Ok(json!({ "success": true, "appId": app_id }))
    }

    pub fn uninstall(app_id: &str) -> Result<Value> {
        // Close any running session first.
        if let Some(session) = AppHost::global().find_session_by_app(app_id) {
            let _ = AppHost::global().close(&session.session_id);
        }
        let dir = apps_root().join(app_id);
        if !dir.exists() {
            return Err(anyhow!("app '{}' is not installed", app_id));
        }
        fs::remove_dir_all(&dir)
            .with_context(|| format!("remove {}", dir.display()))?;
        Ok(json!({ "success": true, "appId": app_id }))
    }

    pub fn kill(app_id: &str) -> Result<Value> {
        let host = AppHost::global();
        let session = host
            .find_session_by_app(app_id)
            .ok_or_else(|| anyhow!("app '{}' is not running", app_id))?;
        let killed = host.kill(&session.session_id)?;
        Ok(json!({
            "success": true,
            "appId": app_id,
            "sessionId": killed.session_id,
            "pid": killed.pid,
        }))
    }

    pub fn restart(app_id: &str) -> Result<Value> {
        let host = AppHost::global();
        if let Some(session) = host.find_session_by_app(app_id) {
            let restarted = host.restart(&session.session_id)?;
            Ok(json!({
                "success": true,
                "appId": app_id,
                "sessionId": restarted.session_id,
                "pid": restarted.pid,
            }))
        } else {
            // Not running — just launch.
            Self::launch(app_id, None)
        }
    }

    pub fn set_active(app_id: &str) -> Result<Value> {
        let host = AppHost::global();
        let session = host
            .find_session_by_app(app_id)
            .ok_or_else(|| anyhow!("app '{}' is not running", app_id))?;
        host.set_active(&session.session_id)?;
        Ok(json!({
            "success": true,
            "appId": app_id,
            "sessionId": session.session_id,
        }))
    }

    pub fn get_active() -> Result<Value> {
        let host = AppHost::global();
        match host.active_session() {
            Some(sid) => {
                let app_id = host
                    .list_running()
                    .into_iter()
                    .find(|s| s.session_id == sid)
                    .map(|s| s.app_id);
                Ok(json!({
                    "appId": app_id,
                    "sessionId": sid,
                }))
            }
            None => Ok(json!({ "appId": null, "sessionId": null })),
        }
    }
}
