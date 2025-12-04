// gecko/src/window_manager.rs
//! Central window manager orchestrator
//!
//! Manages:
//! - Wayland surface lifecycle
//! - Window hierarchy and Z-order
//! - Input event routing
//! - Communication between Gaia (JavaScript) and native apps

use crate::input_event_router::{InputEventRouter, WindowGeometry};
use crate::surface::SurfaceConfig;
use crate::surface_manager::SurfaceManager;
use crate::wayland_client::WaylandClient;
use crate::window::{Window, WindowConfig, WindowState};

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Central window manager for Gecko
pub struct WindowManager {
    wayland: Arc<WaylandClient>,
    surfaces: Arc<SurfaceManager>,
    windows: Arc<DashMap<u32, Arc<Window>>>,
    input_router: Arc<InputEventRouter>,
    next_window_id: Arc<RwLock<u32>>,
}

impl WindowManager {
    /// Create new window manager
    pub fn new(wayland: Arc<WaylandClient>) -> Self {
        info!("Creating WindowManager");
        WindowManager {
            wayland,
            surfaces: Arc::new(SurfaceManager::new()),
            windows: Arc::new(DashMap::new()),
            input_router: Arc::new(InputEventRouter::new(0, 0)),
            next_window_id: Arc::new(RwLock::new(1)),
        }
    }

    /// Create new window for Gaia JavaScript app
    ///
    /// # Arguments
    /// - `app_id`: Gaia application identifier
    /// - `url`: Application URL (file:// or http://)
    /// - `width`, `height`: Initial window dimensions
    pub async fn create_gaia_window(
        &self,
        app_id: impl Into<String>,
        url: impl Into<String>,
        width: u32,
        height: u32,
    ) -> Result<u32> {
        let app_id = app_id.into();
        let url = url.into();

        info!(
            "Creating Gaia window: app_id={}, {}x{}",
            app_id, width, height
        );

        // Get next window ID
        let mut id_guard = self.next_window_id.write().await;
        let window_id = *id_guard;
        *id_guard += 1;
        drop(id_guard);

        // Create Wayland surface
        let surface_id = self.wayland.create_surface(width, height).unwrap();
        debug!("Created Wayland surface: {}", surface_id);

        // Configure surface
        // self.wayland
        //     .configure_surface(surface_id, width, height)
        //     .await?;

        // Create window model
        let window = Arc::new(Window::new(
            window_id,
            surface_id,
            WindowConfig {
                app_id: app_id.clone(),
                app_type: crate::window::AppType::Gaia,
                url: url.clone(),
                width,
                height,
            },
        ));

        let geometry = WindowGeometry {
            window_id,
            app_id: app_id.to_string(),
            x: 0,              // ✅ Add window X position
            y: 0,              // ✅ Add window Y position
            width,             // ✅ Add window width
            height,            // ✅ Add window height
            is_visible: true,  // ✅ Initially visible
            z_order: 1,        // ✅ Initial z-order
            is_focused: true,  // ✅ Initially focused
            is_grabbed: false, // ✅ Not grabbed initially
        };

        // Register window
        self.windows.insert(window_id, window.clone());
        self.surfaces
            .map_window_to_surface(window_id, surface_id, width, height)?;
        self.input_router.register_window(geometry);

        info!(
            "Gaia window created: window_id={}, surface_id={}",
            window_id, surface_id
        );

        Ok(window_id)
    }

    /// Create new window for native Wayland app
    ///
    /// # Arguments
    /// - `app_id`: Application identifier
    /// - `width`, `height`: Initial window dimensions
    pub async fn create_native_window(
        &self,
        app_id: impl Into<String>,
        width: u32,
        height: u32,
    ) -> Result<u32> {
        let app_id = app_id.into();

        info!(
            "Creating native Wayland window: app_id={}, {}x{}",
            app_id, width, height
        );

        let mut id_guard = self.next_window_id.write().await;
        let window_id = *id_guard;
        *id_guard += 1;
        drop(id_guard);

        let surface_id = self.wayland.create_surface(width, height).unwrap();
        debug!("Created Wayland surface for native app: {}", surface_id);

        // self.wayland
        //     .configure_surface(surface_id, width, height)
        //     .await?;

        let window = Arc::new(Window::new(
            window_id,
            surface_id,
            WindowConfig {
                app_id: app_id.clone(),
                app_type: crate::window::AppType::Native,
                url: String::new(),
                width,
                height,
            },
        ));

        let geometry = WindowGeometry {
            window_id,
            app_id: app_id.to_string(),
            x: 0,              // ✅ Add window X position
            y: 0,              // ✅ Add window Y position
            width,             // ✅ Add window width
            height,            // ✅ Add window height
            is_visible: true,  // ✅ Initially visible
            z_order: 1,        // ✅ Initial z-order
            is_focused: true,  // ✅ Initially focused
            is_grabbed: false, // ✅ Not grabbed initially
        };

        self.windows.insert(window_id, window.clone());
        self.surfaces
            .map_window_to_surface(window_id, surface_id, width, height)?;
        self.input_router.register_window(geometry);

        info!(
            "Native window created: window_id={}, surface_id={}",
            window_id, surface_id
        );

        Ok(window_id)
    }

    /// Close and destroy window
    pub async fn destroy_window(&self, window_id: u32) -> Result<()> {
        info!("Destroying window: {}", window_id);

        if let Some((_, window)) = self.windows.remove(&window_id) {
            let surface_id = window.surface_id();

            // Destroy Wayland surface
            self.wayland.destroy_surface(surface_id);
            debug!("Destroyed Wayland surface: {}", surface_id);

            // Unmap in surface manager
            self.surfaces.unmap_window(window_id)?;

            // Unregister from input router
            self.input_router.unregister_window(window_id);

            info!("Window destroyed: {}", window_id);
            Ok(())
        } else {
            Err(anyhow!("Window {} not found", window_id))
        }
    }

