// gecko/src/echo_ipc.rs

use std::sync::Arc;
use std::sync::Mutex;
use anyhow::{Result, anyhow};
use tracing::{debug, info, warn, error};
use serde_json::json;
use tokio::sync::Mutex as TokioMutex;

use geckoOS_echo::{
    IPCBus, ServiceInterface, MethodDefinition, SignalDefinition, 
    ArgumentDefinition, EchoClient
};

/// Window data structure
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub app_id: String,
    pub pid: u32,
}

/// ECHO IPC service for Gecko rendering engine
pub struct GeckoEchoService {
    service_name: String,
    ready: Arc<Mutex<bool>>,
    windows: Arc<Mutex<Vec<WindowInfo>>>,  // ← Track actual windows
    client: Option<Arc<TokioMutex<EchoClient>>>,
}

impl GeckoEchoService {
    pub fn new() -> Result<Self> {
        debug!("Initializing GECKO ECHO service");

        let service = GeckoEchoService {
            service_name: "org.geckoOS.Gecko".to_string(),
            ready: Arc::new(Mutex::new(false)),
            windows: Arc::new(Mutex::new(Vec::new())),
            client: None,
        };

        info!("GECKO service created: {}", service.service_name);
        Ok(service)
    }

    /// Register with ECHO bus
    pub async fn register(&mut self) -> Result<()> {
        debug!("Registering GECKO with ECHO bus");

        let mut client = EchoClient::connect().await?;

        // Define methods
        let methods = vec![
            MethodDefinition {
                name: "LaunchApplication".to_string(),
                input_args: vec![
                    ArgumentDefinition {
                        name: "app_id".to_string(),
                        arg_type: "s".to_string(),
                    },
                    ArgumentDefinition {
                        name: "url".to_string(),
                        arg_type: "s".to_string(),
                    },
                ],
                output_args: vec![
                    ArgumentDefinition {
                        name: "window_id".to_string(),
                        arg_type: "u".to_string(),
                    },
                ],
            },
            MethodDefinition {
                name: "CreateWindow".to_string(),
                input_args: vec![
                    ArgumentDefinition {
                        name: "title".to_string(),
                        arg_type: "s".to_string(),
                    },
                    ArgumentDefinition {
                        name: "width".to_string(),
                        arg_type: "u".to_string(),
                    },
                    ArgumentDefinition {
                        name: "height".to_string(),
                        arg_type: "u".to_string(),
                    },
                ],
                output_args: vec![
                    ArgumentDefinition {
                        name: "window_id".to_string(),
                        arg_type: "u".to_string(),
                    },
                ],
            },
            MethodDefinition {
                name: "CloseWindow".to_string(),
                input_args: vec![
                    ArgumentDefinition {
                        name: "window_id".to_string(),
                        arg_type: "u".to_string(),
                    },
                ],
                output_args: vec![],
            },
            MethodDefinition {
                name: "GetActiveWindows".to_string(),
                input_args: vec![],
                output_args: vec![
                    ArgumentDefinition {
                        name: "count".to_string(),
                        arg_type: "u".to_string(),
                    },
                ],
            },
            MethodDefinition {
                name: "GetRenderingStatus".to_string(),
                input_args: vec![],
                output_args: vec![
                    ArgumentDefinition {
                        name: "status".to_string(),
                        arg_type: "v".to_string(),
                    },
                ],
            },
            MethodDefinition {
                name: "SetGPUAcceleration".to_string(),
                input_args: vec![
                    ArgumentDefinition {
                        name: "enabled".to_string(),
                        arg_type: "b".to_string(),
                    },
                ],
                output_args: vec![],
            },
        ];

        // Define signals
        let signals = vec![
            SignalDefinition {
                name: "GeckoReady".to_string(),
                arguments: vec![],
            },
            SignalDefinition {
                name: "WindowCreated".to_string(),
                arguments: vec![
                    ArgumentDefinition {
                        name: "window_id".to_string(),
                        arg_type: "u".to_string(),
                    },
                    ArgumentDefinition {
                        name: "title".to_string(),
                        arg_type: "s".to_string(),
                    },
                    ArgumentDefinition {
                        name: "width".to_string(),
                        arg_type: "u".to_string(),
                    },
                    ArgumentDefinition {
                        name: "height".to_string(),
                        arg_type: "u".to_string(),
                    },
                ],
            },
            SignalDefinition {
                name: "WindowClosed".to_string(),
                arguments: vec![
                    ArgumentDefinition {
                        name: "window_id".to_string(),
                        arg_type: "u".to_string(),
                    },
                ],
            },
            SignalDefinition {
                name: "RenderingStarted".to_string(),
                arguments: vec![],
            },
            SignalDefinition {
                name: "RenderingError".to_string(),
                arguments: vec![
                    ArgumentDefinition {
                        name: "error_code".to_string(),
                        arg_type: "i".to_string(),
                    },
                    ArgumentDefinition {
                        name: "message".to_string(),
                        arg_type: "s".to_string(),
                    },
                ],
            },
        ];

        // Create interface
        let interface = ServiceInterface {
            name: "org.geckoOS.Gecko".to_string(),
            methods,
            signals,
            properties: vec![],
        };

        // Register
        let _service_id = client.register_service(&self.service_name, interface).await?;
        self.client = Some(Arc::new(TokioMutex::new(client)));

        info!("GECKO successfully registered on ECHO bus");
        Ok(())
    }

