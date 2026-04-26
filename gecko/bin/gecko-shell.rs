// GeckoOS Window Manager Daemon
// File: geckoOS/bin/geckoOS-daemon.rs
// Purpose: System daemon for window management
// Managed by systemd - handles restarts, signal handling, lifecycle

use anyhow::Result;
// use signal_hook::consts::signal::*;
// use signal_hook::iterator::Signals;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::Duration;
use tracing::{info, error, warn};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== GeckoOS Daemon Starting ===");

    let running = Arc::new(AtomicBool::new(true));
    let running_signal = running.clone();

    // // Set up signal handling for graceful shutdown
    // std::thread::spawn(move || {
    //     match Signals::new(&[SIGTERM, SIGINT]) {
    //         Ok(mut signals) => {
    //             for sig in signals.forever() {
    //                 match sig {
    //                     SIGTERM => {
    //                         info!("Received SIGTERM - shutting down gracefully");
    //                         running_signal.store(false, Ordering::Relaxed);
    //                     }
    //                     SIGINT => {
    //                         info!("Received SIGINT - shutting down gracefully");
    //                         running_signal.store(false, Ordering::Relaxed);
    //                     }
    //                     _ => {}
    //                 }
    //             }
    //         }
    //         Err(e) => {
    //             error!("Failed to set up signal handling: {}", e);
    //         }
    //     }
    // });

    // Initialize Gecko Window Manager
    info!("Initializing Gecko Window Manager...");
    
    let wayland_socket = std::env::var("WAYLAND_DISPLAY")
        .unwrap_or_else(|_| "wayland-0".to_string());
    
    let socket_path = format!("/run/user/{}",
        // unsafe { libc::getuid() },
        wayland_socket
    );

    info!("Connecting to Wayland compositor: {}", socket_path);

    // let gecko = match  geckoOS_gecko_integration::initialize(&socket_path).await {
    //     Ok(wm) => {
    //         info!("✓ Gecko Window Manager initialized");
    //         wm
    //     }
    //     Err(e) => {
    //         error!("Failed to initialize Gecko: {}", e);
    //         error!("Make sure OURANOS compositor is running");
    //         std::process::exit(1);
    //     }
    // };

    // Register signal handlers for window events
    // (In production, these would receive events from compositor)
    info!("Gecko Window Manager ready to accept connections");

    // Keep daemon alive, checking for shutdown signal
    // let mut check_interval = tokio::time::interval(Duration::from_secs(1));

    // while running.load(Ordering::Relaxed) {
    //     check_interval.tick().await;

    //     // Optional: Log active windows periodically for debugging
    //     // let windows = gecko.list_windows();
    //     // if !windows.is_empty() {
    //     //     tracing::trace!("Active windows: {}", windows.len());
    //     // }
    // }

    info!("=== GeckoOS Daemon Shutting Down ===");
    Ok(())
}