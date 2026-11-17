// puppis/src/gecko_app_launcher.rs
//! Application lifecycle management within Gecko context

use std::sync::Arc;
use std::collections::HashMap;
use parking_lot::Mutex;
use anyhow::{Result, anyhow};
use tracing::{debug, info, warn};
use serde_json::json;
use std::time::{SystemTime, Duration};

/// Request to launch a native application, keyed by app name.
///
/// Moved here directly (2026-07-07) from the old `gaia` crate's
/// `ui_server.rs` (an actix-web request-body type this module `pub use`'d
/// across a crate boundary that no longer exists) — this was the ONLY
/// coupling between the genuinely-used launcher logic below and `gaia`'s
/// otherwise-dead HTTP/WebSocket server, so moving the type here instead of
/// bringing the whole server across was enough to fully decouple this
/// module.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LaunchAppRequest {
    pub app_name: String,
}

/// Application type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppType {
    Web,
    Native,
}

/// Application status enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AppStatus {
    Launching,
    Running,
    Suspended,
    Terminating,
    Terminated,
}

impl std::fmt::Display for AppStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppStatus::Launching => write!(f, "Launching"),
            AppStatus::Running => write!(f, "Running"),
            AppStatus::Suspended => write!(f, "Suspended"),
            AppStatus::Terminating => write!(f, "Terminating"),
            AppStatus::Terminated => write!(f, "Terminated"),
        }
    }
}

/// Application descriptor (metadata)
#[derive(Debug, Clone)]
pub struct AppDescriptor {
    pub app_id: String,
    pub name: String,
    pub executable: String,
    pub icon: Option<String>,
    pub category: String,
    pub app_type: AppType,
}

/// Running application instance
#[derive(Debug, Clone, serde::Serialize)]
pub struct GeckoApp {
    pub app_id: String,
    pub process_id: u32,
    pub context_id: u32,
    pub window_id: u32,
    pub status: AppStatus,
    pub created_at: SystemTime,
    pub last_activity: SystemTime,
    pub memory_mb: u32,
    pub cpu_percent: f32,
}

/// Application statistics
#[derive(Debug, Clone, serde::Serialize)]
pub struct AppStats {
    pub app_id: String,
    pub uptime_secs: u64,
    pub memory_mb: u32,
    pub cpu_percent: f32,
    pub last_activity: SystemTime,
}

/// Gecko application launcher
pub struct GeckoAppLauncher {
    apps: Arc<Mutex<HashMap<String, GeckoApp>>>,
    next_context_id: Arc<Mutex<u32>>,
    next_process_id: Arc<Mutex<u32>>,
    next_window_id: Arc<Mutex<u32>>,
}

impl GeckoAppLauncher {
    /// Create new application launcher
    pub fn new() -> Self {
        debug!("Initializing Gecko app launcher");
        GeckoAppLauncher {
            apps: Arc::new(Mutex::new(HashMap::new())),
            next_context_id: Arc::new(Mutex::new(1)),
            next_process_id: Arc::new(Mutex::new(1000)),
            next_window_id: Arc::new(Mutex::new(100)),
        }
    }

    // ========================================================================
    // APPLICATION LIFECYCLE
    // ========================================================================

    /// Launch a new application
    pub fn launch_app(&self, request: LaunchAppRequest) -> Result<GeckoApp> {
        info!("Launching application: {}", request.app_name);

        if request.app_name.is_empty() {
            return Err(anyhow!("Application name cannot be empty"));
        }

        let mut apps = self.apps.lock();
        if apps.contains_key(&request.app_name) {
            return Err(anyhow!("Application already running: {}", request.app_name));
        }

        // Allocate IDs
        let context_id = {
            let mut ctx = self.next_context_id.lock();
            let id = *ctx;
            *ctx += 1;
            id
        };

        let process_id = {
            let mut pid = self.next_process_id.lock();
            let id = *pid;
            *pid += 1;
            id
        };

        let now = SystemTime::now();
        let app = GeckoApp {
            app_id: request.app_name.clone(),
            process_id,
            context_id,
            window_id: 0,
            status: AppStatus::Launching,
            created_at: now,
            last_activity: now,
            memory_mb: 0,
            cpu_percent: 0.0,
        };

        apps.insert(request.app_name.clone(), app.clone());

        info!(
            "Application launched: id={}, process={}, context={}",
            request.app_name, process_id, context_id
        );

        Ok(app)
    }

