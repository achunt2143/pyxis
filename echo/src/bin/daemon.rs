//! Echo IPC Daemon - Message Bus Service - FIXED VERSION
//!
//! Properly handles:
//! - Native host handshake with ACK
//! - Connection registration and keep-alive
//! - Message routing with proper responses

use dashmap::DashMap;
use geckoOS_echo::{
    handler, services, ForwardRequest, IPCBus, Message, MessageType, UnixSocketTransport,
};
use serde_json::json;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::UnixStream;
use tokio::signal;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
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

    // Register all GeckoOS service handlers
    register_all_services(&bus).await?;

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
        handler(move |msg| {
            Box::pin({
                let value = bus_clone.clone();
                async move {
                    let bus = value.clone();

                    let name = msg
                        .body
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing service name"))?;

                    let interface: geckoOS_echo::ServiceInterface = serde_json::from_value(
                        msg.body
                            .get("interface")
                            .unwrap_or(&serde_json::json!({}))
                            .clone(),
                    )?;

                    let service_id = bus.register_service(name, interface).await?;

                    if let Some(sender_id) = &msg.header.sender {
                        bus.register_external_service(name, sender_id).await;
                    } else {
                        warn!("RegisterService for '{}' had no sender", name);
                    }

                    Ok(Message::method_return(
                        msg.header.message_id,
                        serde_json::json!({
                            "service_id": service_id,
                            "status": "registered"
                        }),
                    ))
                }
            })
        }),
    )
    .await;

    let bus_clone = bus.clone();

    // Handler for listing services
    bus.register_handler(
        "org.geckoOS.Echo",
        "ListServices",
        handler(move |msg| {
            Box::pin({
                let value = bus_clone.clone();
                async move {
                    let bus = value.clone();
                    let services = bus.list_services().await?;

                    let service_list: Vec<_> = services
                        .iter()
                        .map(|s| {
                            serde_json::json!({
                                "service_id": s.service_id,
                                "service_name": s.service_name,
                                "registered_at": s.registered_at,
                            })
                        })
                        .collect();

                    Ok(Message::method_return(
                        msg.header.message_id,
                        serde_json::json!({
                            "services": service_list,
                            "count": services.len()
                        }),
                    ))
                }
            })
        }),
    )
    .await;

    let bus_clone = bus.clone();

    // Handler for getting service info
    bus.register_handler(
        "org.geckoOS.Echo",
        "GetServiceInfo",
        handler(move |msg| {
            Box::pin({
                let value = bus_clone.clone();
                async move {
                    let bus = value.clone();
                    let service_name = msg
                        .body
                        .get("service_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing service_name"))?;

                    let service = bus.get_service(service_name).await?;

                    Ok(Message::method_return(
                        msg.header.message_id,
                        serde_json::json!({
                            "service_id": service.service_id,
                            "service_name": service.service_name,
                            "registered_at": service.registered_at,
                            "interface": service.interface,
                        }),
                    ))
                }
            })
        }),
    )
    .await;

    // NEW: Handler for emitting signals
    let bus_clone = bus.clone();
    bus.register_handler(
        "org.geckoOS.Echo",
        "EmitSignal",
        handler(move |msg| {
            Box::pin({
                let value = bus_clone.clone();
                async move {
                    let bus = value.clone();

                    // Extract signal parameters
                    let service_name = msg.header.interface.clone(); // Use interface as service name
                    let signal_name = msg
                        .body
                        .get("signal_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing signal_name"))?;
                    let args = msg.body.get("args").cloned().unwrap_or_else(|| json!({}));

                    // Create signal message
                    let signal = Message::signal(&service_name, signal_name, args)?;

                    // Broadcast signal to in-process subscribers, then push
                    // it out over every connected socket client too (real
                    // fix, 2026-07-07 -- see IPCBus::broadcast_to_all_connections).
                    bus.signal_bus().broadcast(signal.clone()).await?;
                    bus.broadcast_to_all_connections(&signal).await;

                    info!("Signal emitted: {}.{}", service_name, signal_name);

                    Ok(Message::method_return(
                        msg.header.message_id,
                        json!({
                            "status": "signal_emitted",
                            "signal_name": signal_name
                        }),
                    ))
                }
            })
        }),
    )
    .await;

    info!("✓ Internal handlers registered");

    Ok(())
}

