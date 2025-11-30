// Echo IPC Daemon - Message Bus Service
// File: echo/bin/daemon.rs
// Purpose: Long-running Echo IPC message bus

use std::error::Error;
use std::sync::Arc;
use tokio::signal;
use tracing::{info, error, warn};
use echo::echo_daemon::EchoDaemon;
use echo::service_registry::ServiceRegistry;
use echo::message_router::MessageRouter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    info!("🔗 Echo IPC Bus v2.0.0 starting");
    
    // Create core components
    let service_registry = Arc::new(ServiceRegistry::new());
    let message_router = Arc::new(MessageRouter::new());
    let daemon = Arc::new(EchoDaemon::new(
        service_registry.clone(),
        message_router.clone(),
    )?);
    
    info!("✓ Service registry initialized");
    info!("✓ Message router initialized");
    info!("✓ Echo daemon created");
    
    // Start listening for connections
    start_listener(daemon.clone()).await?;
    
    info!("✓ Echo IPC bus ready");
    info!("✓ Listening for service connections");
    
    // Print startup info
    print_startup_info();
    
    // Wait for shutdown signal
    shutdown_signal().await?;
    
    info!("🛑 Echo IPC bus shutting down");
    cleanup(daemon.clone()).await?;
    
    Ok(())
}

async fn start_listener(daemon: Arc<EchoDaemon>) -> Result<(), Box<dyn Error>> {
    tokio::spawn(async move {
        loop {
            match daemon.accept_connection().await {
                Ok(connection) => {
                    info!("✓ New service connected: {}", connection.service_id);
                    
                    let daemon_clone = daemon.clone();
                    tokio::spawn(async move {
                        if let Err(e) = daemon_clone.handle_connection(connection).await {
                            error!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        }
    });
    
    Ok(())
}

async fn shutdown_signal() -> Result<(), Box<dyn Error>> {
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
    
    Ok(())
}

async fn cleanup(daemon: Arc<EchoDaemon>) -> Result<(), Box<dyn Error>> {
    info!("Cleaning up resources...");
    
    // Notify all connected services
    daemon.broadcast_shutdown().await?;
    
    // Close all connections
    daemon.close_all().await?;
    
    info!("✓ All services notified");
    info!("✓ All connections closed");
    
    Ok(())
}

fn print_startup_info() {
    println!();
    println!("╔════════════════════════════════════════╗");
    println!("║     Echo IPC Bus - Active Services     ║");
    println!("╠════════════════════════════════════════╣");
    println!("║ Listening on: /run/user/{}/echo.sock  ║", std::env::var("UID").unwrap_or_default());
    println!("║ Max connections: 256                   ║");
    println!("║ D-Bus compatible: Yes                  ║");
    println!("╚════════════════════════════════════════╝");
    println!();
    println!("Ready to register services. Connect using:");
    println!("  - org.geckoOS.Gecko (Rendering)");
    println!("  - org.geckoOS.Cronus (Process Manager)");
    println!("  - org.geckoOS.Gaia (Shell UI)");
    println!("  - org.geckoOS.Persephone (Linux Support)");
    println!("  - org.geckoOS.Performance (Optimization)");
    println!();
}
