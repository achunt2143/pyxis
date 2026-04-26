//! Application Manager Service
//!
//! Manages application lifecycle, installation, and listing for GeckoOS.
//!
//! Apps are installed at `/usr/share/geckoOS/apps/<app_id>/` and described by a
//! `manifest.json` of the form:
//!
//! ```json
//! {
//!   "id": "terminal",
//!   "name": "Terminal",
//!   "exec": "./launch",
//!   "icon": "icon.png",
//!   "version": "1.0.0"
//! }
//! ```
//!
//! Running apps are tracked in a process-global `DashMap<app_id, AppEntry>`.
//! The active (foregrounded) app is tracked via an atomic slot that maps to a
//! `String` lookup in the registry.

use anyhow::{anyhow, Context, Result};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::time::SystemTime;

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

const APPS_ROOT: &str = "/usr/share/geckoOS/apps";

/// Manifest describing a single installed app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppManifest {
    pub id: String,
    pub name: String,
    pub exec: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub version: String,
}

/// Registry entry for a running app.
#[derive(Debug, Clone, Serialize)]
pub struct AppEntry {
    pub pid: u32,
    pub app_id: String,
    pub name: String,
    pub started_at: SystemTime,
}

pub struct AppManagerService;

// ---------- Static state ----------

fn registry() -> &'static DashMap<String, AppEntry> {
    static REGISTRY: OnceLock<DashMap<String, AppEntry>> = OnceLock::new();
    REGISTRY.get_or_init(DashMap::new)
}

fn active_slot() -> &'static RwLock<Option<String>> {
    static ACTIVE: OnceLock<RwLock<Option<String>>> = OnceLock::new();
    ACTIVE.get_or_init(|| RwLock::new(None))
}

// ---------- Helpers ----------

fn apps_root() -> PathBuf {
    PathBuf::from(APPS_ROOT)
}

fn manifest_path(app_id: &str) -> PathBuf {
    apps_root().join(app_id).join("manifest.json")
}

fn read_manifest(app_id: &str) -> Result<AppManifest> {
    let path = manifest_path(app_id);
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read manifest at {}", path.display()))?;
    let manifest: AppManifest = serde_json::from_str(&content)
        .with_context(|| format!("invalid manifest JSON at {}", path.display()))?;
    if manifest.id != app_id {
        return Err(anyhow!(
            "manifest id mismatch: expected '{}', got '{}'",
            app_id,
            manifest.id
        ));
    }
    Ok(manifest)
}

fn send_signal(pid: u32, signal: Signal) -> Result<()> {
    kill(Pid::from_raw(pid as i32), signal)
        .with_context(|| format!("failed to send {:?} to pid {}", signal, pid))?;
    Ok(())
}

