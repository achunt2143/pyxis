// gecko/src/lib.rs
//! GeckoOS Window Manager
//!
//! Core window management for GeckoOS - handles bidirectional communication
//! between Gaia (JavaScript apps) and native Wayland applications.
//!
//! # Architecture
//!
//! - **WindowManager**: Central orchestrator (Wayland client + routing)
//! - **SurfaceManager**: Maps Gecko windows to Wayland surfaces
//! - **InputEventRouter**: Routes events from compositor to correct window/app
//! - **Window & Surface**: Domain models for window hierarchy and rendering

pub mod input_event_router;
pub mod surface;
pub mod surface_manager;
pub mod wayland_client;
pub mod window;
pub mod window_manager;

pub use input_event_router::{InputEvent, InputEventRouter};
pub use surface::{Surface, SurfaceConfig};
pub use surface_manager::SurfaceManager;
pub use wayland_client::WaylandClient;
pub use window::{Window, WindowConfig, WindowState};
pub use window_manager::WindowManager;

use anyhow::Result;
use std::sync::Arc;
use tracing::info;

/// Initialize Gecko window manager
pub async fn initialize(wayland_socket: &str) -> Result<Arc<WindowManager>> {
    info!("Initializing Gecko Window Manager");

    let wayland = WaylandClient::new(0,0);
    info!("Connected to Wayland compositor");

    let window_manager = Arc::new(WindowManager::new(wayland.into()));
    info!("Gecko Window Manager ready");

    Ok(window_manager)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_exports() {
        // Verify all public types are accessible
        let _ = std::any::type_name::<WindowManager>();
        let _ = std::any::type_name::<Window>();
        let _ = std::any::type_name::<Surface>();
        let _ = std::any::type_name::<InputEvent>();
    }
}
