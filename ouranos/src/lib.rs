//! GeckoOS Wayland Compositor
//!
//! A production-ready Wayland compositor built with Smithay, providing:
//! - Advanced Wayland protocol support (XDG Shell, Layer Shell)
//! - Server-side window decorations
//! - Advanced input handling (pointer, keyboard, touch)
//! - Hot-plug support for monitors and input devices
//! - Damage tracking and incremental rendering
//! - Data device support (clipboard, drag-drop)

#![warn(missing_docs)]

pub mod compositor;
pub mod config;
pub mod data_device;
pub mod hotplug;
pub mod input;
pub mod rendering;
pub mod shell;
pub mod utils;
pub mod gecko_surface;

use anyhow::Result;
use std::sync::{Arc, Mutex};
use tracing::{error, info};

pub use compositor::CompositorState;
pub use config::Config;
pub use input::InputManager;
pub use rendering::RenderingEngine;
pub use shell::ShellManager;
pub use gecko_surface::{GeckoSurfaceManager, GeckoSurface};

/// Global compositor instance
static COMPOSITOR: once_cell::sync::OnceCell<Arc<Mutex<CompositorState>>> =
    once_cell::sync::OnceCell::new();

/// Initialize the Wayland compositor
///
/// # Arguments
/// * `config` - Compositor configuration
///
/// # Returns
/// Result containing the initialized compositor state
pub fn initialize_compositor(config: Config) -> Result<Arc<Mutex<CompositorState>>> {
    info!("Initializing GeckoOS Wayland Compositor");
    
    let compositor = Arc::new(Mutex::new(CompositorState::new(config)?));
    
    if let Err(_) = COMPOSITOR.set(compositor.clone()) {
        error!("Compositor already initialized");
        anyhow::bail!("Compositor already initialized");
    }
    
    info!("Compositor initialized successfully");
    Ok(compositor)
}

/// Get the global compositor instance
pub fn get_compositor() -> Option<Arc<Mutex<CompositorState>>> {
    COMPOSITOR.get().cloned()
}

/// Shutdown the compositor
pub fn shutdown_compositor() -> Result<()> {
    info!("Shutting down Wayland Compositor");
    
    if let Some(compositor) = COMPOSITOR.get() {
        let mut comp = compositor.lock().unwrap();
        comp.shutdown()?;
    }
    
    info!("Compositor shutdown complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compositor_initialization() {
        let config = Config::default();
        let result = initialize_compositor(config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_compositor() {
        let comp = get_compositor();
        assert!(comp.is_some());
    }
}