    /// Get window by ID
    pub fn get_window(&self, window_id: u32) -> Option<Arc<Window>> {
        self.windows.get(&window_id).map(|r| r.clone())
    }

    /// Get all windows
    pub fn list_windows(&self) -> Vec<Arc<Window>> {
        self.windows.iter().map(|r| r.clone()).collect()
    }

    /// Show/raise window to front
    pub async fn raise_window(&self, window_id: u32) -> Result<()> {
        let window = self
            .get_window(window_id)
            .ok_or_else(|| anyhow!("Window {} not found", window_id))?;

        debug!("Raising window: {}", window_id);

        let surface_id = window.surface_id();
        // self.wayland.show_surface(surface_id).await?;

        window.set_state(WindowState::Focused);

        Ok(())
    }

    /// Hide window
    pub async fn hide_window(&self, window_id: u32) -> Result<()> {
        let window = self
            .get_window(window_id)
            .ok_or_else(|| anyhow!("Window {} not found", window_id))?;

        debug!("Hiding window: {}", window_id);

        let surface_id = window.surface_id();
        // self.wayland.hide_surface(surface_id).await?;

        window.set_state(WindowState::Hidden);

        Ok(())
    }

    /// Resize window
    pub async fn resize_window(&self, window_id: u32, width: u32, height: u32) -> Result<()> {
        let window = self
            .get_window(window_id)
            .ok_or_else(|| anyhow!("Window {} not found", window_id))?;

        debug!("Resizing window: {}x{}", width, height);

        let surface_id = window.surface_id();
        self.wayland
            .resize_surface(surface_id, width, height);

        window.set_size(width, height);
        self.surfaces.update_window_size(window_id, width, height)?;

        Ok(())
    }

    /// Submit rendered buffer from Gecko to Wayland compositor
    pub async fn submit_buffer(&self, window_id: u32, buffer: Vec<u8>) -> Result<()> {
        let window = self
            .get_window(window_id)
            .ok_or_else(|| anyhow!("Window {} not found", window_id))?;

        let surface_id = window.surface_id();
        let (width, height) = window.size();

        self.wayland
            .submit_buffer(surface_id, buffer, width, height)
            .await?;

        Ok(())
    }

    /// Handle input event from compositor
    ///
    /// Routes to correct window/app based on surface coordinates
    pub fn handle_input_event(&self, event: crate::InputEvent) -> Result<()> {
        debug!("Handling input event: {:?}", event);

        // Router determines target window
        let target_windows = self.input_router.route_event(&event);

        for window_id in target_windows {
            if let Some(window) = self.get_window(window_id) {
                debug!("Routing input to window: {}", window_id);

                // Send event to window - implementation specific to app type
                match window.app_type() {
                    crate::window::AppType::Gaia => {
                        // Route to Gaia via IPC - event will be dispatched to JavaScript
                        debug!("Routing event to Gaia app: {}", window.app_id());
                    }
                    crate::window::AppType::Native => {
                        // Route to native app via Wayland pointer/keyboard events
                        debug!("Routing event to native app: {}", window.app_id());
                    }
                }
            }
        }

        Ok(())
    }

    /// Access to Wayland client (for advanced operations)
    pub fn wayland(&self) -> Arc<WaylandClient> {
        self.wayland.clone()
    }

    /// Access to surface manager
    pub fn surfaces(&self) -> Arc<SurfaceManager> {
        self.surfaces.clone()
    }

    /// Access to input router
    pub fn input_router(&self) -> Arc<InputEventRouter> {
        self.input_router.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #[test]
    // fn test_window_manager_creation() {
    //     let wayland = Arc::new(WaylandClient::mock());
    //     let manager = WindowManager::new(wayland);
    //     assert_eq!(manager.list_windows().len(), 0);
    // }

    // #[tokio::test]
    // async fn test_create_gaia_window() {
    //     let wayland = Arc::new(WaylandClient::mock());
    //     let manager = WindowManager::new(wayland);

    //     let window_id = manager
    //         .create_gaia_window("test.app", "file:///apps/test/index.html", 800, 600)
    //         .await
    //         .unwrap();

    //     assert_eq!(window_id, 1);
    //     assert!(manager.get_window(window_id).is_some());
    // }

    // #[tokio::test]
    // async fn test_create_multiple_windows() {
    //     let wayland = Arc::new(WaylandClient::mock());
    //     let manager = WindowManager::new(wayland);

    //     let w1 = manager
    //         .create_gaia_window("app1", "file:///apps/app1/index.html", 800, 600)
    //         .await
    //         .unwrap();

    //     let w2 = manager
    //         .create_native_window("app2", 1024, 768)
    //         .await
    //         .unwrap();

    //     assert_eq!(manager.list_windows().len(), 2);
    //     assert_ne!(w1, w2);
    // }

    // #[tokio::test]
    // async fn test_destroy_window() {
    //     let wayland = Arc::new(WaylandClient::mock());
    //     let manager = WindowManager::new(wayland);

    //     let window_id = manager
    //         .create_gaia_window("test.app", "file:///apps/test/index.html", 800, 600)
    //         .await
    //         .unwrap();

    //     manager.destroy_window(window_id).await.unwrap();
    //     assert!(manager.get_window(window_id).is_none());
    // }
}
