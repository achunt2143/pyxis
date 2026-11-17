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
use std::path::Path;
use std::process::Command;

use crate::services::apphost::AppHost;
use crate::services::nova::Nova;

pub struct AppManagerService;

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

    /// Install by delegating to `Nova::install`, which does all verification
    /// and extraction (this module never extracts anything itself).
    pub fn install(app_path: &str) -> Result<Value> {
        let mut res = Nova::global().install(app_path)?;
        if let Some(obj) = res.as_object_mut() {
            obj.insert("success".to_string(), json!(true));
        }
        Ok(res)
    }

    pub fn uninstall(app_id: &str) -> Result<Value> {
        let mut res = Nova::global().uninstall(app_id)?;
        if let Some(obj) = res.as_object_mut() {
            obj.insert("success".to_string(), json!(true));
        }
        Ok(res)
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

/// Inspect `package_path` and reject any unsafe member: absolute or `..`
/// paths, and anything that is not a regular file or directory (symlinks,
/// hardlinks, devices, fifos). Callers pass their own private copy of the
/// package and extract that same copy, never the original.
pub(crate) fn validate_tar_file(package_path: &Path) -> Result<()> {
    let run = |args: &[&str]| -> Result<String> {
        let out = Command::new("tar")
            .args(args)
            .arg("-f")
            .arg(package_path)
            .output()
            .with_context(|| format!("list archive {}", package_path.display()))?;
        if !out.status.success() {
            return Err(anyhow!(
                "tar exited with {} inspecting {}",
                out.status,
                package_path.display()
            ));
        }
        String::from_utf8(out.stdout).context("archive contains non-UTF-8 entry names")
    };
    // Escaped names keep one member per line; a backslash then means the
    // name held a control character or a backslash, which we refuse.
    let names = run(&["-t", "--quoting-style=escape"])?;
    if names.contains('\\') {
        return Err(anyhow!("archive member name contains control characters"));
    }
    validate_tar_entries(&names)?;
    let verbose = run(&["-tv", "--quoting-style=escape"])?;
    validate_tar_types(&verbose, names.lines().count())
}

/// `tar -tv` lines start with a type letter: `-` file, `d` directory,
/// `l` symlink, `h` hardlink, `c`/`b`/`p` devices and fifos.
pub(crate) fn validate_tar_types(verbose_listing: &str, expected: usize) -> Result<()> {
    let lines: Vec<&str> = verbose_listing.lines().collect();
    if lines.len() != expected {
        return Err(anyhow!("archive listing is inconsistent"));
    }
    for line in lines {
        match line.chars().next() {
            Some('-') | Some('d') => {}
            _ => return Err(anyhow!("unsupported archive member (not a plain file or directory): {:?}", line)),
        }
    }
    Ok(())
}

/// Validates every archive member path returned by `tar -tf` to block
/// absolute paths, empty lines, and `..` traversal components.
pub(crate) fn validate_tar_entries(listing: &str) -> Result<()> {
    let entries: Vec<&str> = listing.lines().collect();
    if entries.is_empty() {
        return Err(anyhow!("archive has no entries"));
    }
    for entry in entries {
        if entry.is_empty()
            || entry.starts_with('/')
            || entry.split('/').any(|seg| seg == "..")
            || Path::new(entry).components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(anyhow!("unsafe archive entry path: {:?}", entry));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_tar_entries;
    use std::process::Command;

    #[test]
    fn validates_safe_and_unsafe_tar_entries() {
        assert!(validate_tar_entries("com.example.app/\ncom.example.app/index.html\n./manifest.webapp\n").is_ok());
        assert!(validate_tar_entries("").is_err());
        assert!(validate_tar_entries("\n").is_err());
        assert!(validate_tar_entries("/etc/passwd\n").is_err());
        assert!(validate_tar_entries("../evil\n").is_err());
        assert!(validate_tar_entries("app/../../etc/shadow\n").is_err());
        assert!(super::validate_tar_types("-rw-r--r-- 0/0 1 d a\ndrwxr-xr-x 0/0 0 d b/\n", 2).is_ok());
        assert!(super::validate_tar_types("lrwxrwxrwx 0/0 0 d a -> /etc\n", 1).is_err());
        assert!(super::validate_tar_types("hrwxr-xr-x 0/0 0 d a link to b\n", 1).is_err());
    }

    #[test]
    fn real_tar_with_a_symlink_member_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("manifest.webapp"), "{}").unwrap();
        let good = dir.path().join("good.tar");
        let run = |args: &[&str]| assert!(Command::new("tar").args(args).status().unwrap().success());
        run(&["-cf", good.to_str().unwrap(), "-C", src.to_str().unwrap(), "manifest.webapp"]);
        assert!(super::validate_tar_file(&good).is_ok());
        std::os::unix::fs::symlink("/etc/shadow", src.join("evil")).unwrap();
        let bad = dir.path().join("bad.tar");
        run(&["-cf", bad.to_str().unwrap(), "-C", src.to_str().unwrap(), "manifest.webapp", "evil"]);
        assert!(super::validate_tar_file(&bad).is_err());
    }
}