fn start_connection_handler(bus: Arc<IPCBus>) {
    tokio::spawn(async move {
        info!("Connection handler started");
        loop {
            match bus.accept_connection().await {
                Ok((connection_id, stream)) => {
                    info!("New connection: {}", connection_id);
                    let bus_clone = bus.clone();

                    tokio::spawn(async move {
                        if let Err(e) =
                            handle_connection(bus_clone.clone(), connection_id.clone(), stream)
                                .await
                        {
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
    info!("Handling connection: {}", connection_id);

    // Channel used by other connections to send requests to THIS connection (e.g. WM)
    let (forward_tx, mut forward_rx) = mpsc::channel::<ForwardRequest>(32);
    bus.add_connection_sender(connection_id.clone(), forward_tx);

    // Fire-and-forget channel used to push SIGNAL messages out over THIS
    // connection's socket (see IPCBus::broadcast_to_all_connections's doc
    // comment in lib.rs -- SignalBus::broadcast() alone never reaches
    // remote socket clients, only in-process Rust subscribers).
    let (broadcast_tx, mut broadcast_rx) = mpsc::channel::<Message>(64);
    bus.add_broadcast_sender(connection_id.clone(), broadcast_tx);

    // Tracks in-flight forwarded calls that are waiting for a reply from THIS socket
    let pending_replies: Arc<DashMap<String, oneshot::Sender<anyhow::Result<Message>>>> =
        Arc::new(DashMap::new());

    // Track if we've completed handshake
    let mut handshake_completed = false;

    loop {
        tokio::select! {
            // CASE 1: Data arriving from the socket (this process -> daemon)
            socket_res = UnixSocketTransport::recv_message(&mut stream) => {
                let data = match socket_res {
                    Ok(d) => d,
                    Err(e) => {
                        info!("Connection {} closed: {}", connection_id, e);
                        break;
                    }
                };

                // Deserialize message
                let mut message: Message = match serde_json::from_slice(&data) {
                    Ok(msg) => msg,
                    Err(e) => {
                        warn!(
                            "Failed to deserialize message from {}: {}",
                            connection_id, e
                        );
                        continue;
                    }
                };

                message.header.sender = Some(connection_id.clone());

                debug!(
                    "Received message from {}: type={:?}, dest={}",
                    connection_id, message.header.message_type, message.header.destination
                );
                debug!("overall message {:?}", message);

                match message.header.message_type {
                    MessageType::Handshake => {
                        // ✓ Respond to handshake immediately
                        info!("Handshake received from {}", connection_id);

                        let ack = Message::method_return(
                            message.header.message_id.clone(),
                            json!({ "status": "ok", "version": "1.0.0" }),
                        );

                        let ack_data = serde_json::to_vec(&ack)?;
                        UnixSocketTransport::send_message(&mut stream, &ack_data).await?;

                        handshake_completed = true;
                        info!("Handshake ACK sent to {}", connection_id);
                    }

                    MessageType::Signal => {
                        // Broadcast to in-process subscribers, then push out
                        // over every OTHER connected socket client too (real
                        // fix, 2026-07-07 -- see
                        // IPCBus::broadcast_to_all_connections's doc comment;
                        // this is the path external processes like Evo use,
                        // as opposed to in-process services which go through
                        // IPCBus::emit_signal instead).
                        bus.broadcast_to_all_connections(&message).await;
                        if let Err(e) = bus.signal_bus().broadcast(message).await {
                            error!("Failed to broadcast signal: {}", e);
                        }
                    }

                    MessageType::MethodCall => {
                        let message_id = message.header.message_id.clone();
                        let dest = message.header.destination.clone();

                        // First try local/router handlers (internal services)
                        let response_res = if bus.router().has_handler(&dest, &message.header.method) {
                            bus.send_message(message).await
                        } else {
                            // External service: look up owning connection
                            if let Some(target_conn_id) = bus.get_connection_for_service(&dest).await {
                                // Avoid infinite loop: if this connection already owns the service,
                                // treat as not registered instead of re-forwarding to itself.
                                if target_conn_id == connection_id {
                                    Err(anyhow::anyhow!("Service not registered: {}", dest))
                                } else {
                                    // Forward to the external service owner (e.g. WM)
                                    bus.forward_call(&target_conn_id, message).await
                                }
                            } else {
                                Err(anyhow::anyhow!("Service not registered: {}", dest))
                            }
                        };

                        match response_res {
                            Ok(response) => {
                                let response_data = serde_json::to_vec(&response)?;
                                UnixSocketTransport::send_message(&mut stream, &response_data).await?;
                                debug!("Method response sent");
                            }
                            Err(e) => {
                                error!("Method call error: {}", e);
                                let error_msg = Message::error(
                                    message_id,
                                    &format!("Method failed: {}", e),
                                );
                                let error_data = serde_json::to_vec(&error_msg)?;
                                UnixSocketTransport::send_message(&mut stream, &error_data).await?;
                            }
                        }
                    }

                    MessageType::MethodReturn | MessageType::Error => {
                        // This might be a response for a request that was forwarded TO this connection
                        debug!(
                            "Reply message from {}: type={:?}, id={}",
                            connection_id, message.header.message_type, message.header.message_id
                        );

                        // FIXED: Use reply_to to find original caller
                        let lookup_id = message.header.reply_to.clone()
                            .unwrap_or_else(|| message.header.message_id.clone());

                        if let Some((_msgid, tx)) = pending_replies.remove(&lookup_id) {
                            // Send the full Message back to the origin connection task
                            let _ = tx.send(Ok(message));
                        } else {
                            debug!("No pending reply entry for lookup_id {} on connection {}",
                                lookup_id, connection_id);
                        }

                    }

                    MessageType::Ping => {
                        // Respond with PONG
                        let pong = Message::signal("org.geckoOS.Echo", "Pong", json!({}))?;
                        let pong_data = serde_json::to_vec(&pong)?;
                        UnixSocketTransport::send_message(&mut stream, &pong_data).await?;
                        debug!("PONG sent");
                    }

                    _ => {
                        warn!(
                            "Unhandled message type from {}: {:?}",
                            connection_id, message.header.message_type
                        );
                    }
                }
            }

            // CASE 2: Another connection wants this one to handle a call (forwarded request)
            Some(req) = forward_rx.recv() => {
                // Store the reply sender keyed by this message_id so we can match the reply later
                let msg_id = req.message.header.message_id.clone();
                pending_replies.insert(msg_id.clone(), req.response_tx);

                // Forward the message over THIS socket to the process on this connection (e.g. WM)
                let data = serde_json::to_vec(&req.message)?;
                if let Err(e) = UnixSocketTransport::send_message(&mut stream, &data).await {
                    // If sending failed, clean up and return error to caller
                    error!("Failed to forward message {} on {}: {}", msg_id, connection_id, e);
                    // FIXED: Use reply_to to find original caller
                    let lookup_id = req.message.header.reply_to.clone()
                        .unwrap_or_else(|| req.message.header.message_id.clone());

                    if let Some((_msgid, tx)) = pending_replies.remove(&lookup_id) {
                        // Send the full Message back to the origin connection task
                        let _ = tx.send(Ok(req.message));
                    } else {
                        debug!("No pending reply entry for lookup_id {} on connection {}",
                            lookup_id, connection_id);
                    }

                }
            }

            // CASE 3: A SIGNAL was emitted somewhere and needs pushing out
            // over THIS socket (fire-and-forget, no reply expected -- see
            // IPCBus::broadcast_to_all_connections).
            Some(signal_msg) = broadcast_rx.recv() => {
                let data = serde_json::to_vec(&signal_msg)?;
                if let Err(e) = UnixSocketTransport::send_message(&mut stream, &data).await {
                    warn!("Failed to push signal to {}: {}", connection_id, e);
                }
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
    if let Err(e) = bus
        .emit_signal(
            "org.geckoOS.Echo",
            "Shutdown",
            serde_json::json!({ "reason": "daemon_shutdown" }),
        )
        .await
    {
        warn!("Failed to emit shutdown signal: {}", e);
    }

    info!("✓ Resources cleaned up");
    Ok(())
}

async fn register_all_services(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering all GeckoOS services");

    // ==================== FILESYSTEM SERVICE ====================
    register_filesystem_handlers(bus).await?;

    // ==================== APPMANAGER SERVICE (legacy shim) ====================
    register_appmanager_handlers(bus).await?;

    // ==================== APPHOST SERVICE (lifecycle) ====================
    register_apphost_handlers(bus).await?;

    // ==================== NOVA SERVICE (web-app runtime host) ====================
    register_nova_handlers(bus).await?;

    // ==================== TERMINAL SERVICE (real PTY-backed shell) ====================
    register_terminal_handlers(bus).await?;

    // ==================== POWER SERVICE ====================
    register_power_handlers(bus).await?;

    // ==================== SETTINGS SERVICE ====================
    register_settings_handlers(bus).await?;

    // ==================== NETWORK SERVICE ====================
    register_network_handlers(bus).await?;

    // ==================== DEVICEINFO SERVICE ====================
    register_deviceinfo_handlers(bus).await?;

    // ==================== NOTIFICATION SERVICE ====================
    register_notification_handlers(bus).await?;

    // ==================== PROCESS SERVICE ====================
    register_process_handlers(bus).await?;

    info!("✓ All services registered successfully");
    Ok(())
}

// ==================== FILESYSTEM SERVICE HANDLERS ====================
async fn register_filesystem_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering filesystem service handlers");

    // readDirectory
    let bus_clone = bus.clone();
    bus.router().register_handler(
        "org.geckoOS.filesystem",
        "readDirectory",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move {
                let path = msg.body.get("path").and_then(|v| v.as_str()).unwrap_or("/");
                match services::filesystem::FilesystemService::read_directory(path) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // readFile
    let bus_clone = bus.clone();
    bus.router().register_handler(
        "org.geckoOS.filesystem",
        "readFile",
        handler(move |msg| {
            Box::pin(async move {
                let path = msg.body.get("path").and_then(|v| v.as_str()).unwrap_or("/");
                let encoding = msg
                    .body
                    .get("encoding")
                    .and_then(|v| v.as_str())
                    .unwrap_or("utf-8");
                match services::filesystem::FilesystemService::read_file(path, encoding) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // writeFile
    bus.router().register_handler(
        "org.geckoOS.filesystem",
        "writeFile",
        handler(move |msg| {
            Box::pin(async move {
                let path = msg.body.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let content = msg
                    .body
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let encoding = msg
                    .body
                    .get("encoding")
                    .and_then(|v| v.as_str())
                    .unwrap_or("utf-8");
                match services::filesystem::FilesystemService::write_file(path, content, encoding) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // appendFile
    bus.router().register_handler(
        "org.geckoOS.filesystem",
        "appendFile",
        handler(move |msg| {
            Box::pin(async move {
                let path = msg.body.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let content = msg
                    .body
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let encoding = msg
                    .body
                    .get("encoding")
                    .and_then(|v| v.as_str())
                    .unwrap_or("utf-8");
                match services::filesystem::FilesystemService::append_file(path, content, encoding)
                {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // deleteFile
    bus.router().register_handler(
        "org.geckoOS.filesystem",
        "deleteFile",
        handler(move |msg| {
            Box::pin(async move {
                let path = msg.body.get("path").and_then(|v| v.as_str()).unwrap_or("");
                match services::filesystem::FilesystemService::delete_file(path) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // deleteDirectory
    bus.router().register_handler(
        "org.geckoOS.filesystem",
        "deleteDirectory",
        handler(move |msg| {
            Box::pin(async move {
                let path = msg.body.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let recursive = msg
                    .body
                    .get("recursive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match services::filesystem::FilesystemService::delete_directory(path, recursive) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // createDirectory
    bus.router().register_handler(
        "org.geckoOS.filesystem",
        "createDirectory",
        handler(move |msg| {
            Box::pin(async move {
                let path = msg.body.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let mode = msg
                    .body
                    .get("mode")
                    .and_then(|v| v.as_u64())
                    .map(|m| m as u32);
                match services::filesystem::FilesystemService::create_directory(path, mode) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // exists
    bus.router().register_handler(
        "org.geckoOS.filesystem",
        "exists",
        handler(move |msg| {
            Box::pin(async move {
                let path = msg.body.get("path").and_then(|v| v.as_str()).unwrap_or("");
                match services::filesystem::FilesystemService::exists(path) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // stat
    bus.router().register_handler(
        "org.geckoOS.filesystem",
        "stat",
        handler(move |msg| {
            Box::pin(async move {
                let path = msg.body.get("path").and_then(|v| v.as_str()).unwrap_or("");
                match services::filesystem::FilesystemService::stat(path) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // copy
    bus.router().register_handler(
        "org.geckoOS.filesystem",
        "copy",
        handler(move |msg| {
            Box::pin(async move {
                let source = msg
                    .body
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let destination = msg
                    .body
                    .get("destination")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match services::filesystem::FilesystemService::copy(source, destination) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // move
    bus.router().register_handler(
        "org.geckoOS.filesystem",
        "move",
        handler(move |msg| {
            Box::pin(async move {
                let source = msg
                    .body
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let destination = msg
                    .body
                    .get("destination")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match services::filesystem::FilesystemService::move_file(source, destination) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // chmod
    bus.router().register_handler(
        "org.geckoOS.filesystem",
        "chmod",
        handler(move |msg| {
            Box::pin(async move {
                let path = msg.body.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let mode = msg
                    .body
                    .get("mode")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0o755) as u32;
                match services::filesystem::FilesystemService::chmod(path, mode) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    info!("✓ Filesystem service handlers registered");
    Ok(())
}

// ==================== POWER SERVICE HANDLERS ====================
async fn register_power_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering power service handlers");

    // shutdown
    bus.router().register_handler(
        "org.geckoOS.power",
        "shutdown",
        handler(move |msg| {
            Box::pin(async move {
                let delay = msg.body.get("delay").and_then(|v| v.as_u64());
                match services::power::PowerService::shutdown(delay) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // reboot
    bus.router().register_handler(
        "org.geckoOS.power",
        "reboot",
        handler(move |msg| {
            Box::pin(async move {
                let delay = msg.body.get("delay").and_then(|v| v.as_u64());
                match services::power::PowerService::reboot(delay) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // sleep
    bus.router().register_handler(
        "org.geckoOS.power",
        "sleep",
        handler(move |msg| {
            Box::pin(async move {
                match services::power::PowerService::sleep() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // wake
    bus.router().register_handler(
        "org.geckoOS.power",
        "wake",
        handler(move |msg| {
            Box::pin(async move {
                match services::power::PowerService::wake() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // getBatteryStatus
    bus.router().register_handler(
        "org.geckoOS.power",
        "getBatteryStatus",
        handler(move |msg| {
            Box::pin(async move {
                match services::power::PowerService::get_battery_status() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // setScreenBrightness
    bus.router().register_handler(
        "org.geckoOS.power",
        "setScreenBrightness",
        handler(move |msg| {
            Box::pin(async move {
                let brightness = msg
                    .body
                    .get("brightness")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50) as u32;
                match services::power::PowerService::set_screen_brightness(brightness) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // getScreenBrightness
    bus.router().register_handler(
        "org.geckoOS.power",
        "getScreenBrightness",
        handler(move |msg| {
            Box::pin(async move {
                match services::power::PowerService::get_screen_brightness() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // setScreenTimeout
    bus.router().register_handler(
        "org.geckoOS.power",
        "setScreenTimeout",
        handler(move |msg| {
            Box::pin(async move {
                let timeout = msg
                    .body
                    .get("timeout")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(300);
                match services::power::PowerService::set_screen_timeout(timeout) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // getScreenTimeout
    bus.router().register_handler(
        "org.geckoOS.power",
        "getScreenTimeout",
        handler(move |msg| {
            Box::pin(async move {
                match services::power::PowerService::get_screen_timeout() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    info!("✓ Power service handlers registered");
    Ok(())
}

// ==================== SETTINGS SERVICE HANDLERS ====================
async fn register_settings_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering settings service handlers");

    bus.router().register_handler(
        "org.geckoOS.settings",
        "get",
        handler(move |msg| {
            Box::pin(async move {
                let key = msg.body.get("key").and_then(|v| v.as_str()).unwrap_or("");
                match services::settings::SettingsService::get(key) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.settings",
        "set",
        handler(move |msg| {
            Box::pin(async move {
                let key = msg.body.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let value = msg.body.get("value").cloned().unwrap_or_default();
                match services::settings::SettingsService::set(key, value) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.settings",
        "getAll",
        handler(move |msg| {
            Box::pin(async move {
                match services::settings::SettingsService::get_all() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.settings",
        "delete",
        handler(move |msg| {
            Box::pin(async move {
                let key = msg.body.get("key").and_then(|v| v.as_str()).unwrap_or("");
                match services::settings::SettingsService::delete(key) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.settings",
        "clear",
        handler(move |msg| {
            Box::pin(async move {
                match services::settings::SettingsService::clear() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.settings",
        "reset",
        handler(move |msg| {
            Box::pin(async move {
                match services::settings::SettingsService::reset() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    info!("✓ Settings service handlers registered");
    Ok(())
}

// ==================== NETWORK SERVICE HANDLERS ====================
async fn register_network_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering network service handlers");

    bus.router().register_handler(
        "org.geckoOS.network",
        "getStatus",
        handler(move |msg| {
            Box::pin(async move {
                match services::network::NetworkService::get_status() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.network",
        "getType",
        handler(move |msg| {
            Box::pin(async move {
                match services::network::NetworkService::get_type() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.network",
        "getInterfaces",
        handler(move |msg| {
            Box::pin(async move {
                match services::network::NetworkService::get_interfaces() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.network",
        "getIPAddress",
        handler(move |msg| {
            Box::pin(async move {
                let iface = msg.body.get("interface").and_then(|v| v.as_str());
                match services::network::NetworkService::get_ip_address(iface) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.network",
        "getGateway",
        handler(move |msg| {
            Box::pin(async move {
                match services::network::NetworkService::get_gateway() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.network",
        "getDNS",
        handler(move |msg| {
            Box::pin(async move {
                match services::network::NetworkService::get_dns() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    info!("✓ Network service handlers registered");
    Ok(())
}

// ==================== DEVICEINFO SERVICE HANDLERS ====================
async fn register_deviceinfo_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering deviceinfo service handlers");

    bus.router().register_handler(
        "org.geckoOS.deviceInfo",
        "getOS",
        handler(move |msg| {
            Box::pin(async move {
                match services::deviceinfo::DeviceInfoService::get_os() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.deviceInfo",
        "getKernel",
        handler(move |msg| {
            Box::pin(async move {
                match services::deviceinfo::DeviceInfoService::get_kernel() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.deviceInfo",
        "getModel",
        handler(move |msg| {
            Box::pin(async move {
                match services::deviceinfo::DeviceInfoService::get_model() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.deviceInfo",
        "getManufacturer",
        handler(move |msg| {
            Box::pin(async move {
                match services::deviceinfo::DeviceInfoService::get_manufacturer() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.deviceInfo",
        "getSerial",
        handler(move |msg| {
            Box::pin(async move {
                match services::deviceinfo::DeviceInfoService::get_serial() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.deviceInfo",
        "getHardware",
        handler(move |msg| {
            Box::pin(async move {
                match services::deviceinfo::DeviceInfoService::get_hardware() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.deviceInfo",
        "getUUID",
        handler(move |msg| {
            Box::pin(async move {
                match services::deviceinfo::DeviceInfoService::get_uuid() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.deviceInfo",
        "getHostname",
        handler(move |msg| {
            Box::pin(async move {
                match services::deviceinfo::DeviceInfoService::get_hostname() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.deviceInfo",
        "getMemoryInfo",
        handler(move |msg| {
            Box::pin(async move {
                match services::deviceinfo::DeviceInfoService::get_memory_info() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.deviceInfo",
        "getStorageInfo",
        handler(move |msg| {
            Box::pin(async move {
                let mount = msg.body.get("mount").and_then(|v| v.as_str());
                match services::deviceinfo::DeviceInfoService::get_storage_info(mount) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.deviceInfo",
        "getCPUInfo",
        handler(move |msg| {
            Box::pin(async move {
                match services::deviceinfo::DeviceInfoService::get_cpu_info() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    info!("✓ DeviceInfo service handlers registered");
    Ok(())
}

// ==================== NOTIFICATION SERVICE HANDLERS ====================
async fn register_notification_handlers(
    bus: &Arc<IPCBus>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering notification service handlers");

    bus.router().register_handler(
        "org.geckoOS.notification",
        "show",
        handler(move |msg| {
            Box::pin(async move {
                let title = msg
                    .body
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Notification");
                let options = msg.body.get("options").cloned();
                match services::notification::NotificationService::show(title, options) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.notification",
        "close",
        handler(move |msg| {
            Box::pin(async move {
                let notif_id = msg
                    .body
                    .get("notificationId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match services::notification::NotificationService::close(notif_id) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.notification",
        "closeAll",
        handler(move |msg| {
            Box::pin(async move {
                match services::notification::NotificationService::close_all() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    info!("✓ Notification service handlers registered");
    Ok(())
}

// ==================== PROCESS SERVICE HANDLERS ====================
async fn register_process_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering process service handlers");

    bus.router().register_handler(
        "org.geckoOS.process",
        "spawn",
        handler(move |msg| {
            Box::pin(async move {
                let command = msg
                    .body
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = msg.body.get("args").and_then(|v| v.as_array()).map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.to_string())
                        .collect()
                });
                let options = msg.body.get("options").cloned();
                match services::process::ProcessService::spawn(command, args, options) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.process",
        "kill",
        handler(move |msg| {
            Box::pin(async move {
                let pid = msg.body.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let signal = msg.body.get("signal").and_then(|v| v.as_str());
                match services::process::ProcessService::kill(pid, signal) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.process",
        "killGroup",
        handler(move |msg| {
            Box::pin(async move {
                let pgid = msg.body.get("pgid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let signal = msg.body.get("signal").and_then(|v| v.as_str());
                match services::process::ProcessService::kill_group(pgid, signal) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.process",
        "list",
        handler(move |msg| {
            Box::pin(async move {
                match services::process::ProcessService::list() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.process",
        "getInfo",
        handler(move |msg| {
            Box::pin(async move {
                let pid = msg.body.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                match services::process::ProcessService::get_info(pid) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    info!("✓ Process service handlers registered");
    Ok(())
}

// ==================== APPMANAGER SERVICE HANDLERS ====================
async fn register_appmanager_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering appManager service handlers");

    bus.router().register_handler(
        "org.geckoOS.appManager",
        "launch",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                let params = msg.body.get("params").cloned();
                match services::appmanager::AppManagerService::launch(app_id, params) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.appManager",
        "close",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                match services::appmanager::AppManagerService::close(app_id) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.appManager",
        "list",
        handler(move |msg| {
            Box::pin(async move {
                match services::appmanager::AppManagerService::list() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.appManager",
        "getInfo",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                match services::appmanager::AppManagerService::get_info(app_id) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.appManager",
        "install",
        handler(move |msg| {
            Box::pin(async move {
                let app_path = msg
                    .body
                    .get("appPath")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match services::appmanager::AppManagerService::install(app_path) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.appManager",
        "uninstall",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                match services::appmanager::AppManagerService::uninstall(app_id) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.appManager",
        "kill",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                match services::appmanager::AppManagerService::kill(app_id) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.appManager",
        "restart",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                match services::appmanager::AppManagerService::restart(app_id) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.appManager",
        "setActive",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                match services::appmanager::AppManagerService::set_active(app_id) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.appManager",
        "getActive",
        handler(move |msg| {
            Box::pin(async move {
                match services::appmanager::AppManagerService::get_active() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    info!("✓ AppManager service handlers registered");
    Ok(())
}

// ==================== APPHOST SERVICE HANDLERS ====================
async fn register_apphost_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering appHost service handlers");

    // Wire the bus into the global AppHost so lifecycle transitions can
    // emit signals on the `org.geckoOS.appHost` interface.
    services::AppHost::global().set_bus(bus.clone());
    // Start the SIGCHLD reaper so process exits map to AppStopped/AppCrashed
    // signals automatically. Idempotent.
    services::AppHost::global().start_child_reaper();

    // LaunchApp(appId, params?) -> AppSession
    bus.router().register_handler(
        "org.geckoOS.appHost",
        "LaunchApp",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                let params = msg.body.get("params").cloned();
                match services::AppHost::global().launch_json(app_id, params) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // CloseApp(sessionId) -> AppSession
    bus.router().register_handler(
        "org.geckoOS.appHost",
        "CloseApp",
        handler(move |msg| {
            Box::pin(async move {
                let sid = msg
                    .body
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match services::AppHost::global().close_json(sid) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // KillApp(sessionId) -> AppSession
    bus.router().register_handler(
        "org.geckoOS.appHost",
        "KillApp",
        handler(move |msg| {
            Box::pin(async move {
                let sid = msg
                    .body
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match services::AppHost::global().kill_json(sid) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // RestartApp(sessionId) -> AppSession
    bus.router().register_handler(
        "org.geckoOS.appHost",
        "RestartApp",
        handler(move |msg| {
            Box::pin(async move {
                let sid = msg
                    .body
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match services::AppHost::global().restart_json(sid) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // ListApps() -> { apps: [...] }
    bus.router().register_handler(
        "org.geckoOS.appHost",
        "ListApps",
        handler(move |msg| {
            Box::pin(async move {
                match services::AppHost::global().list_apps_json() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // ListRunningApps() -> { sessions: [...] }
    bus.router().register_handler(
        "org.geckoOS.appHost",
        "ListRunningApps",
        handler(move |msg| {
            Box::pin(async move {
                match services::AppHost::global().list_running_json() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // GetAppState(sessionId) -> { sessionId, state }
    bus.router().register_handler(
        "org.geckoOS.appHost",
        "GetAppState",
        handler(move |msg| {
            Box::pin(async move {
                let sid = msg
                    .body
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match services::AppHost::global().get_state_json(sid) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // BindSurface(appId, surfaceId, sessionId?, pid?) -> AppSession
    //
    // Called from the compositor (evo) when an XDG toplevel is mapped. It
    // attaches the Wayland window id to the matching AppSession (or
    // synthesises one for foreign-process windows) and emits an
    // `AppSurfaceMapped` signal so the shell can wire its UI slot.
    bus.router().register_handler(
        "org.geckoOS.appHost",
        "BindSurface",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                let surface_id = msg
                    .body
                    .get("surfaceId")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or(0);
                let session_id = msg.body.get("sessionId").and_then(|v| v.as_str());
                let pid = msg.body.get("pid").and_then(|v| v.as_u64()).map(|v| v as u32);
                match services::AppHost::global()
                    .bind_surface_json(app_id, surface_id, session_id, pid)
                {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // UnbindSurface(surfaceId) -> AppSession | { surfaceId, unbound: false }
    bus.router().register_handler(
        "org.geckoOS.appHost",
        "UnbindSurface",
        handler(move |msg| {
            Box::pin(async move {
                let surface_id = msg
                    .body
                    .get("surfaceId")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or(0);
                match services::AppHost::global().unbind_surface_json(surface_id) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // ListSurfaceSessions() -> { sessions: [...] }
    //
    // Returns only sessions that currently own at least one mapped
    // compositor surface. Useful for the dock to render "phantom" icons
    // for orphan surfaces.
    bus.router().register_handler(
        "org.geckoOS.appHost",
        "ListSurfaceSessions",
        handler(move |msg| {
            Box::pin(async move {
                match services::AppHost::global().list_surface_sessions_json() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // SubscribeAppLifecycle() -> { interface }
    //
    // Echo's signal subscription is handled at the transport layer (see the
    // signal bus + connection broadcasting); this method exists so shells can
    // explicitly express intent and discover the canonical interface name.
    bus.router().register_handler(
        "org.geckoOS.appHost",
        "SubscribeAppLifecycle",
        handler(move |msg| {
            Box::pin(async move {
                let result = json!({
                    "success": true,
                    "interface": services::APPHOST_IFACE,
                    "signals": [
                        "AppStarting",
                        "AppRunning",
                        "AppStopped",
                        "AppCrashed",
                        "AppSurfaceMapped",
                        "AppSurfaceUnmapped",
                    ],
                });
                Ok(Message::method_return(msg.header.message_id, result))
            })
        })
        .into(),
    );

    info!("✓ AppHost service handlers registered");
    Ok(())
}

// ==================== NOVA SERVICE HANDLERS ====================
async fn register_nova_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering Nova service handlers");

    services::Nova::global().set_bus(bus.clone());
    // Initial registry scan — discovers whatever manifest.webapp entries
    // are staged under the system/user app roots at daemon start. Safe to
    // fail (e.g. roots don't exist yet on a fresh image); logged, not fatal.
    if let Err(e) = services::Nova::global().reload() {
        tracing::warn!("nova: initial registry scan failed: {}", e);
    }

    // Lifecycle: subscribe to Evo's real `WindowMinimized` signal (interface
    // "org.geckoOS.evo", see evo/src/enhancements/echo_ipc.rs's
    // emit_window_minimized) so backgrounding/restoring a card actually
    // suspends/resumes its process — see Nova::handle_window_minimized/
    // handle_window_restored's doc comment for the full rationale. This
    // works in-process (no separate socket connection needed) because Evo,
    // as an external client, emits signals via a MethodCall to THIS
    // daemon's "org.geckoOS.Echo"/"EmitSignal" handler, which broadcasts
    // through this same `SignalBus` — so subscribing here really does
    // observe Evo's real signals, not a separate/parallel channel.
    bus.signal_bus()
        .subscribe(
            "org.geckoOS.evo",
            "nova-lifecycle".to_string(),
            handler(move |msg| {
                Box::pin(async move {
                    if msg.header.method == "WindowMinimized" {
                        let window_id = msg
                            .body
                            .get("window_id")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as u32);
                        let minimized = msg
                            .body
                            .get("minimized")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if let Some(window_id) = window_id {
                            if minimized {
                                services::Nova::global().handle_window_minimized(window_id);
                            } else {
                                services::Nova::global().handle_window_restored(window_id);
                            }
                        }
                    }
                    // Signal subscribers' return value is discarded (see
                    // SignalBus::broadcast) — this is never sent anywhere.
                    Ok(Message::method_return(msg.header.message_id, json!({})))
                })
            })
            .into(),
        )
        .await?;

    // Launch(appId, params?) -> { sessionId, appId, raised }
    bus.router().register_handler(
        "org.geckoOS.nova",
        "Launch",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                let params = msg.body.get("params").cloned();
                match services::Nova::global().launch(app_id, params) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // Close(appId) -> { appId, sessionId }
    bus.router().register_handler(
        "org.geckoOS.nova",
        "Close",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                match services::Nova::global().close(app_id) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // List() -> { apps: [...] }
    bus.router().register_handler(
        "org.geckoOS.nova",
        "List",
        handler(move |msg| {
            Box::pin(async move {
                match services::Nova::global().list() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // GetInfo(appId) -> manifest projection
    bus.router().register_handler(
        "org.geckoOS.nova",
        "GetInfo",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                match services::Nova::global().get_info(app_id) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // Install(packagePath) -> { appId, version }
    bus.router().register_handler(
        "org.geckoOS.nova",
        "Install",
        handler(move |msg| {
            Box::pin(async move {
                let package_path = msg
                    .body
                    .get("packagePath")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match services::Nova::global().install(package_path) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // Uninstall(appId, purgeData?) -> { appId }
    bus.router().register_handler(
        "org.geckoOS.nova",
        "Uninstall",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                let purge = msg
                    .body
                    .get("purgeData")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match services::Nova::global().uninstall(app_id, purge) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // Reload() -> { count }
    bus.router().register_handler(
        "org.geckoOS.nova",
        "Reload",
        handler(move |msg| {
            Box::pin(async move {
                match services::Nova::global().reload() {
                    Ok(count) => Ok(Message::method_return(
                        msg.header.message_id,
                        json!({ "count": count }),
                    )),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    info!("✓ Nova service handlers registered");
    Ok(())
}

// ==================== TERMINAL SERVICE HANDLERS ====================
async fn register_terminal_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering Terminal service handlers");

    services::TerminalService::global().set_bus(bus.clone());

    // Open(cols?, rows?) -> { terminalId }
    bus.router().register_handler(
        "org.geckoOS.terminal",
        "Open",
        handler(move |msg| {
            Box::pin(async move {
                let cols = msg.body.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;
                let rows = msg.body.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;
                match services::TerminalService::global().open_json(cols, rows) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // Write(terminalId, data) -> { success }
    bus.router().register_handler(
        "org.geckoOS.terminal",
        "Write",
        handler(move |msg| {
            Box::pin(async move {
                let terminal_id = msg
                    .body
                    .get("terminalId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let data = msg.body.get("data").and_then(|v| v.as_str()).unwrap_or("");
                match services::TerminalService::global().write_json(terminal_id, data) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // Resize(terminalId, cols, rows) -> { success }
    bus.router().register_handler(
        "org.geckoOS.terminal",
        "Resize",
        handler(move |msg| {
            Box::pin(async move {
                let terminal_id = msg
                    .body
                    .get("terminalId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let cols = msg.body.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;
                let rows = msg.body.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;
                match services::TerminalService::global().resize_json(terminal_id, cols, rows) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    // Close(terminalId) -> { success }
    bus.router().register_handler(
        "org.geckoOS.terminal",
        "Close",
        handler(move |msg| {
            Box::pin(async move {
                let terminal_id = msg
                    .body
                    .get("terminalId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match services::TerminalService::global().close_json(terminal_id) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &e.to_string())),
                }
            })
        })
        .into(),
    );

    info!("✓ Terminal service handlers registered");
    Ok(())
}

fn print_startup_info() {
    println!();
    println!("╔════════════════════════════════════════╗");
    println!("║ Echo IPC Bus - Running                 ║");
    println!("╠════════════════════════════════════════╣");
    println!("║ Socket: $XDG_RUNTIME_DIR/geckoOS/      ║");
    println!("║         echo.sock                      ║");
    println!("╠════════════════════════════════════════╣");
    println!("║ Status: READY                          ║");
    println!("╚════════════════════════════════════════╝");
    println!();
}
