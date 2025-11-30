// persephone/src/echo_ipc.rs - Linux App Compatibility Layer
//! PERSEPHONE registration and communication with ECHO IPC bus
//!
//! This module enables launching native Linux applications through ECHO:
//! - Registration of ApplicationLauncher service
//! - Native binary execution (fork/exec)
//! - Wayland surface detection and management
//! - Environment configuration

use std::process::{Command, Stdio};
use std::collections::HashMap;
use anyhow::{Result, anyhow, Context};
use tracing::{debug, info, warn, error};

/// ECHO IPC service for Linux application launching
pub struct PersephoneEchoService {
    service_name: String,
    running_apps: std::sync::Arc<std::sync::Mutex<HashMap<u32, AppProcess>>>,
}

#[derive(Debug, Clone)]
struct AppProcess {
    pid: u32,
    app_id: String,
    executable: String,
    wayland_display: String,
}

impl PersephoneEchoService {
    /// Create and register PERSEPHONE service on ECHO bus
    pub fn new() -> Result<Self> {
        debug!("Initializing PERSEPHONE ECHO service");

        let service = PersephoneEchoService {
            service_name: "org.geckoOS.Persephone".to_string(),
            running_apps: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
        };

        info!("PERSEPHONE service created: {}", service.service_name);
        Ok(service)
    }

    /// Register with ECHO bus
    pub fn register(&self) -> Result<()> {
        debug!("Registering PERSEPHONE with ECHO bus");

        // INTEGRATE: Call to echo bus registry
        // echo.registry.register_service(ServiceDescriptor {
        //     service_name: self.service_name.clone(),
        //     methods: vec![
        //         "ExecuteApplication",
        //         "LaunchBinary",
        //         "GetRunningApplications",
        //         "TerminateApplication",
        //         "SetEnvironment",
        //     ],
        //     signals: vec![
        //         "ApplicationStarted",
        //         "ApplicationTerminated",
        //         "ApplicationError",
        //     ],
        // })?;

        info!("PERSEPHONE successfully registered on ECHO bus");
        Ok(())
    }

    /// Execute a native Linux application
    pub fn handle_execute_application(&self, app_id: &str, executable: &str, args: Vec<String>) -> Result<u32> {
        debug!("ExecuteApplication called: {} ({})", app_id, executable);

        let wayland_display = std::env::var("WAYLAND_DISPLAY")
            .unwrap_or_else(|_| "wayland-0".to_string());

        debug!("Using WAYLAND_DISPLAY: {}", wayland_display);

        // Build command
        let mut cmd = Command::new(executable);
        cmd.args(&args)
            .env("WAYLAND_DISPLAY", &wayland_display)
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Execute
        let child = cmd.spawn()
            .context(format!("Failed to launch application: {}", executable))?;

        let pid = child.id();
        debug!("Application spawned: pid={}, app_id={}", pid, app_id);

        // Track running application
        let app_process = AppProcess {
            pid,
            app_id: app_id.to_string(),
            executable: executable.to_string(),
            wayland_display,
        };

        let mut apps = self.running_apps.lock().unwrap();
        apps.insert(pid, app_process);

        // INTEGRATE: Emit signal
        // self.emit_application_started(pid, app_id)?;

        Ok(pid)
    }

    /// Get list of running applications
    pub fn handle_get_running_applications(&self) -> Result<Vec<RunningApp>> {
        debug!("GetRunningApplications called");

        let apps = self.running_apps.lock().unwrap();
        let running: Vec<RunningApp> = apps.values().map(|app| RunningApp {
            pid: app.pid,
            app_id: app.app_id.clone(),
            executable: app.executable.clone(),
        }).collect();

        Ok(running)
    }

    /// Terminate an application
    pub fn handle_terminate_application(&self, pid: u32) -> Result<()> {
        debug!("TerminateApplication called: pid={}", pid);

        let mut apps = self.running_apps.lock().unwrap();

        if let Some(app) = apps.remove(&pid) {
            // Try graceful terminate first
            let mut cmd = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .spawn()?;

            cmd.wait()?;

            debug!("Application terminated: pid={}", pid);

            // INTEGRATE: Emit signal
            // self.emit_application_terminated(pid, &app.app_id)?;

            Ok(())
        } else {
            Err(anyhow!("Application not found: pid={}", pid))
        }
    }

    /// Set environment variables for app execution
    pub fn handle_set_environment(&self, var_name: &str, var_value: &str) -> Result<()> {
        debug!("SetEnvironment called: {}={}", var_name, var_value);

        // Note: Environment changes apply to future spawned processes
        std::env::set_var(var_name, var_value);

        Ok(())
    }

    /// Emit ApplicationStarted signal
    pub fn emit_application_started(&self, pid: u32, app_id: &str) -> Result<()> {
        debug!("Emitting ApplicationStarted signal: pid={}, app_id={}", pid, app_id);

        // INTEGRATE: echo.emit(self.service_name, "ApplicationStarted", { pid, app_id })?;

        Ok(())
    }

    /// Emit ApplicationTerminated signal
    pub fn emit_application_terminated(&self, pid: u32, app_id: &str) -> Result<()> {
        debug!("Emitting ApplicationTerminated signal: pid={}, app_id={}", pid, app_id);

        // INTEGRATE: echo.emit(self.service_name, "ApplicationTerminated", { pid, app_id })?;

        Ok(())
    }

    /// Get service name
    pub fn service_name(&self) -> &str {
        &self.service_name
    }
}

#[derive(Debug, Clone)]
pub struct RunningApp {
    pub pid: u32,
    pub app_id: String,
    pub executable: String,
}

impl Default for PersephoneEchoService {
    fn default() -> Self {
        Self::new().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_creation() {
        let service = PersephoneEchoService::new();
        assert!(service.is_ok());
    }

    #[test]
    fn test_service_name() {
        let service = PersephoneEchoService::new().unwrap();
        assert_eq!(service.service_name(), "org.geckoOS.Persephone");
    }

    #[test]
    fn test_running_apps_empty() {
        let service = PersephoneEchoService::new().unwrap();
        let result = service.handle_get_running_applications();
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_app_process() {
        let app = AppProcess {
            pid: 5000,
            app_id: "test-app".to_string(),
            executable: "/usr/bin/test".to_string(),
            wayland_display: "wayland-0".to_string(),
        };
        assert_eq!(app.pid, 5000);
    }
}

// INTEGRATE: Add to persephone/src/lib.rs:
// pub mod echo_ipc;
// pub use echo_ipc::PersephoneEchoService;
//
// During startup in persephone daemon:
// let echo_service = echo_ipc::PersephoneEchoService::new()?;
// echo_service.register()?;
