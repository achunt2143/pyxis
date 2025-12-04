//! GeckoOS Ouranos - Wayland Compositor
//!
//! Main entry point for the compositor

use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod backend;
mod config;
mod data_device;
mod damage_tracker;
mod decorations;
mod gecko_surface;
mod hotplug;
mod input_advanced;
mod state;
mod window;

use state::GeckoOSState;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!("╔════════════════════════════════════════╗");
    tracing::info!("║  GeckoOS Ouranos Compositor v0.1.0    ║");
    tracing::info!("╚════════════════════════════════════════╝");

    // Initialize compositor state
    let (mut state, mut event_loop, display) = GeckoOSState::new()?;
    tracing::info!("✓ Compositor initialized");
    tracing::info!("✓ Wayland socket: {}", state.socket_name);

    /// Run event loop
    event_loop.run(
        std::time::Duration::from_millis(16), // timeout (~60 FPS)
        &mut state,
        |_| {}, // callback
    )?;

    tracing::info!("GeckoOS Compositor shutting down");
    Ok(())
}