    /// Signal ready
    pub async fn signal_ready(&self) -> Result<()> {
        debug!("Gecko ready for applications");

        let mut ready = self.ready.lock().unwrap();
        *ready = true;
        drop(ready);

        if let Some(client) = &self.client {
            let client = client.lock().await;
            client.emit_signal(
                &self.service_name,
                "GeckoReady",
                json!({}),
            ).await?;
        }

        info!("GeckoReady signal emitted");
        Ok(())
    }

    // ============================================================================
    // METHOD IMPLEMENTATIONS
    // ============================================================================

    /// Launch an application
    pub async fn handle_launch_application(&self, app_id: &str, url: &str) -> Result<u32> {
        debug!("LaunchApplication: app_id={}, url={}", app_id, url);

        // Check if Gecko is ready
        let ready = self.ready.lock().unwrap();
        if !*ready {
            return Err(anyhow!("Gecko not ready"));
        }
        drop(ready);

        // TODO: Integrate with actual app launcher
        // For now, simulate with unique window ID
        let window_id = self.next_window_id();

        // TODO: Actually launch process
        // let pid = launcher::launch_app(app_id, url)?;
        let pid = 1000 + window_id;  // Fake PID

        // Add to windows list
        let mut windows = self.windows.lock().unwrap();
        windows.push(WindowInfo {
            id: window_id,
            title: app_id.to_string(),
            width: 1280,
            height: 720,
            app_id: app_id.to_string(),
            pid,
        });
        drop(windows);

        // Emit signal
        if let Some(client) = &self.client {
            let client = client.lock().await;
            client.emit_signal(
                &self.service_name,
                "WindowCreated",
                json!({
                    "window_id": window_id,
                    "title": app_id,
                    "width": 1280,
                    "height": 720,
                    "pid": pid,
                }),
            ).await?;
        }

        info!("Application launched: app_id={}, window_id={}, pid={}", app_id, window_id, pid);
        Ok(window_id)
    }

    /// Create a window
    pub async fn handle_create_window(&self, title: &str, width: u32, height: u32) -> Result<u32> {
        debug!("CreateWindow: title={}, {}x{}", title, width, height);

        let window_id = self.next_window_id();

        // TODO: Request surface from OURANOS (Wayland compositor)
        // let surface = OURANOS.create_surface(title, width, height).await?;

        // Add to windows
        let mut windows = self.windows.lock().unwrap();
        windows.push(WindowInfo {
            id: window_id,
            title: title.to_string(),
            width,
            height,
            app_id: "generic".to_string(),
            pid: 0,
        });
        drop(windows);

        // Emit signal
        if let Some(client) = &self.client {
            let client = client.lock().await;
            client.emit_signal(
                &self.service_name,
                "WindowCreated",
                json!({
                    "window_id": window_id,
                    "title": title,
                    "width": width,
                    "height": height,
                }),
            ).await?;
        }

        info!("Window created: id={}, title={}", window_id, title);
        Ok(window_id)
    }

