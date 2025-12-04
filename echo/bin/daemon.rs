//! Echo IPC Daemon - Message Bus Service
//!
//! This daemon runs continuously and manages IPC communication
//! between all GeckoOS system components.

use std::error::Error;
use std::sync::Arc;
use tokio::signal;
use tracing::{info, error, warn, debug};
use geckoOS_echo::{IPCBus, Message, MessageType, UnixSocketTransport};
use std::path::PathBuf;
use tokio::net::UnixStream;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🔗 Echo IPC Bus v0.1.0 starting");

    // Determine socket path
    let socket_path = get_socket_path();
    info!("Socket path: {:?}", socket_path);

    // Create Echo IPC bus
    let bus = match IPCBus::new(socket_path).await {
        Ok(b) => Arc::new(b),
        Err(e) => {
            error!("Failed to initialize Echo IPC bus: {}", e);
            return Err(e.into());
        }
    };

    // Start the bus
    if let Err(e) = bus.start().await {
        error!("Failed to start Echo IPC bus: {}", e);
        return Err(e.into());
    }

    info!("✓ Echo IPC bus initialized");
    info!("✓ Service registry ready");
    info!("✓ Message router ready");
    info!("✓ Signal bus ready");

    // Register internal Echo service handlers
    register_internal_handlers(&bus).await?;

    // Start connection handler
    start_connection_handler(bus.clone());

    // Start statistics reporter
    start_statistics_reporter(bus.clone());

    info!("✓ Echo IPC bus ready");
    info!("✓ Listening for service connections");

    // Print startup info
    print_startup_info();

    // Wait for shutdown signal
    shutdown_signal().await;

    info!("🛑 Echo IPC bus shutting down");
    cleanup(&bus).await?;

    Ok(())
}

fn get_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let path = PathBuf::from(runtime_dir).join("geckoOS");
        std::fs::create_dir_all(&path).ok();
        path.join("echo.sock")
    } else {
        let path = PathBuf::from("/tmp/geckoOS");
        std::fs::create_dir_all(&path).ok();
        path.join("echo.sock")
    }
}

async fn register_internal_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn Error>> {
    info!("Registering internal Echo handlers");

    let bus_clone = bus.clone();

    // Handler for service registration
    bus.register_handler(
        "org.geckoOS.Echo",
        "RegisterService",
        Box::new(move |msg| {
            let bus = bus_clone.clone();
            let rt = tokio::runtime::Handle::current();
            
            rt.block_on(async {
                let name = msg.body.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing service name"))?;
                
                let interface: geckoOS_echo::ServiceInterface = serde_json::from_value(
                    msg.body.get("interface").unwrap_or(&serde_json::json!({})).clone()
                )?;
                
                let service_id = bus.register_service(name, interface).await?;
                
                Message::method_return(
                    &msg.header.message_id,
                    serde_json::json!({
                        "service_id": service_id,
                        "status": "registered"
                    }),
                )
            })
        }),
    );

    let bus_clone = bus.clone();

    // Handler for listing services
    bus.register_handler(
        "org.geckoOS.Echo",
        "ListServices",
        Box::new(move |msg| {
            let bus = bus_clone.clone();
            let rt = tokio::runtime::Handle::current();
            
            rt.block_on(async {
                let services = bus.list_services().await?;
                
                let service_list: Vec<_> = services.iter().map(|s| {
                    serde_json::json!({
                        "service_id": s.service_id,
                        "service_name": s.service_name,
                        "registered_at": s.registered_at,
                    })
                }).collect();
                
                Message::method_return(
                    &msg.header.message_id,
                    serde_json::json!({
                        "services": service_list,
                        "count": services.len()
                    }),
                )
            })
        }),
    );

    let bus_clone = bus.clone();

    // Handler for getting service info
    bus.register_handler(
        "org.geckoOS.Echo",
        "GetServiceInfo",
        Box::new(move |msg| {
            let bus = bus_clone.clone();
            let rt = tokio::runtime::Handle::current();
            
            rt.block_on(async {
                let service_name = msg.body.get("service_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing service_name"))?;
                
                let service = bus.get_service(service_name).await?;
                
                Message::method_return(
                    &msg.header.message_id,
                    serde_json::json!({
                        "service_id": service.service_id,
                        "service_name": service.service_name,
                        "registered_at": service.registered_at,
                        "interface": service.interface,
                    }),
                )
            })
        }),
    );

    info!("✓ Internal handlers registered");
    Ok(())
}

