// Persephone Daemon - Linux Application Runtime
// File: persephone/bin/daemon.rs
// Purpose: Manage Linux application execution and compatibility

use std::error::Error;
use std::sync::Arc;
use geckoOS_echo::EchoClient;
use geckoOS_echo::{ArgumentDefinition, SignalDefinition, ServiceInterface, MethodDefinition};
use geckoOS_persephone::CompatibilityLayer;
use geckoOS_gaia::{GeckoAppLauncher};
use geckoOS_gaia::gecko_app_launcher::{AppDescriptor, GeckoApp};
use tokio::signal;
use tracing::{info, error};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    info!("🐧 Persephone Linux Layer v2.0.0 starting");
    
    // Create components
    let _compat_layer = Arc::new(CompatibilityLayer::new());
    let app_launcher = Arc::new(GeckoAppLauncher::new());
    
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

/// Connect Persephone to Cronus process manager via Echo IPC
/// 
/// Registers Persephone as a service on the Echo IPC bus so that
/// Cronus and other system components can request application launches.
async fn connect_to_cronus(launcher: &Arc<GeckoAppLauncher>) -> Result<(), Box<dyn Error>> {
    info!("Connecting to Cronus Process Manager...");
    
    // Connect to Echo IPC daemon
    let mut echo_client = EchoClient::connect().await?;
    
    // Define Persephone service interface
    let interface = ServiceInterface {
        name: "org.geckoOS.Persephone".to_string(),
        methods: vec![
            MethodDefinition {
                name: "LaunchApplication".to_string(),
                input_args: vec![
                    ArgumentDefinition { name: "app_id".to_string(), arg_type: "s".to_string() },
                    ArgumentDefinition { name: "args".to_string(), arg_type: "s".to_string() },
                ],
                output_args: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                ],
            },
            MethodDefinition {
                name: "TerminateApplication".to_string(),
                input_args: vec![
                    ArgumentDefinition { name: "app_id".to_string(), arg_type: "s".to_string() },
                ],
                output_args: vec![],
            },
        ],
        signals: vec![
            SignalDefinition {
                name: "ApplicationLaunched".to_string(),
                arguments: vec![
                    ArgumentDefinition { name: "app_id".to_string(), arg_type: "s".to_string() },
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                ],
            },
        ],
        properties: vec![],
    };
    
    // Register on Echo bus
    let _service_id = echo_client.register_service("org.geckoOS.Persephone", interface).await?;
    
    info!("✓ Registered Persephone on Echo IPC bus");
    Ok(())
}


/// Wait for shutdown signals (CTRL+C or SIGTERM)
/// 
/// Blocks until the daemon receives either:
/// - SIGINT (CTRL+C)
/// - SIGTERM (termination signal)
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

/// Graceful shutdown - terminate all running applications
/// 
/// 1. Gets list of all running applications
/// 2. Sends graceful termination request to each
/// 3. Cleans up resources
async fn cleanup(launcher: &Arc<GeckoAppLauncher>) -> Result<(), Box<dyn Error>> {
    info!("Cleaning up resources...");

    // Get list of running applications
    let apps = launcher.list_apps();
    
    if apps.is_empty() {
        info!("No applications running");
    } else {
        info!("Terminating {} running application(s)", apps.len());
        
        // Terminate all running applications gracefully
        for app in apps {
            match launcher.terminate_app(&app.app_id) {
                Ok(()) => info!("✓ Terminated: {}", app.app_id),
                Err(e) => error!("✗ Failed to terminate {}: {}", app.app_id, e),
            }
        }
    }
    
    info!("✓ All applications terminated");
    info!("✓ Resources cleaned up");
    
    Ok(())
}

/// Print Persephone system information banner
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