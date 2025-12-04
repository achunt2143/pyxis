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

pub mod backend;
pub mod config;
pub mod damage_tracker;
pub mod data_device;
pub mod decorations;
pub mod gecko_surface;
pub mod hotplug;
pub mod input_advanced;
pub mod state;
pub mod window;

pub use config::Config;
pub use state::GeckoOSState;
pub use window::{WindowState, WindowGeometry, WindowMetadata};

use anyhow::Result;
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use smithay::reexports::calloop::EventLoop;
use tracing::info;

/// Initialize and run the GeckoOS Wayland compositor
///
/// # Arguments
/// * `config` - Compositor configuration
///
/// # Returns
/// Result containing the compositor state, event loop, and display
///
/// # Example
/// ```
/// use geckoos::initialize;
/// use geckoos::Config;
///
/// let config = Config::default();
/// let (mut state, event_loop, display) = initialize(config)?;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn initialize(config: Config) -> Result<(GeckoOSState, EventLoop<'static, GeckoOSState>, Display<GeckoOSState>)> {
    info!("Initializing GeckoOS Wayland Compositor");
    
    let (state, event_loop, display) = GeckoOSState::new()?;
    
    info!("Compositor initialized successfully");
    info!("Wayland socket: {}", state.socket_name());
    
    Ok((state, event_loop, display))
}

/// Get the socket name for the Wayland display
pub fn get_socket_name(state: &GeckoOSState) -> &str {
    state.socket_name()
}

/// Shutdown the compositor gracefully
///
/// # Arguments
/// * `state` - Mutable reference to the compositor state
pub fn shutdown(state: &mut GeckoOSState) {
    info!("Shutting down GeckoOS Wayland Compositor");
    state.stop();
    info!("Compositor shutdown complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.display.default_width > 0);
        assert!(config.display.default_height > 0);
    }
}
