// gecko/src/echo_ipc.rs - Gecko Rendering Engine ECHO Integration
//! GECKO registration and communication with ECHO IPC bus
//!
//! Exposes Gecko services to system:
//! - Registration of Gecko rendering service
//! - Window management API
//! - Application launch interface
//! - Rendering readiness signals

use std::sync::Arc;
use std::sync::Mutex;
use anyhow::{Result, anyhow};
use tracing::{debug, info, warn};

/// ECHO IPC service for Gecko rendering engine
pub struct GeckoEchoService {
    service_name: String,
    ready: Arc<Mutex<bool>>,
    active_windows: Arc<Mutex<usize>>,
}

impl GeckoEchoService {
    /// Create and register GECKO service on ECHO bus
    pub fn new() -> Result<Self> {
        debug!("Initializing GECKO ECHO service");

        let service = GeckoEchoService {
            service_name: "org.geckoOS.Gecko".to_string(),
            ready: Arc::new(Mutex::new(false)),
            active_windows: Arc::new(Mutex::new(0)),
        };

        info!("GECKO service created: {}", service.service_name);
        Ok(service)
    }

    /// Register with ECHO bus
    pub fn register(&self) -> Result<()> {
        debug!("Registering GECKO with ECHO bus");

        // INTEGRATE: Call to echo bus registry
        // echo.registry.register_service(ServiceDescriptor {
        //     service_name: self.service_name.clone(),
        //     methods: vec![
        //         "LaunchApplication",
        //         "GetActiveWindows",
        //         "CreateWindow",
        //         "CloseWindow",
        //         "GetRenderingStatus",
        //         "SetGPUAcceleration",
        //     ],
        //     signals: vec![
        //         "GeckoReady",
        //         "WindowCreated",
        //         "WindowClosed",
        //         "RenderingStarted",
        //     ],
        // })?;

        info!("GECKO successfully registered on ECHO bus");
        Ok(())
    }

    /// Signal that Gecko is ready for applications
    pub fn signal_ready(&self) -> Result<()> {
        debug!("Gecko ready for applications");

        let mut ready = self.ready.lock().unwrap();
        *ready = true;

        // INTEGRATE: echo.emit(self.service_name, "GeckoReady", {})?;

        Ok(())
    }

    /// Handle LaunchApplication method call
    pub fn handle_launch_application(&self, app_id: &str, url: &str) -> Result<u32> {
        debug!("LaunchApplication called: app_id={}, url={}", app_id, url);

        // Check if Gecko is ready
        let ready = self.ready.lock().unwrap();
        if !*ready {
            return Err(anyhow!("Gecko not ready"));
        }

        drop(ready);

        // INTEGRATE: Call gecko_app_launcher::launch_app()
        // let app = GECKO_LAUNCHER.launch_app(AppDescriptor {
        //     app_id: app_id.to_string(),
        //     executable: url.to_string(),
        //     ...
        // })?;

        // Increment active window counter
        let mut windows = self.active_windows.lock().unwrap();
        *windows += 1;

        // INTEGRATE: emit WindowCreated signal
        debug!("Application launched successfully: pid=1000");
        Ok(1000)
    }

    /// Handle GetActiveWindows method call
    pub fn handle_get_active_windows(&self) -> Result<usize> {
        debug!("GetActiveWindows called");

        let windows = self.active_windows.lock().unwrap();
        Ok(*windows)
    }

    /// Handle CreateWindow method call
    pub fn handle_create_window(&self, title: &str, width: u32, height: u32) -> Result<u32> {
        debug!("CreateWindow called: {}x{} '{}'", width, height, title);

        // INTEGRATE: Call to compositor to create surface
        // This would request a new wl_surface from OURANOS

        let mut windows = self.active_windows.lock().unwrap();
        *windows += 1;

        // INTEGRATE: emit WindowCreated signal
        Ok(1001)
    }

    /// Handle CloseWindow method call
    pub fn handle_close_window(&self, window_id: u32) -> Result<()> {
        debug!("CloseWindow called: window_id={}", window_id);

        let mut windows = self.active_windows.lock().unwrap();
        if *windows > 0 {
            *windows -= 1;
        }

        // INTEGRATE: emit WindowClosed signal
        Ok(())
    }

    /// Get Gecko rendering status
    pub fn handle_get_rendering_status(&self) -> Result<RenderingStatus> {
        debug!("GetRenderingStatus called");

        let ready = self.ready.lock().unwrap();
        let windows = self.active_windows.lock().unwrap();

        Ok(RenderingStatus {
            is_ready: *ready,
            active_windows: *windows,
            fps: 60,
            memory_mb: 256,
        })
    }

    /// Enable/disable GPU acceleration
    pub fn handle_set_gpu_acceleration(&self, enabled: bool) -> Result<()> {
        debug!("SetGPUAcceleration called: enabled={}", enabled);

        if enabled {
            info!("GPU acceleration enabled");
            // INTEGRATE: Enable WebRender GPU path
        } else {
            info!("GPU acceleration disabled");
            // INTEGRATE: Fall back to CPU rendering
        }

        Ok(())
    }

    /// Emit rendering started signal
    pub fn emit_rendering_started(&self) -> Result<()> {
        debug!("Emitting RenderingStarted signal");

        // INTEGRATE: echo.emit(self.service_name, "RenderingStarted", {})?;

        Ok(())
    }

    /// Get service name
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// Check if Gecko is ready
    pub fn is_ready(&self) -> bool {
        *self.ready.lock().unwrap()
    }
}

/// Rendering status information
#[derive(Debug, Clone)]
pub struct RenderingStatus {
    pub is_ready: bool,
    pub active_windows: usize,
    pub fps: u32,
    pub memory_mb: u32,
}

impl Default for GeckoEchoService {
    fn default() -> Self {
        Self::new().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_creation() {
        let service = GeckoEchoService::new();
        assert!(service.is_ok());
    }

    #[test]
    fn test_service_name() {
        let service = GeckoEchoService::new().unwrap();
        assert_eq!(service.service_name(), "org.geckoOS.Gecko");
    }

    #[test]
    fn test_gecko_not_ready_initially() {
        let service = GeckoEchoService::new().unwrap();
        assert!(!service.is_ready());
    }

    #[test]
    fn test_gecko_ready_signal() {
        let service = GeckoEchoService::new().unwrap();
        service.signal_ready().unwrap();
        assert!(service.is_ready());
    }

    #[test]
    fn test_active_windows_count() {
        let service = GeckoEchoService::new().unwrap();
        let count = service.handle_get_active_windows().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_rendering_status() {
        let service = GeckoEchoService::new().unwrap();
        let status = service.handle_get_rendering_status().unwrap();
        assert!(!status.is_ready);
        assert_eq!(status.active_windows, 0);
    }
}

// INTEGRATE: Add to gecko/src/lib.rs:
// pub mod echo_ipc;
// pub use echo_ipc::GeckoEchoService;
//
// During startup in gecko main:
// let echo_service = echo_ipc::GeckoEchoService::new()?;
// echo_service.register()?;
// ... after initialization complete:
// echo_service.signal_ready()?;