    /// Close a window
    pub async fn handle_close_window(&self, window_id: u32) -> Result<()> {
        debug!("CloseWindow: window_id={}", window_id);

        // Find and remove window
        let mut windows = self.windows.lock().unwrap();
        windows.retain(|w| w.id != window_id);
        drop(windows);

        // TODO: Destroy Wayland surface and render context
        // OURANOS.destroy_surface(window_id).await?;

        // Emit signal
        if let Some(client) = &self.client {
            let client = client.lock().await;
            client.emit_signal(
                &self.service_name,
                "WindowClosed",
                json!({
                    "window_id": window_id,
                }),
            ).await?;
        }

        info!("Window closed: id={}", window_id);
        Ok(())
    }

    /// Get active window count
    pub fn handle_get_active_windows(&self) -> Result<usize> {
        debug!("GetActiveWindows");
        
        let windows = self.windows.lock().unwrap();
        let count = windows.len();
        
        info!("Active windows: {}", count);
        Ok(count)
    }

    /// Get rendering status
    pub fn handle_get_rendering_status(&self) -> Result<RenderingStatus> {
        debug!("GetRenderingStatus");

        let ready = self.ready.lock().unwrap();
        let windows = self.windows.lock().unwrap();

        // TODO: Get real FPS from WebRender
        // TODO: Get real memory from Gecko
        
        let status = RenderingStatus {
            is_ready: *ready,
            active_windows: windows.len(),
            fps: 60,  // TODO: Real value
            memory_mb: 256,  // TODO: Real value
        };

        info!("Rendering status: ready={}, windows={}, fps={}", 
              status.is_ready, status.active_windows, status.fps);
        
        Ok(status)
    }

    /// Set GPU acceleration
    pub async fn handle_set_gpu_acceleration(&self, enabled: bool) -> Result<()> {
        debug!("SetGPUAcceleration: enabled={}", enabled);

        if enabled {
            // TODO: Check GPU available
            // if !GECKO.is_gpu_available() {
            //     return Err(anyhow!("GPU not available"));
            // }
            
            // TODO: Enable GPU rendering
            info!("GPU acceleration enabled");
        } else {
            // TODO: Fallback to CPU rendering
            info!("GPU acceleration disabled - CPU fallback");
        }

        Ok(())
    }

    /// Emit rendering started
    pub async fn emit_rendering_started(&self) -> Result<()> {
        debug!("Emitting RenderingStarted");

        if let Some(client) = &self.client {
            let client = client.lock().await;
            client.emit_signal(
                &self.service_name,
                "RenderingStarted",
                json!({}),
            ).await?;
        }

        Ok(())
    }

    // ============================================================================
    // HELPER METHODS
    // ============================================================================

    /// Generate next window ID
    fn next_window_id(&self) -> u32 {
        let windows = self.windows.lock().unwrap();
        let max_id = windows.iter().map(|w| w.id).max().unwrap_or(0);
        max_id + 1
    }

    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    pub fn is_ready(&self) -> bool {
        *self.ready.lock().unwrap()
    }

    pub fn get_windows(&self) -> Vec<WindowInfo> {
        self.windows.lock().unwrap().clone()
    }
}

impl Default for GeckoEchoService {
    fn default() -> Self {
        Self::new().unwrap()
    }
}

/// Rendering status
#[derive(Debug, Clone)]
pub struct RenderingStatus {
    pub is_ready: bool,
    pub active_windows: usize,
    pub fps: u32,
    pub memory_mb: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_creation() {
        let service = GeckoEchoService::new().unwrap();
        assert_eq!(service.service_name(), "org.geckoOS.Gecko");
        assert!(!service.is_ready());
    }

    #[tokio::test]
    async fn test_window_lifecycle() {
        let mut service = GeckoEchoService::new().unwrap();
        service.signal_ready().await.unwrap();
        
        // Create window
        let id1 = service.handle_create_window("Test", 1280, 720).await.unwrap();
        assert_eq!(id1, 1);
        
        let id2 = service.handle_create_window("Test2", 800, 600).await.unwrap();
        assert_eq!(id2, 2);
        
        // Check count
        let count = service.handle_get_active_windows().unwrap();
        assert_eq!(count, 2);
        
        // Close window
        service.handle_close_window(id1).await.unwrap();
        let count = service.handle_get_active_windows().unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_rendering_status() {
        let service = GeckoEchoService::new().unwrap();
        let status = service.handle_get_rendering_status().unwrap();
        assert!(!status.is_ready);
        assert_eq!(status.active_windows, 0);
    }
}
