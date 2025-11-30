// Cronus Daemon - Background Service
// File: cronus/bin/daemon.rs
// Purpose: Long-running process manager daemon

use std::error::Error;
use std::sync::Arc;
use tokio::signal;
use tracing::{info, error};
use cronus::process_manager::ProcessManager;
use cronus::resource_monitor::ResourceMonitor;
use cronus::scheduler::ProcessScheduler;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    info!("🚀 Cronus Process Manager Daemon v2.0.0 starting");
    
    // Create components
    let process_manager = Arc::new(ProcessManager::new()?);
    let resource_monitor = Arc::new(ResourceMonitor::new()?);
    let scheduler = Arc::new(ProcessScheduler::new()?);
    
    info!("✓ Process manager initialized");
    info!("✓ Resource monitor initialized");
    info!("✓ Scheduler initialized");
    
    // Connect to Echo IPC
    connect_to_echo(&process_manager).await?;
    
    // Start background tasks
    start_monitoring(resource_monitor.clone());
    start_scheduling(scheduler.clone());
    
    info!("✓ Cronus daemon ready");
    info!("✓ Listening for process requests");
    
    // Wait for shutdown signal
    shutdown_signal().await?;
    
    info!("🛑 Cronus daemon shutting down");
    cleanup(&process_manager).await?;
    
    Ok(())
}

async fn connect_to_echo(pm: &Arc<ProcessManager>) -> Result<(), Box<dyn Error>> {
    info!("Connecting to Echo IPC bus...");
    
    // Register service with Echo
    pm.register_with_echo(
        "org.geckoOS.Cronus",
        &[
            "createProcess",
            "terminateProcess",
            "getProcessInfo",
            "listProcesses",
            "setResourceLimit",
            "getResourceUsage",
        ],
    ).await?;
    
    info!("✓ Registered with Echo IPC");
    Ok(())
}

fn start_monitoring(monitor: Arc<ResourceMonitor>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            
            match monitor.collect_metrics() {
                Ok(metrics) => {
                    info!(
                        "Metrics - CPU: {:.1}%, Memory: {:.1}%, Processes: {}",
                        metrics.cpu_percent, metrics.memory_percent, metrics.process_count
                    );
                    
                    // Check for resource warnings
                    if metrics.cpu_percent > 85.0 {
                        error!("⚠️  High CPU usage: {:.1}%", metrics.cpu_percent);
                    }
                    if metrics.memory_percent > 85.0 {
                        error!("⚠️  High memory usage: {:.1}%", metrics.memory_percent);
                    }
                }
                Err(e) => error!("Failed to collect metrics: {}", e),
            }
        }
    });
}

fn start_scheduling(scheduler: Arc<ProcessScheduler>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            
            match scheduler.schedule() {
                Ok(scheduled_count) => {
                    if scheduled_count > 0 {
                        info!("Scheduled {} processes", scheduled_count);
                    }
                }
                Err(e) => error!("Scheduling error: {}", e),
            }
        }
    });
}

async fn shutdown_signal() -> Result<(), Box<dyn Error>> {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install CTRL+C signal handler");
    };
    
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM signal handler")
            .recv()
            .await;
    };
    
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

async fn cleanup(pm: &Arc<ProcessManager>) -> Result<(), Box<dyn Error>> {
    info!("Cleaning up resources...");
    
    // Gracefully terminate all managed processes
    pm.shutdown_all().await?;
    
    info!("✓ All processes terminated");
    info!("✓ Resources cleaned up");
    
    Ok(())
}