    /// Initialize application context
    pub fn init_context(&self, app_id: &str) -> Result<u32> {
        debug!("Initializing Gecko context for app: {}", app_id);
        let mut apps = self.apps.lock();
        let app = apps
            .get_mut(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        let context_id = app.context_id;
        info!("Gecko context initialized: {} (id={})", app_id, context_id);
        Ok(context_id)
    }

    /// Load application in context
    pub fn load_app(&self, app_id: &str, url: &str) -> Result<()> {
        debug!("Loading application {} from {}", app_id, url);

        if url.is_empty() {
            return Err(anyhow!("URL cannot be empty"));
        }

        let mut apps = self.apps.lock();
        let _app = apps
            .get_mut(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        info!("Application loaded: {} from {}", app_id, url);
        Ok(())
    }

    /// Create Wayland surface
    pub fn create_surface(&self, app_id: &str) -> Result<u32> {
        debug!("Creating Wayland surface for app: {}", app_id);

        let mut apps = self.apps.lock();
        let app = apps
            .get_mut(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        let window_id = {
            let mut wid = self.next_window_id.lock();
            let id = *wid;
            *wid += 1;
            id
        };

        app.window_id = window_id;
        info!("Surface created for app {}: window_id={}", app_id, window_id);
        Ok(window_id)
    }

    /// Set application status
    pub fn set_status(&self, app_id: &str, status: AppStatus) -> Result<()> {
        debug!("Setting app {} status to {}", app_id, status);

        let mut apps = self.apps.lock();
        let app = apps
            .get_mut(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        let old_status = app.status;
        app.status = status;
        app.last_activity = SystemTime::now();

        info!(
            "Application status changed: {} ({} -> {})",
            app_id, old_status, status
        );

        Ok(())
    }

    // ========================================================================
    // APPLICATION QUERIES
    // ========================================================================

    /// Get application by ID
    pub fn get_app(&self, app_id: &str) -> Option<GeckoApp> {
        self.apps.lock().get(app_id).cloned()
    }

    /// List all running applications
    pub fn list_apps(&self) -> Vec<GeckoApp> {
        self.apps.lock().values().cloned().collect()
    }

    /// List applications by status
    pub fn list_apps_by_status(&self, status: AppStatus) -> Vec<GeckoApp> {
        self.apps
            .lock()
            .values()
            .filter(|app| app.status == status)
            .cloned()
            .collect()
    }

    /// Get application by process ID
    pub fn get_app_by_pid(&self, pid: u32) -> Option<GeckoApp> {
        self.apps
            .lock()
            .values()
            .find(|app| app.process_id == pid)
            .cloned()
    }

    /// Get application by context ID
    pub fn get_app_by_context(&self, context_id: u32) -> Option<GeckoApp> {
        self.apps
            .lock()
            .values()
            .find(|app| app.context_id == context_id)
            .cloned()
    }

    /// Get application by window ID
    pub fn get_app_by_window(&self, window_id: u32) -> Option<GeckoApp> {
        self.apps
            .lock()
            .values()
            .find(|app| app.window_id == window_id)
            .cloned()
    }

    /// Count total applications
    pub fn count_apps(&self) -> usize {
        self.apps.lock().len()
    }

    /// Count applications by status
    pub fn count_apps_by_status(&self, status: AppStatus) -> usize {
        self.apps
            .lock()
            .values()
            .filter(|app| app.status == status)
            .count()
    }

    /// Check if application exists
    pub fn app_exists(&self, app_id: &str) -> bool {
        self.apps.lock().contains_key(app_id)
    }

    /// Check if application is running
    pub fn is_running(&self, app_id: &str) -> bool {
        self.apps
            .lock()
            .get(app_id)
            .map(|app| app.status == AppStatus::Running)
            .unwrap_or(false)
    }

    // ========================================================================
    // APPLICATION MANAGEMENT
    // ========================================================================

    /// Suspend application
    pub fn suspend_app(&self, app_id: &str) -> Result<()> {
        info!("Suspending application: {}", app_id);

        let mut apps = self.apps.lock();
        let app = apps
            .get_mut(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        if app.status != AppStatus::Running {
            return Err(anyhow!("Cannot suspend app with status: {}", app.status));
        }

        app.status = AppStatus::Suspended;
        app.last_activity = SystemTime::now();

        info!("Application suspended: {}", app_id);
        Ok(())
    }

    /// Resume suspended application
    pub fn resume_app(&self, app_id: &str) -> Result<()> {
        info!("Resuming application: {}", app_id);

        let mut apps = self.apps.lock();
        let app = apps
            .get_mut(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        if app.status != AppStatus::Suspended {
            return Err(anyhow!("Cannot resume app with status: {}", app.status));
        }

        app.status = AppStatus::Running;
        app.last_activity = SystemTime::now();

        info!("Application resumed: {}", app_id);
        Ok(())
    }

    /// Gracefully terminate application
    pub fn terminate_app(&self, app_id: &str) -> Result<()> {
        info!("Terminating application: {}", app_id);

        let mut apps = self.apps.lock();
        if let Some(app) = apps.get_mut(app_id) {
            let old_status = app.status;
            app.status = AppStatus::Terminating;

            // Remove from registry
            apps.remove(app_id);

            info!("Application terminated: {} (was {})", app_id, old_status);
            Ok(())
        } else {
            Err(anyhow!("Application not found: {}", app_id))
        }
    }

    /// Force terminate application
    pub fn force_terminate_app(&self, app_id: &str) -> Result<()> {
        warn!("Force terminating application: {}", app_id);

        if self.apps.lock().remove(app_id).is_some() {
            info!("Application force terminated: {}", app_id);
            Ok(())
        } else {
            Err(anyhow!("Application not found: {}", app_id))
        }
    }

    /// Terminate all applications
    pub fn terminate_all_apps(&self) -> Result<()> {
        info!("Terminating all applications");
        let mut apps = self.apps.lock();
        let count = apps.len();
        apps.clear();
        info!("All {} applications terminated", count);
        Ok(())
    }

    // ========================================================================
    // RESOURCE MANAGEMENT
    // ========================================================================

    /// Update application resource usage
    pub fn update_app_resources(&self, app_id: &str, memory_mb: u32, cpu_percent: f32) -> Result<()> {
        debug!("Updating resources for app {}: memory={}, cpu={}", app_id, memory_mb, cpu_percent);

        let mut apps = self.apps.lock();
        let app = apps
            .get_mut(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        app.memory_mb = memory_mb;
        app.cpu_percent = cpu_percent.max(0.0).min(100.0);
        app.last_activity = SystemTime::now();

        Ok(())
    }

    /// Get application statistics
    pub fn get_app_stats(&self, app_id: &str) -> Result<AppStats> {
        let apps = self.apps.lock();
        let app = apps
            .get(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        let uptime = app
            .created_at
            .elapsed()
            .map(|d| d.as_secs())
            .unwrap_or(0);

        Ok(AppStats {
            app_id: app.app_id.clone(),
            uptime_secs: uptime,
            memory_mb: app.memory_mb,
            cpu_percent: app.cpu_percent,
            last_activity: app.last_activity,
        })
    }

    /// Get all application statistics
    pub fn get_all_stats(&self) -> Vec<AppStats> {
        self.apps
            .lock()
            .values()
            .map(|app| {
                let uptime = app
                    .created_at
                    .elapsed()
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                AppStats {
                    app_id: app.app_id.clone(),
                    uptime_secs: uptime,
                    memory_mb: app.memory_mb,
                    cpu_percent: app.cpu_percent,
                    last_activity: app.last_activity,
                }
            })
            .collect()
    }

    /// Get total memory used by all apps
    pub fn get_total_memory_mb(&self) -> u32 {
        self.apps.lock().values().map(|app| app.memory_mb).sum()
    }

    /// Get average CPU usage across all apps
    pub fn get_average_cpu_percent(&self) -> f32 {
        let apps = self.apps.lock();
        if apps.is_empty() {
            return 0.0;
        }

        let total: f32 = apps.values().map(|app| app.cpu_percent).sum();
        total / apps.len() as f32
    }

    // ========================================================================
    // ACTIVITY TRACKING
    // ========================================================================

    /// Update last activity time for application
    pub fn touch_app(&self, app_id: &str) -> Result<()> {
        debug!("Touching app activity: {}", app_id);

        let mut apps = self.apps.lock();
        let app = apps
            .get_mut(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        app.last_activity = SystemTime::now();
        Ok(())
    }

    /// Get applications inactive for longer than duration
    pub fn get_inactive_apps(&self, duration: Duration) -> Result<Vec<GeckoApp>> {
        let threshold = SystemTime::now() - duration;
        let apps = self.apps.lock();

        let inactive: Vec<_> = apps
            .values()
            .filter(|app| app.last_activity < threshold && app.status == AppStatus::Running)
            .cloned()
            .collect();

        Ok(inactive)
    }

    // ========================================================================
    // VALIDATION & UTILITIES
    // ========================================================================

    /// Validate app is in valid state for operation
    pub fn validate_app_state(&self, app_id: &str, expected_status: AppStatus) -> Result<()> {
        let apps = self.apps.lock();
        let app = apps
            .get(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        if app.status != expected_status {
            return Err(anyhow!(
                "Application in invalid state: expected {}, got {}",
                expected_status,
                app.status
            ));
        }

        Ok(())
    }

    /// Get application info as JSON
    pub fn get_app_info_json(&self, app_id: &str) -> Result<serde_json::Value> {
        let app = self
            .get_app(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        Ok(json!({
            "app_id": app.app_id,
            "process_id": app.process_id,
            "context_id": app.context_id,
            "window_id": app.window_id,
            "status": app.status.to_string(),
            "created_at": app.created_at
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            "memory_mb": app.memory_mb,
            "cpu_percent": app.cpu_percent,
        }))
    }

    /// Get all applications info as JSON
    pub fn get_all_apps_json(&self) -> serde_json::Value {
        let apps = self.list_apps();
        json!(apps
            .iter()
            .map(|app| json!({
                "app_id": app.app_id,
                "process_id": app.process_id,
                "context_id": app.context_id,
                "window_id": app.window_id,
                "status": app.status.to_string(),
                "memory_mb": app.memory_mb,
                "cpu_percent": app.cpu_percent,
            }))
            .collect::<Vec<_>>())
    }

    /// Get debug info
    pub fn get_debug_info(&self) -> serde_json::Value {
        let apps = self.apps.lock();
        let running = apps.values().filter(|a| a.status == AppStatus::Running).count();
        let suspended = apps.values().filter(|a| a.status == AppStatus::Suspended).count();

        json!({
            "total_apps": apps.len(),
            "running": running,
            "suspended": suspended,
            "total_memory_mb": apps.values().map(|a| a.memory_mb).sum::<u32>(),
            "average_cpu_percent": if apps.is_empty() {
                0.0
            } else {
                apps.values().map(|a| a.cpu_percent).sum::<f32>() / apps.len() as f32
            },
        })
    }
}

impl Default for GeckoAppLauncher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_spawns_nothing() {
        // See the crate docs: starting a process here would start it outside
        // pyxis-wlctx, on Evo's host socket, trusted. Building the needles
        // from pieces keeps this file from matching itself.
        let src = include_str!("gecko_app_launcher.rs");
        let body = &src[..src.find("#[cfg(test)]").unwrap()];
        for needle in [
            concat!("std::process", "::Command"),
            concat!("Command", "::new"),
            concat!("tokio::process", "::Command"),
            concat!("libc::", "fork"),
            concat!(".spa", "wn("),
        ] {
            assert!(!body.contains(needle), "gecko_app_launcher.rs now contains {needle}");
        }
    }

    fn test_request() -> LaunchAppRequest {
        LaunchAppRequest {
            app_name: "test-app".to_string(),
        }
    }

    #[test]
    fn test_launcher_creation() {
        let launcher = GeckoAppLauncher::new();
        assert_eq!(launcher.count_apps(), 0);
    }

    #[test]
    fn test_launch_app() {
        let launcher = GeckoAppLauncher::new();
        let result = launcher.launch_app(test_request());
        assert!(result.is_ok());
        assert_eq!(launcher.count_apps(), 1);
    }

    #[test]
    fn test_app_retrieval() {
        let launcher = GeckoAppLauncher::new();
        let req = test_request();
        launcher.launch_app(req.clone()).unwrap();
        let app = launcher.get_app(&req.app_name);
        assert!(app.is_some());
    }

    #[test]
    fn test_duplicate_launch_fails() {
        let launcher = GeckoAppLauncher::new();
        let req = test_request();
        launcher.launch_app(req.clone()).unwrap();
        let result = launcher.launch_app(req);
        assert!(result.is_err());
    }

    #[test]
    fn test_status_change() {
        let launcher = GeckoAppLauncher::new();
        let req = test_request();
        launcher.launch_app(req.clone()).unwrap();
        launcher.set_status(&req.app_name, AppStatus::Running).unwrap();
        let app = launcher.get_app(&req.app_name).unwrap();
        assert_eq!(app.status, AppStatus::Running);
    }

    #[test]
    fn test_suspend_resume() {
        let launcher = GeckoAppLauncher::new();
        let req = test_request();
        launcher.launch_app(req.clone()).unwrap();
        launcher.set_status(&req.app_name, AppStatus::Running).unwrap();
        launcher.suspend_app(&req.app_name).unwrap();
        assert_eq!(
            launcher.get_app(&req.app_name).unwrap().status,
            AppStatus::Suspended
        );
        launcher.resume_app(&req.app_name).unwrap();
        assert_eq!(
            launcher.get_app(&req.app_name).unwrap().status,
            AppStatus::Running
        );
    }

    #[test]
    fn test_resource_tracking() {
        let launcher = GeckoAppLauncher::new();
        let req = test_request();
        launcher.launch_app(req.clone()).unwrap();
        launcher.update_app_resources(&req.app_name, 256, 25.5).unwrap();
        let app = launcher.get_app(&req.app_name).unwrap();
        assert_eq!(app.memory_mb, 256);
        assert_eq!(app.cpu_percent, 25.5);
    }

    #[test]
    fn test_terminate_app() {
        let launcher = GeckoAppLauncher::new();
        let req = test_request();
        launcher.launch_app(req.clone()).unwrap();
        assert_eq!(launcher.count_apps(), 1);
        launcher.terminate_app(&req.app_name).unwrap();
        assert_eq!(launcher.count_apps(), 0);
    }

    #[test]
    fn test_force_terminate() {
        let launcher = GeckoAppLauncher::new();
        let req = test_request();
        launcher.launch_app(req.clone()).unwrap();
        launcher.force_terminate_app(&req.app_name).unwrap();
        assert_eq!(launcher.count_apps(), 0);
    }

    #[test]
    fn test_terminate_all() {
        let launcher = GeckoAppLauncher::new();
        launcher.launch_app(LaunchAppRequest {
            app_name: "app1".to_string(),
        }).unwrap();
        launcher.launch_app(LaunchAppRequest {
            app_name: "app2".to_string(),
        }).unwrap();
        launcher.launch_app(LaunchAppRequest {
            app_name: "app3".to_string(),
        }).unwrap();
        assert_eq!(launcher.count_apps(), 3);
        launcher.terminate_all_apps().unwrap();
        assert_eq!(launcher.count_apps(), 0);
    }
}