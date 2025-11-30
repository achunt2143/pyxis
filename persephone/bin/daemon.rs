// Persephone Daemon - Linux Application Runtime
// File: persephone/bin/daemon.rs
// Purpose: Manage Linux application execution and compatibility

use std::error::Error;
use std::sync::Arc;
use tokio::signal;
use tracing::{info, error};
use persephone::elf_loader::ElfLoader;
use persephone::runtime::Runtime;
use persephone::compatibility::CompatibilityLayer;
use persephone::app_launcher::AppLauncher;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    info!("🐧 Persephone Linux Layer v2.0.0 starting");
    
    // Create components
    let compat_layer = Arc::new(CompatibilityLayer::new());
    let app_launcher = Arc::new(AppLauncher::new(compat_layer.clone())?);
    
    info!("✓ Compatibility layer initialized");
    info!("✓ Application launcher initialized");
    
    // Connect to Cronus
    connect_to_cronus(&app_launcher).await?;
    
    info!("✓ Persephone daemon ready");
    info!("✓ Listening for application launch requests");
    
    // Print system information
    print_system_info();
    
    // Wait for shutdown signal
    shutdown_signal().await?;
    
    info!("🛑 Persephone daemon shutting down");
    cleanup(&app_launcher).await?;
    
    Ok(())
}

async fn connect_to_cronus(launcher: &Arc<AppLauncher>) -> Result<(), Box<dyn Error>> {
    info!("Connecting to Cronus Process Manager...");
    
    // Register with Echo IPC
    launcher.register_with_echo(
        "org.geckoOS.Persephone",
        &[
            "executeApplication",
            "getApplicationInfo",
            "listApplications",
            "getCompatibility",
            "setResourceLimit",
        ],
    ).await?;
    
    info!("✓ Registered with Echo IPC");
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

async fn cleanup(launcher: &Arc<AppLauncher>) -> Result<(), Box<dyn Error>> {
    info!("Cleaning up resources...");
    
    // Terminate all running applications
    launcher.shutdown_all().await?;
    
    info!("✓ All applications terminated");
    info!("✓ Resources cleaned up");
    
    Ok(())
}

fn print_system_info() {
    println!();
    println!("╔═════════════════════════════════════╗");
    println!("║  Persephone Linux Compatibility    ║");
    println!("╠═════════════════════════════════════╣");
    println!("║ Version: 2.0.0                      ║");
    println!("║ Architecture: x86-64                ║");
    println!("║ Kernel: GeckoOS 2.0                 ║");
    println!("║ libc: Compatible                    ║");
    println!("╚═════════════════════════════════════╝");
    println!();
    println!("Supported Application Formats:");
    println!("  • ELF x86-64 executables");
    println!("  • Static and dynamic linking");
    println!("  • Position-independent executables (PIE)");
    println!("  • Shared objects (.so)");
    println!();
    println!("Resource Limits (per application):");
    println!("  • Memory: 512 MB default");
    println!("  • CPU: 100% default");
    println!("  • Open Files: 1024");
    println!("  • Threads: 256");
    println!();
}
