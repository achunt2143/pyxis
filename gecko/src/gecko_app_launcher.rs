// gecko/src/gecko_app_launcher.rs
//! Application lifecycle management within Gecko context
//!
//! Manages launching and tracking applications that run within Gecko:
//! - JavaScript applications loaded in Gecko DOM
//! - Sandboxed application contexts
//! - IPC communication to system services
//! - Surface creation and management

use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use anyhow::{Result, anyhow};
use tracing::{debug, info, warn};

/// Application identifier and metadata
#[derive(Debug, Clone)]
pub struct AppDescriptor {
    pub app_id: String,
    pub name: String,
    pub executable: String,  // URL or path
    pub icon: Option<String>,
    pub category: String,
    pub app_type: AppType,  // Web or Native
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppType {
    Web,
    Native,
}

/// Running Gecko application instance
#[derive(Debug, Clone)]
pub struct GeckoApp {
    pub app_id: String,
    pub process_id: u32,
    pub context_id: u32,
    pub window_id: u32,
    pub status: AppStatus,
    pub created_at: std::time::SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppStatus {
    Launching,
    Running,
    Suspended,
    Terminating,
    Terminated,
}

/// Gecko application launcher and lifecycle manager
pub struct GeckoAppLauncher {
    apps: Arc<Mutex<HashMap<String, GeckoApp>>>,
    next_context_id: Arc<Mutex<u32>>,
    next_process_id: Arc<Mutex<u32>>,
}

impl GeckoAppLauncher {
    /// Create new application launcher
    pub fn new() -> Self {
        debug!("Initializing Gecko app launcher");
        GeckoAppLauncher {
            apps: Arc::new(Mutex::new(HashMap::new())),
            next_context_id: Arc::new(Mutex::new(1)),
            next_process_id: Arc::new(Mutex::new(1000)),
        }
    }

    /// Launch a new application in Gecko
    pub fn launch_app(&self, descriptor: AppDescriptor) -> Result<GeckoApp> {
        info!("Launching application: {}", descriptor.app_id);

        // Get next IDs
        let mut ctx_id = self.next_context_id.lock().unwrap();
        let context_id = *ctx_id;
        *ctx_id += 1;

        let mut proc_id = self.next_process_id.lock().unwrap();
        let process_id = *proc_id;
        *proc_id += 1;

        // Create application instance
        let app = GeckoApp {
            app_id: descriptor.app_id.clone(),
            process_id,
            context_id,
            window_id: 0,  // Will be set when surface created
            status: AppStatus::Launching,
            created_at: std::time::SystemTime::now(),
        };

        // Store in registry
        let mut apps = self.apps.lock().unwrap();
        apps.insert(descriptor.app_id.clone(), app.clone());

        debug!(
            "Application launched: id={}, context={}, process={}",
            descriptor.app_id, context_id, process_id
        );

        // INTEGRATE: Call to CRONUS to track process
        // cronus.register_process(ProcessInfo {
        //     pid: process_id,
        //     app_id: descriptor.app_id,
        //     ppid: std::process::id(),
        //     status: ProcessStatus::Running,
        // })?;

        Ok(app)
    }

