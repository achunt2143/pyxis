mod backend;
mod input;
mod render;
mod shell;
mod state;
mod window;
mod input_advanced;
mod decorations;
mod hotplug;
mod damage_tracker;
mod data_device;
mod config;


use anyhow::Result;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

pub use state::{GeckoOSState, WindowElement};
pub use window::WindowState;
pub use input_advanced::*;
pub use decorations::*;
pub use hotplug::*;
pub use damage_tracker::*;
pub use data_device::*;
pub use config::*;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!("GeckoOS Compositor starting...");

    // Initialize compositor state
    let mut state = GeckoOSState::new().await?;
    
    tracing::info!("Compositor initialized successfully");
    tracing::info!("Wayland socket: {}", state.socket_name());

    // Run event loop
    state.run().await?;

    tracing::info!("GeckoOS Compositor shutting down");
    Ok(())
}
