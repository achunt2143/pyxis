pub mod gecko;
pub mod wayland;
pub mod javascript;
pub mod surface;
pub mod renderer;
pub mod gonk;
pub mod ipc;
pub mod compositor_client;
pub mod gecko_app_launcher;
pub mod echo_ipc;

pub use compositor_client::CompositorClient;
pub use gecko_app_launcher::{GeckoAppLauncher, AppDescriptor, GeckoApp};
pub use echo_ipc::GeckoEchoService;


pub use gecko::GeckoInstance;
pub use wayland::WaylandClient;
pub use surface::SurfaceManager;
pub use renderer::RenderingContext;
pub use javascript::SystemAPI;
pub use gonk::GonkInterface;

use anyhow::Result;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing;

/// Main Gecko integration context
pub struct GeckoIntegration {
    gecko: Arc<RwLock<Option<GeckoInstance>>>,
    wayland: Arc<WaylandClient>,
    surfaces: Arc<SurfaceManager>,
    renderer: Arc<RenderingContext>,
    system_api: Arc<SystemAPI>,
    gonk: Arc<GonkInterface>,
}

impl GeckoIntegration {
    /// Initialize Gecko integration with Wayland
    pub async fn new(compositor_socket: &str) -> Result<Self> {
        tracing::info!("Initializing Gecko Integration Module");

        // Connect to Wayland compositor
        let wayland = WaylandClient::connect(compositor_socket).await?;
        tracing::debug!("Connected to Wayland compositor");

        // Initialize Gecko runtime
        let gecko = GeckoInstance::new().await?;
        tracing::debug!("Gecko runtime initialized");

        // Initialize surface manager
        let surfaces = SurfaceManager::new();
        tracing::debug!("Surface manager initialized");

        // Initialize rendering context
        let renderer = RenderingContext::new(&wayland)?;
        tracing::debug!("Rendering context initialized");

        // Initialize system APIs
        let gonk = GonkInterface::new()?;
        let system_api = SystemAPI::new(Arc::new(gonk.clone()))?;
        tracing::debug!("System APIs initialized");

        Ok(GeckoIntegration {
            gecko: Arc::new(RwLock::new(Some(gecko))),
            wayland: Arc::new(wayland),
            surfaces: Arc::new(surfaces),
            renderer: Arc::new(renderer),
            system_api: Arc::new(system_api),
            gonk: Arc::new(gonk),
        })
    }

    /// Create a new Gecko application window
    pub async fn create_app_window(
        &self,
        app_id: &str,
        url: &str,
        width: u32,
        height: u32,
    ) -> Result<u32> {
        tracing::info!("Creating app window: {} ({}x{})", app_id, width, height);

        // Create Wayland surface
        let surface_id = self.wayland.create_surface().await?;
        tracing::debug!("Created Wayland surface: {}", surface_id);

        // Create Gecko content area
        let gecko_lock = self.gecko.read();
        if let Some(gecko) = gecko_lock.as_ref() {
            let window_id = gecko.create_window(app_id, url, width, height).await?;
            tracing::debug!("Created Gecko window: {}", window_id);

            // Map Gecko window to Wayland surface
            self.surfaces
                .map_window_to_surface(window_id, surface_id, width, height)?;
            tracing::debug!("Mapped window {} to surface {}", window_id, surface_id);

            // Configure surface dimensions
            self.wayland.configure_surface(surface_id, width, height).await?;

            tracing::info!("App window created: window={}, surface={}", window_id, surface_id);
            Ok(window_id)
        } else {
            Err(anyhow::anyhow!("Gecko instance not available"))
        }
    }

    /// Destroy an application window
    pub async fn destroy_app_window(&self, window_id: u32) -> Result<()> {
        tracing::info!("Destroying app window: {}", window_id);

        // Get surface ID
        let surface_id = self.surfaces.get_surface_for_window(window_id)?;

        // Destroy Gecko window
        let gecko_lock = self.gecko.read();
        if let Some(gecko) = gecko_lock.as_ref() {
            gecko.destroy_window(window_id).await?;
        }

        // Destroy Wayland surface
        self.wayland.destroy_surface(surface_id).await?;

        // Clean up surface mapping
        self.surfaces.unmap_window(window_id)?;

        tracing::info!("App window destroyed: {}", window_id);
        Ok(())
    }

    /// Render frame for window
    pub async fn render_frame(&self, window_id: u32) -> Result<()> {
        let gecko_lock = self.gecko.read();
        if let Some(gecko) = gecko_lock.as_ref() {
            // Get rendering buffer from Gecko
            let buffer = gecko.get_rendered_buffer(window_id).await?;

            // Get surface for window
            let surface_id = self.surfaces.get_surface_for_window(window_id)?;

            // Submit buffer to Wayland
            self.wayland.submit_buffer(surface_id, buffer).await?;
        }
        Ok(())
    }

    /// Execute JavaScript in window context
    pub async fn execute_javascript(&self, window_id: u32, script: &str) -> Result<String> {
        let gecko_lock = self.gecko.read();
        if let Some(gecko) = gecko_lock.as_ref() {
            gecko.execute_javascript(window_id, script).await
        } else {
            Err(anyhow::anyhow!("Gecko instance not available"))
        }
    }

    /// Get system API interface
    pub fn system_api(&self) -> Arc<SystemAPI> {
        self.system_api.clone()
    }

    /// Get Gonk hardware interface
    pub fn gonk(&self) -> Arc<GonkInterface> {
        self.gonk.clone()
    }

    /// List all active windows
    pub fn list_windows(&self) -> Vec<WindowInfo> {
        self.surfaces.list_all_windows()
    }

    /// Get information about a window
    pub fn get_window_info(&self, window_id: u32) -> Option<WindowInfo> {
        self.surfaces.get_window_info(window_id)
    }
}

/// Information about a Gecko application window
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub window_id: u32,
    pub surface_id: u32,
    pub app_id: String,
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub is_visible: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_integration_init() {
        // Note: Requires actual Wayland compositor and Gecko runtime
        // Skip in CI without graphics
        if std::env::var("DISPLAY").is_err() {
            return;
        }
    }
}