    /// Initialize application context (DOM, JavaScript environment)
    pub fn init_context(&self, app_id: &str) -> Result<u32> {
        debug!("Initializing Gecko context for app: {}", app_id);

        let apps = self.apps.lock().unwrap();
        let app = apps.get(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        let context_id = app.context_id;

        // This would:
        // - Create isolated DOM context
        // - Load JavaScript environment
        // - Set up IPC bridge
        debug!("Gecko context initialized: {}", context_id);

        Ok(context_id)
    }

    /// Load application in context
    pub fn load_app(&self, app_id: &str, url: &str) -> Result<()> {
        debug!("Loading application {} from {}", app_id, url);

        let mut apps = self.apps.lock().unwrap();
        let app = apps.get_mut(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        // This would:
        // - Load document at URL
        // - Execute initialization scripts
        // - Wait for ready signal

        debug!("Application loaded: {}", app_id);
        Ok(())
    }

    /// Create Wayland surface for application
    pub fn create_surface(&self, app_id: &str) -> Result<u32> {
        debug!("Creating Wayland surface for app: {}", app_id);

        let mut apps = self.apps.lock().unwrap();
        let app = apps.get_mut(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        // This would:
        // - Request surface from OURANOS
        // - Set up buffer attachment
        // - Configure XDG toplevel

        app.window_id = self.next_process_id.lock().unwrap().next_window_id();

        debug!("Surface created for app {}: window_id={}", app_id, app.window_id);
        Ok(app.window_id)
    }

    /// Set application status
    pub fn set_status(&self, app_id: &str, status: AppStatus) -> Result<()> {
        debug!("Setting app {} status to {:?}", app_id, status);

        let mut apps = self.apps.lock().unwrap();
        let app = apps.get_mut(app_id)
            .ok_or_else(|| anyhow!("Application not found: {}", app_id))?;

        app.status = status;

        // INTEGRATE: Emit to ECHO IPC for subscribers
        // echo.emit("org.geckoOS.Gecko", "ApplicationStatusChanged", {
        //     app_id: app_id,
        //     status: status,
        // });

        Ok(())
    }

    /// Get application by ID
    pub fn get_app(&self, app_id: &str) -> Option<GeckoApp> {
        let apps = self.apps.lock().unwrap();
        apps.get(app_id).cloned()
    }

    /// List all running applications
    pub fn list_apps(&self) -> Vec<GeckoApp> {
        let apps = self.apps.lock().unwrap();
        apps.values().cloned().collect()
    }

    /// Terminate application
    pub fn terminate_app(&self, app_id: &str) -> Result<()> {
        info!("Terminating application: {}", app_id);

        let mut apps = self.apps.lock().unwrap();
        if let Some(app) = apps.get_mut(app_id) {
            app.status = AppStatus::Terminating;

            // INTEGRATE: Call CRONUS to untrack process
            // cronus.terminate_process(app.process_id)?;

            apps.remove(app_id);
            debug!("Application terminated: {}", app_id);
        }

        Ok(())
    }

    /// Get total running applications
    pub fn count_apps(&self) -> usize {
        self.apps.lock().unwrap().len()
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

    fn test_descriptor() -> AppDescriptor {
        AppDescriptor {
            app_id: "test-app".to_string(),
            name: "Test Application".to_string(),
            executable: "file:///apps/test/index.html".to_string(),
            icon: None,
            category: "utilities".to_string(),
            app_type: AppType::Web,
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
        let descriptor = test_descriptor();

        let result = launcher.launch_app(descriptor);
        assert!(result.is_ok());
        assert_eq!(launcher.count_apps(), 1);
    }

    #[test]
    fn test_app_retrieval() {
        let launcher = GeckoAppLauncher::new();
        let descriptor = test_descriptor();

        launcher.launch_app(descriptor.clone()).unwrap();
        let app = launcher.get_app(&descriptor.app_id);
        assert!(app.is_some());
    }

    #[test]
    fn test_app_termination() {
        let launcher = GeckoAppLauncher::new();
        let descriptor = test_descriptor();

        launcher.launch_app(descriptor.clone()).unwrap();
        assert_eq!(launcher.count_apps(), 1);

        launcher.terminate_app(&descriptor.app_id).unwrap();
        assert_eq!(launcher.count_apps(), 0);
    }

    #[test]
    fn test_status_change() {
        let launcher = GeckoAppLauncher::new();
        let descriptor = test_descriptor();

        launcher.launch_app(descriptor.clone()).unwrap();
        launcher.set_status(&descriptor.app_id, AppStatus::Running).unwrap();

        let app = launcher.get_app(&descriptor.app_id).unwrap();
        assert_eq!(app.status, AppStatus::Running);
    }
}

// INTEGRATE: Add to gecko/src/lib.rs:
// pub mod gecko_app_launcher;
// pub use gecko_app_launcher::{GeckoAppLauncher, AppDescriptor, GeckoApp};
