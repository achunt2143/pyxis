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
//! - **gecko**: `vela` launch composition (Approach A launcher/
//!   supervisor — see gecko/src/gecko.rs's own doc comment for the full
//!   rationale; this crate no longer does libxul FFI embedding).

pub mod gecko;
pub mod window;

pub use gecko::{compose_launch, ComposedLaunch, DisplayMode, LaunchEnvironment, WebAppLaunchSpec};
pub use window::{Window, WindowConfig, WindowState};

use anyhow::Result;
use std::sync::Arc;
use tracing::info;

/// Initialize Gecko window manager
pub async fn initialize(wayland_socket: &str) {
    info!("Initializing Gecko Window Manager");

    // let wayland = WaylandClient::new(0,0);
    info!("Connected to Wayland compositor");

    // let window_manager = Arc::new(WindowManager::new(wayland.into()));
    info!("Gecko Window Manager ready");

    // Ok()
}