fn pid_alive(pid: u32) -> bool {
    // Sending signal 0 is a "does this process exist + can I signal it?" check.
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

fn entry_to_json(entry: &AppEntry) -> Value {
    let started_secs = entry
        .started_at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    json!({
        "pid": entry.pid,
        "appId": entry.app_id,
        "name": entry.name,
        "startedAt": started_secs,
    })
}

// ---------- Service impl ----------

impl AppManagerService {
    /// Launch an app by id. If already running, raise to front via SIGUSR1.
    pub fn launch(app_id: &str, _params: Option<Value>) -> Result<Value> {
        // If app is already in registry and the process is alive, raise it.
        if let Some(entry) = registry().get(app_id).map(|e| e.clone()) {
            if pid_alive(entry.pid) {
                // SIGUSR1: by convention, "bring me to front"
                let _ = send_signal(entry.pid, Signal::SIGUSR1);
                return Ok(json!({
                    "success": true,
                    "pid": entry.pid,
                    "appId": app_id,
                    "alreadyRunning": true,
                }));
            } else {
                // Stale entry — clean up and fall through to spawn.
                registry().remove(app_id);
            }
        }

        let manifest = read_manifest(app_id)?;
        let app_dir = apps_root().join(&manifest.id);

        // Resolve exec relative to app dir if it starts with "./"
        let exec_path: PathBuf = if manifest.exec.starts_with("./") {
            app_dir.join(&manifest.exec[2..])
        } else if Path::new(&manifest.exec).is_absolute() {
            PathBuf::from(&manifest.exec)
        } else {
            app_dir.join(&manifest.exec)
        };

        let child = Command::new(&exec_path)
            .current_dir(&app_dir)
            .spawn()
            .with_context(|| {
                format!(
                    "failed to spawn '{}' for app '{}'",
                    exec_path.display(),
                    app_id
                )
            })?;

        let pid = child.id();
        // We deliberately drop the Child handle — the process is now an
        // independent child. The kernel will reap it via SIGCHLD; if a parent
        // wait is needed in the future, switch to a global Child registry.
        std::mem::forget(child);

        let entry = AppEntry {
            pid,
            app_id: app_id.to_string(),
            name: manifest.name.clone(),
            started_at: SystemTime::now(),
        };
        registry().insert(app_id.to_string(), entry);

        // Newly launched app becomes active.
        if let Ok(mut slot) = active_slot().write() {
            *slot = Some(app_id.to_string());
        }

        Ok(json!({
            "success": true,
            "pid": pid,
            "appId": app_id,
        }))
    }

    /// Gracefully terminate a running app via SIGTERM and remove from registry.
    pub fn close(app_id: &str) -> Result<Value> {
        let pid = match registry().get(app_id) {
            Some(e) => e.pid,
            None => {
                return Ok(json!({
                    "success": true,
                    "appId": app_id,
                    "wasRunning": false,
                }))
            }
        };

        let _ = send_signal(pid, Signal::SIGTERM);
        registry().remove(app_id);

        if let Ok(mut slot) = active_slot().write() {
            if slot.as_deref() == Some(app_id) {
                *slot = None;
            }
        }

        Ok(json!({
            "success": true,
            "appId": app_id,
            "pid": pid,
        }))
    }

    /// List installed apps (from manifests on disk) and merge running state.
    pub fn list() -> Result<Value> {
        let mut apps: Vec<Value> = Vec::new();
        let root = apps_root();

        if root.exists() {
            for entry in fs::read_dir(&root)
                .with_context(|| format!("read_dir {}", root.display()))?
            {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let app_id = match path.file_name().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let manifest = match read_manifest(&app_id) {
                    Ok(m) => m,
                    Err(_) => continue, // skip directories without a valid manifest
                };

                let running_entry = registry().get(&app_id).map(|e| e.clone());
                let running = running_entry
                    .as_ref()
                    .map(|e| pid_alive(e.pid))
                    .unwrap_or(false);

                apps.push(json!({
                    "id": manifest.id,
                    "name": manifest.name,
                    "icon": manifest.icon,
                    "version": manifest.version,
                    "exec": manifest.exec,
                    "running": running,
                    "pid": running_entry.map(|e| e.pid),
                }));
            }
        }

        let count = apps.len();
        Ok(json!({
            "apps": apps,
            "count": count,
        }))
    }

    /// Look up the manifest for a single app by id. Returns the manifest plus
    /// running info if applicable.
    pub fn get_info(app_id: &str) -> Result<Value> {
        let manifest = read_manifest(app_id)?;
        let running_entry = registry().get(app_id).map(|e| e.clone());
        let running = running_entry
            .as_ref()
            .map(|e| pid_alive(e.pid))
            .unwrap_or(false);

        Ok(json!({
            "id": manifest.id,
            "name": manifest.name,
            "icon": manifest.icon,
            "version": manifest.version,
            "exec": manifest.exec,
            "running": running,
            "pid": running_entry.map(|e| e.pid),
        }))
    }

    /// Get the currently active (foregrounded) app id, if any.
    pub fn get_active() -> Result<Value> {
        let slot = active_slot()
            .read()
            .map_err(|_| anyhow!("active_slot lock poisoned"))?;
        match slot.as_ref() {
            Some(id) => Ok(json!({ "appId": id })),
            None => Ok(json!({ "appId": null })),
        }
    }

    /// Mark an app as the active (foregrounded) app.
    pub fn set_active(app_id: &str) -> Result<Value> {
        if !registry().contains_key(app_id) {
            return Err(anyhow!("app '{}' is not running", app_id));
        }
        let mut slot = active_slot()
            .write()
            .map_err(|_| anyhow!("active_slot lock poisoned"))?;
        *slot = Some(app_id.to_string());
        Ok(json!({
            "success": true,
            "appId": app_id,
        }))
    }

    /// Forcefully kill an app via SIGKILL.
    pub fn kill(app_id: &str) -> Result<Value> {
        let pid = match registry().get(app_id) {
            Some(e) => e.pid,
            None => return Err(anyhow!("app '{}' is not running", app_id)),
        };
        let _ = send_signal(pid, Signal::SIGKILL);
        registry().remove(app_id);

        if let Ok(mut slot) = active_slot().write() {
            if slot.as_deref() == Some(app_id) {
                *slot = None;
            }
        }
        Ok(json!({
            "success": true,
            "appId": app_id,
            "pid": pid,
        }))
    }

    /// Restart: close then launch.
    pub fn restart(app_id: &str) -> Result<Value> {
        Self::close(app_id)?;
        // Brief pause so the process exits before we re-spawn.
        std::thread::sleep(std::time::Duration::from_millis(150));
        Self::launch(app_id, None)
    }

    /// Install an app from a tarball at `app_path` into `/usr/share/geckoOS/apps/`.
    /// The tarball is expected to extract into a directory matching the
    /// manifest's `id`.
    pub fn install(app_path: &str) -> Result<Value> {
        let path = Path::new(app_path);
        if !path.exists() {
            return Err(anyhow!("install source not found: {}", app_path));
        }

        let root = apps_root();
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create {}", root.display()))?;

        // Use system tar — avoids pulling in a tarball crate and matches how
        // most rootfs tooling distributes packages.
        let status = Command::new("tar")
            .arg("-xf")
            .arg(path)
            .arg("-C")
            .arg(&root)
            .status()
            .with_context(|| format!("failed to invoke tar on {}", path.display()))?;
        if !status.success() {
            return Err(anyhow!(
                "tar exited with status {} extracting {}",
                status,
                app_path
            ));
        }

        // Find the newest top-level dir under apps_root and validate its manifest.
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

        // Validate manifest
        let _manifest = read_manifest(&app_id)
            .with_context(|| format!("invalid or missing manifest after install for {}", app_id))?;

        Ok(json!({
            "success": true,
            "appId": app_id,
        }))
    }

    /// Uninstall an app by id. Closes it first if running.
    pub fn uninstall(app_id: &str) -> Result<Value> {
        if registry().contains_key(app_id) {
            let _ = Self::close(app_id);
        }

        let dir = apps_root().join(app_id);
        if !dir.exists() {
            return Err(anyhow!("app '{}' is not installed", app_id));
        }
        fs::remove_dir_all(&dir)
            .with_context(|| format!("failed to remove {}", dir.display()))?;

        Ok(json!({
            "success": true,
            "appId": app_id,
        }))
    }
}

// Allow entry_to_json to be used elsewhere (kept public-ish for tests).
#[allow(dead_code)]
pub(crate) fn _entry_to_json_for_tests(entry: &AppEntry) -> Value {
    entry_to_json(entry)
}