fn start_connection_handler(bus: Arc<IPCBus>) {
    tokio::spawn(async move {
        info!("Connection handler started");
        
        loop {
            match bus.accept_connection().await {
                Ok((connection_id, stream)) => {
                    debug!("New connection: {}", connection_id);
                    
                    let bus_clone = bus.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(bus_clone.clone(), connection_id.clone(), stream).await {
                            error!("Connection {} error: {}", connection_id, e);
                        }
                        
                        // Cleanup connection
                        bus_clone.remove_connection(&connection_id);
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        }
    });
}

async fn handle_connection(
    bus: Arc<IPCBus>,
    connection_id: String,
    mut stream: UnixStream,
) -> Result<(), Box<dyn Error>> {
    debug!("Handling connection: {}", connection_id);
    
    loop {
        // Receive message
        let data = match UnixSocketTransport::recv_message(&mut stream).await {
            Ok(d) => d,
            Err(e) => {
                debug!("Connection {} closed: {}", connection_id, e);
                break;
            }
        };
        
        // Deserialize message
        let mut message: Message = serde_json::from_slice(&data)?;
        message.header.sender = Some(connection_id.clone());
        
        debug!(
            "Received message from {}: type={:?}, dest={}",
            connection_id, message.header.message_type, message.header.destination
        );
        
        match message.header.message_type {
            MessageType::Signal => {
                // Broadcast signal
                bus.signal_bus().broadcast(message).await?;
            }
            MessageType::MethodCall => {
                // Route method call
                let response = bus.send_message(message).await?;
                
                // Send response back
                let response_data = serde_json::to_vec(&response)?;
                UnixSocketTransport::send_message(&mut stream, &response_data).await?;
            }
            MessageType::MethodReturn | MessageType::Error => {
                // This shouldn't happen from client, but handle it
                warn!("Unexpected message type from client: {:?}", message.header.message_type);
            }
        }
    }
    
    Ok(())
}

fn start_statistics_reporter(bus: Arc<IPCBus>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            
            let connections = bus.connection_count();
            let services = bus.service_count().await.unwrap_or(0);
            
            info!(
                "Echo IPC Stats - Connections: {}, Services: {}",
                connections, services
            );
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

async fn cleanup(bus: &Arc<IPCBus>) -> Result<(), Box<dyn Error>> {
    info!("Cleaning up resources...");
    
    // Emit shutdown signal
    if let Err(e) = bus.emit_signal(
        "org.geckoOS.Echo",
        "Shutdown",
        serde_json::json!({"reason": "daemon_shutdown"}),
    ).await {
        warn!("Failed to emit shutdown signal: {}", e);
    }
    
    info!("✓ Resources cleaned up");
    Ok(())
}

fn print_startup_info() {
    println!();
    println!("╔════════════════════════════════════════╗");
    println!("║     Echo IPC Bus - Running             ║");
    println!("╠════════════════════════════════════════╣");
    println!("║ Socket: $XDG_RUNTIME_DIR/geckoOS/     ║");
    println!("║         echo.sock                      ║");
    println!("║ Max connections: 256                   ║");
    println!("║ D-Bus compatible: Yes                  ║");
    println!("╚════════════════════════════════════════╝");
    println!();
    println!("Ready to accept service connections:");
    println!("  • org.geckoOS.Gecko (Rendering Engine)");
    println!("  • org.geckoOS.Cronus (Process Manager)");
    println!("  • org.geckoOS.Gaia (Shell UI)");
    println!("  • org.geckoOS.Persephone (Linux Bridge)");
    println!("  • org.geckoOS.Kratos (System Services)");
    println!();
}
