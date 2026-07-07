//! Shift Daemon - Background Process Management Service
//!
//! This daemon runs continuously and manages all application processes
//! in the GeckoOS system.

use std::error::Error;
use std::sync::Arc;
use tokio::signal;
use tracing::{info, error, warn};
use geckoOS_shift::Shift;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 Shift Process Manager Daemon v0.1.0 starting");

    // Create Shift process manager
    let shift = match Shift::new().await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            error!("Failed to initialize Shift: {}", e);
            return Err(e.into());
        }
    };

    info!("✓ Shift process manager initialized");

    // Register with Echo IPC
    if let Err(e) = shift.register_echo().await {
        warn!("Failed to register with ECHO IPC: {}", e);
    } else {
        info!("✓ Registered with ECHO IPC");
    }

    // Start background tasks
    start_resource_monitoring(shift.clone());
    start_signal_handling(shift.clone());

    info!("✓ Shift daemon ready");
    info!("✓ Listening for process requests");

    // Wait for shutdown signal
    shutdown_signal().await;

    info!("🛑 Shift daemon shutting down");
    cleanup(&shift).await?;

    Ok(())
}

fn start_resource_monitoring(shift: Arc<Shift>) {
    tokio::spawn(async move {
        info!("Resource monitoring started");
        
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            
            match shift.update_resource_usage().await {
                Ok(_) => {
                    let usage = shift.get_all_resource_usage();
                    
                    for proc_usage in usage {
                        if proc_usage.cpu_percent > 80.0 {
                            warn!("High CPU usage for PID {}: {:.1}%", 
                                proc_usage.pid, proc_usage.cpu_percent);
                        }
                        if proc_usage.memory_mb > 512 {
                            warn!("High memory usage for PID {}: {} MB", 
                                proc_usage.pid, proc_usage.memory_mb);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to update resource usage: {}", e);
                }
            }
        }
    });
}

fn start_signal_handling(shift: Arc<Shift>) {
    tokio::spawn(async move {
        info!("Signal handling started");
        
        if let Err(e) = shift.handle_signals().await {
            error!("Signal handler error: {}", e);
        }
    });
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install CTRL+C signal handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received CTRL+C signal");
        }
        _ = terminate => {
            info!("Received SIGTERM signal");
        }
    }
}

async fn cleanup(shift: &Arc<Shift>) -> Result<(), Box<dyn Error>> {
    info!("Cleaning up resources...");

    // Gracefully terminate all managed processes
    if let Err(e) = shift.shutdown_all().await {
        error!("Error during process shutdown: {}", e);
    }

    info!("✓ All processes terminated");
    info!("✓ Resources cleaned up");
    Ok(())
}
