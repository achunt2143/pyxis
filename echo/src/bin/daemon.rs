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

    // The socket is read in a task of its own, and the loop below waits on its
    // channel. Reading a frame inside the `select!` was not cancel-safe: a
    // large frame arrives in pieces (an Evo thumbnail reply), and when a signal
    // or a forwarded call won the select part-way through, the bytes read so
    // far were dropped, the next "length" came from the middle of the frame,
    // and the connection was closed. Evo re-registered, and the call waiting on
    // it failed with "Connection task dropped the request". A channel's recv
    // is cancel-safe.
    let (mut read_half, mut stream) = stream.into_split();
    let (frames_tx, mut frames_rx) = mpsc::channel::<anyhow::Result<Vec<u8>>>(32);
    let reader = tokio::spawn(async move {
        loop {
            let frame = UnixSocketTransport::recv_message(&mut read_half).await;
            let failed = frame.is_err();
            if frames_tx.send(frame).await.is_err() || failed {
                break;
            }
        }
    });
    struct AbortOnDrop(tokio::task::JoinHandle<()>);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _reader = AbortOnDrop(reader);

    loop {
        tokio::select! {
            // CASE 1: Data arriving from the socket (this process -> daemon)
            Some(socket_res) = frames_rx.recv() => {
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

    // ==================== INSTALLER SERVICE (.ipk / opkg) ====================
    register_installer_handlers(bus).await?;

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
    register_sound_handlers(bus).await?;
    register_audio_handlers(bus).await?;

    // ==================== TIME SERVICE ====================
    register_time_handlers(bus).await?;

    // ==================== PROCESS SERVICE ====================
    register_process_handlers(bus).await?;

    // ==================== SYSTEM SERVICE ====================
    register_system_handlers(bus).await?;

    info!("✓ All services registered successfully");
    Ok(())
}

// ==================== FILESYSTEM SERVICE HANDLERS ====================
async fn register_filesystem_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    use services::filesystem::{FilesystemService, METHODS};
    info!("Registering filesystem service handlers");

    // One handler for every method. FilesystemService::call reads a call's
    // scope before anything else, so no method can skip the confinement an
    // app's calls carry (services/filesystem/scope.rs).
    for &method in METHODS {
        bus.router().register_handler(
            "org.geckoOS.filesystem",
            method,
            handler(move |msg| {
                Box::pin(async move {
                    match FilesystemService::call(method, &msg.body).await {
                        Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                        Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                    }
                })
            })
            .into(),
        );
    }

    info!("✓ Filesystem service handlers registered");
    Ok(())
}

/// The shutdown sound, played to its end (at most 4 s) before the power goes.
/// A missing sound or audio stack only means silence, never a failed
/// shutdown.
async fn play_shutdown_sound() {
    if let Err(e) = services::sound::SoundService::system()
        .play_and_wait("shutdown", std::time::Duration::from_secs(4))
        .await
    {
        debug!("no shutdown sound: {e}");
    }
}

// ==================== SOUND SERVICE HANDLERS ====================
async fn register_sound_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    use services::sound::{SoundService, SOUND_IFACE};
    info!("Registering sound service handlers");

    bus.router().register_handler(
        SOUND_IFACE,
        "play",
        handler(move |msg| {
            Box::pin(async move {
                let name = msg.body.get("name").and_then(|v| v.as_str()).unwrap_or("");
                match SoundService::system().play(name).await {
                    Ok(pid) => Ok(Message::method_return(
                        msg.header.message_id,
                        json!({ "success": true, "name": name, "pid": pid }),
                    )),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        SOUND_IFACE,
        "list",
        handler(move |msg| {
            Box::pin(async move {
                Ok(Message::method_return(
                    msg.header.message_id,
                    json!({ "sounds": SoundService::system().list() }),
                ))
            })
        })
        .into(),
    );

    info!("✓ Sound service handlers registered");
    Ok(())
}

// ==================== AUDIO SERVICE HANDLERS ====================
async fn register_audio_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    use services::audio::{AudioService, AUDIO_IFACE, SIGNAL_OUTPUT_CHANGED, SIGNAL_VOLUME_CHANGED};
    info!("Registering audio service handlers");

    /// Reply with `result`, and on success also signal it under `signal`,
    /// so every open view (the quick-settings slider, Settings) follows
    /// a change made anywhere.
    async fn reply<T: serde::Serialize>(
        bus: &IPCBus,
        message_id: String,
        result: anyhow::Result<T>,
        signal: Option<&str>,
    ) -> anyhow::Result<Message> {
        match result {
            Ok(value) => {
                let value = serde_json::to_value(value)?;
                if let Some(signal) = signal {
                    if let Err(e) = bus.emit_signal(AUDIO_IFACE, signal, value.clone()).await {
                        warn!("audio signal {} failed: {}", signal, e);
                    }
                }
                Ok(Message::method_return(message_id, value))
            }
            Err(e) => Ok(Message::error(message_id, &format!("{e:#}"))),
        }
    }

    let bus_clone = bus.clone();
    bus.router().register_handler(
        AUDIO_IFACE,
        "getVolume",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move { reply(&bus, msg.header.message_id, AudioService::volume().await, None).await })
        })
        .into(),
    );

    let bus_clone = bus.clone();
    bus.router().register_handler(
        AUDIO_IFACE,
        "setVolume",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move {
                let result = match msg.body.get("volume").and_then(|v| v.as_u64()) {
                    Some(volume) => AudioService::set_volume(volume.min(100) as u8).await,
                    None => Err(anyhow::anyhow!("setVolume needs a numeric `volume`, 0-100")),
                };
                reply(&bus, msg.header.message_id, result, Some(SIGNAL_VOLUME_CHANGED)).await
            })
        })
        .into(),
    );

    let bus_clone = bus.clone();
    bus.router().register_handler(
        AUDIO_IFACE,
        "setMute",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move {
                let result = match msg.body.get("muted").and_then(|v| v.as_bool()) {
                    Some(muted) => AudioService::set_mute(muted).await,
                    None => Err(anyhow::anyhow!("setMute needs a boolean `muted`")),
                };
                reply(&bus, msg.header.message_id, result, Some(SIGNAL_VOLUME_CHANGED)).await
            })
        })
        .into(),
    );

    let bus_clone = bus.clone();
    bus.router().register_handler(
        AUDIO_IFACE,
        "listOutputs",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move {
                let result = AudioService::outputs().await.map(|outputs| json!({ "outputs": outputs }));
                reply(&bus, msg.header.message_id, result, None).await
            })
        })
        .into(),
    );

    let bus_clone = bus.clone();
    bus.router().register_handler(
        AUDIO_IFACE,
        "setDefaultOutput",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move {
                let result = match msg.body.get("id").and_then(|v| v.as_u64()) {
                    Some(id) => AudioService::set_default_output(id as u32)
                        .await
                        .map(|outputs| json!({ "outputs": outputs })),
                    None => Err(anyhow::anyhow!("setDefaultOutput needs a numeric `id`")),
                };
                reply(&bus, msg.header.message_id, result, Some(SIGNAL_OUTPUT_CHANGED)).await
            })
        })
        .into(),
    );

    info!("✓ Audio service handlers registered");
    Ok(())
}

// ==================== TIME SERVICE HANDLERS ====================
async fn register_time_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    use services::time::{TimeService, SIGNAL_TIMEZONE_CHANGED, TIME_IFACE};
    info!("Registering time service handlers");

    fn reply<T: serde::Serialize>(message_id: String, result: anyhow::Result<T>) -> anyhow::Result<Message> {
        match result.and_then(|value| Ok(serde_json::to_value(value)?)) {
            Ok(value) => Ok(Message::method_return(message_id, value)),
            Err(e) => Ok(Message::error(message_id, &format!("{e:#}"))),
        }
    }

    bus.router().register_handler(
        TIME_IFACE,
        "getClock",
        handler(move |msg| {
            Box::pin(async move { reply(msg.header.message_id, Ok(TimeService::system().clock())) })
        })
        .into(),
    );

    bus.router().register_handler(
        TIME_IFACE,
        "listTimezones",
        handler(move |msg| {
            Box::pin(async move {
                let result = TimeService::system().list_timezones().map(|timezones| json!({ "timezones": timezones }));
                reply(msg.header.message_id, result)
            })
        })
        .into(),
    );

    bus.router().register_handler(
        TIME_IFACE,
        "getLocation",
        handler(move |msg| {
            Box::pin(async move { reply(msg.header.message_id, Ok(TimeService::system().location())) })
        })
        .into(),
    );

    // Relinks /etc/localtime, then tells every view (Bloom's clock, the
    // wallpaper, Settings) the new zone and offset.
    let bus_clone = bus.clone();
    bus.router().register_handler(
        TIME_IFACE,
        "setTimezone",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move {
                let result = match msg.body.get("timezone").and_then(|v| v.as_str()) {
                    Some(zone) => TimeService::system().set_timezone(zone),
                    None => Err(anyhow::anyhow!("setTimezone needs a string `timezone`")),
                };
                if let Ok(change) = &result {
                    let body = serde_json::to_value(change)?;
                    if let Err(e) = bus.emit_signal(TIME_IFACE, SIGNAL_TIMEZONE_CHANGED, body).await {
                        warn!("time signal {} failed: {}", SIGNAL_TIMEZONE_CHANGED, e);
                    }
                }
                reply(msg.header.message_id, result)
            })
        })
        .into(),
    );

    info!("✓ Time service handlers registered");
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
                play_shutdown_sound().await;
                match services::power::PowerService::shutdown(delay) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                play_shutdown_sound().await;
                match services::power::PowerService::reboot(delay) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
    use services::settings::{Outcome, SettingsService, SETTINGS_IFACE, SIGNAL_SETTING_CHANGED};
    info!("Registering settings service handlers");

    /// Reply to a write, and on success signal `SettingChanged` once per key
    /// it changed. The Settings app and the shell are different origins, so
    /// this signal is how a change made in one reaches the other live.
    async fn reply_write(bus: &IPCBus, message_id: String, result: anyhow::Result<Outcome>) -> anyhow::Result<Message> {
        match result {
            Ok(outcome) => {
                for change in &outcome.changes {
                    if let Err(e) = bus.emit_signal(SETTINGS_IFACE, SIGNAL_SETTING_CHANGED, change.signal_body()).await {
                        warn!("settings signal for {} failed: {}", change.key, e);
                    }
                }
                Ok(Message::method_return(message_id, outcome.reply))
            }
            Err(e) => Ok(Message::error(message_id, &format!("{e:#}"))),
        }
    }

    bus.router().register_handler(
        SETTINGS_IFACE,
        "get",
        handler(move |msg| {
            Box::pin(async move {
                let key = msg.body.get("key").and_then(|v| v.as_str()).unwrap_or("");
                match SettingsService::get(key) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    let bus_clone = bus.clone();
    bus.router().register_handler(
        SETTINGS_IFACE,
        "set",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move {
                let key = msg.body.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let value = msg.body.get("value").cloned().unwrap_or_default();
                reply_write(&bus, msg.header.message_id, SettingsService::set(key, value)).await
            })
        })
        .into(),
    );

    bus.router().register_handler(
        SETTINGS_IFACE,
        "getAll",
        handler(move |msg| {
            Box::pin(async move {
                match SettingsService::get_all() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    let bus_clone = bus.clone();
    bus.router().register_handler(
        SETTINGS_IFACE,
        "delete",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move {
                let key = msg.body.get("key").and_then(|v| v.as_str()).unwrap_or("");
                reply_write(&bus, msg.header.message_id, SettingsService::delete(key)).await
            })
        })
        .into(),
    );

    let bus_clone = bus.clone();
    bus.router().register_handler(
        SETTINGS_IFACE,
        "clear",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move { reply_write(&bus, msg.header.message_id, SettingsService::clear()).await })
        })
        .into(),
    );

    let bus_clone = bus.clone();
    bus.router().register_handler(
        SETTINGS_IFACE,
        "reset",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move { reply_write(&bus, msg.header.message_id, SettingsService::reset()).await })
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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

    use services::notification::{NotificationStore, NOTIFICATION_IFACE, SIGNAL_CLOSED, SIGNAL_SHOWN};

    // The notification sound follows the toast's rules (Bloom's
    // NotificationToasts): a low-urgency notification is silent, Do Not
    // Disturb silences the rest, and a critical one sounds regardless.
    fn notification_should_sound(n: &services::notification::Notification) -> bool {
        use services::notification::Urgency;
        match n.urgency {
            Urgency::Low => false,
            Urgency::Critical => true,
            Urgency::Normal => !services::SettingsService::get("pyxis.dnd")
                .ok()
                .and_then(|v| v["value"].as_bool())
                .unwrap_or(false),
        }
    }

    // Every change is signalled, so a shell never has to poll: Bloom draws
    // its toast from `NotificationShown`, and every open view of the centre
    // drops an entry on `NotificationClosed`. A signal that fails to send is
    // logged, not returned: the store already changed, and the caller's
    // `show` did succeed.
    async fn notify_signal(bus: &IPCBus, name: &str, payload: serde_json::Value) {
        if let Err(e) = bus.emit_signal(NOTIFICATION_IFACE, name, payload).await {
            warn!("notification signal {} failed: {}", name, e);
        }
    }

    let bus_clone = bus.clone();
    bus.router().register_handler(
        NOTIFICATION_IFACE,
        "show",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move {
                let notification = NotificationStore::global().show(&msg.body);
                if notification_should_sound(&notification) {
                    if let Err(e) = services::sound::SoundService::system().play("notification").await {
                        debug!("no notification sound: {e}");
                    }
                }
                let value = serde_json::to_value(&notification)?;
                notify_signal(&bus, SIGNAL_SHOWN, serde_json::json!({ "notification": value })).await;
                Ok(Message::method_return(
                    msg.header.message_id,
                    serde_json::json!({
                        "success": true,
                        "notificationId": notification.id,
                        "notification": value,
                    }),
                ))
            })
        })
        .into(),
    );

    bus.router().register_handler(
        NOTIFICATION_IFACE,
        "list",
        handler(move |msg| {
            Box::pin(async move {
                let notifications = NotificationStore::global().list();
                Ok(Message::method_return(
                    msg.header.message_id,
                    serde_json::json!({ "notifications": notifications }),
                ))
            })
        })
        .into(),
    );

    let bus_clone = bus.clone();
    bus.router().register_handler(
        NOTIFICATION_IFACE,
        "close",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move {
                // `notificationId` is what the SDK sends; `id` is what the
                // entries themselves carry, so accept that too.
                let id = msg
                    .body
                    .get("notificationId")
                    .or_else(|| msg.body.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                let closed = NotificationStore::global().close(&id);
                if closed {
                    notify_signal(&bus, SIGNAL_CLOSED, serde_json::json!({ "notificationId": id })).await;
                }
                Ok(Message::method_return(
                    msg.header.message_id,
                    serde_json::json!({ "success": closed, "notificationId": id }),
                ))
            })
        })
        .into(),
    );

    let bus_clone = bus.clone();
    bus.router().register_handler(
        NOTIFICATION_IFACE,
        "closeAll",
        handler(move |msg| {
            let bus = bus_clone.clone();
            Box::pin(async move {
                let ids = NotificationStore::global().close_all();
                for id in &ids {
                    notify_signal(&bus, SIGNAL_CLOSED, serde_json::json!({ "notificationId": id })).await;
                }
                Ok(Message::method_return(
                    msg.header.message_id,
                    serde_json::json!({ "success": true, "closed": ids.len() }),
                ))
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    info!("✓ Process service handlers registered");
    Ok(())
}

// ==================== SYSTEM SERVICE HANDLERS ====================
//
// `org.geckoOS.system` is the destination the compiled-in Gecko bridge has
// always pointed its two top-level methods at --
// `window.geckoOS.getSystemInfo()` and `.listServices()`, see
// gecko/patches/pyxis/toolkit/actors/PyxisBridgeChild.sys.mjs -- with
// nothing registered here to answer them, so both failed with
// "Service not registered: org.geckoOS.system" while every other namespace
// on that object worked. The method names below are spelled exactly as the
// bridge sends them (camelCase, matching filesystem/power/deviceInfo rather
// than nova's PascalCase); changing their case here silently breaks the
// bridge again.
async fn register_system_handlers(bus: &Arc<IPCBus>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering system service handlers");

    bus.router().register_handler(
        "org.geckoOS.system",
        "getInfo",
        handler(move |msg| {
            Box::pin(async move {
                match services::system::SystemService::get_info() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    bus.router().register_handler(
        "org.geckoOS.system",
        "ping",
        handler(move |msg| {
            Box::pin(async move {
                Ok(Message::method_return(
                    msg.header.message_id,
                    services::system::SystemService::ping(),
                ))
            })
        })
        .into(),
    );

    // Unlike every other handler in this file, listServices has to read bus
    // state (the router's handler table plus the registry), so it captures
    // the bus the way register_internal_handlers' ListServices does.
    let bus_for_list = bus.clone();
    bus.router().register_handler(
        "org.geckoOS.system",
        "listServices",
        handler(move |msg| {
            let bus = bus_for_list.clone();
            Box::pin(async move {
                match services::system::SystemService::list_services(&bus).await {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    info!("✓ System service handlers registered");
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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

    // Launch(appId, params?) -> { sessionId, appId, raised, windowId? }
    bus.router().register_handler(
        "org.geckoOS.nova",
        "Launch",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                let params = msg.body.get("params").cloned();
                match services::Nova::global().launch(app_id, params).await {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    // Focus(appId) -> { appId, sessionId, windowId } — raise + give keyboard
    // focus to an already-running app's real compositor window (see
    // Nova::focus's doc comment for why this exists alongside Launch/Close).
    bus.router().register_handler(
        "org.geckoOS.nova",
        "Focus",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg.body.get("appId").and_then(|v| v.as_str()).unwrap_or("");
                match services::Nova::global().focus(app_id).await {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    // OpenExternalBrowser(url) -> { sessionId, appId, url } — real vela
    // window for an arbitrary external URL, bypassing Bloom's iframe-based
    // BrowserApp (which real sites' X-Frame-Options/CSP block). See
    // Nova::open_external_browser's doc comment for the full rationale.
    bus.router().register_handler(
        "org.geckoOS.nova",
        "OpenExternalBrowser",
        handler(move |msg| {
            Box::pin(async move {
                let url = msg.body.get("url").and_then(|v| v.as_str()).unwrap_or("");
                match services::Nova::global().open_external_browser(url) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    // Uninstall(appId) -> { appId, removed }. The app's data and profile
    // always go with it (`purgeData`, which chose that, is gone).
    bus.router().register_handler(
        "org.geckoOS.nova",
        "Uninstall",
        handler(move |msg| {
            Box::pin(async move {
                let app_id = msg
                    .body
                    .get("appId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // It waits for a running app to exit before the profile
                // goes, so it runs off the runtime's threads.
                let uninstalled =
                    tokio::task::spawn_blocking(move || services::Nova::global().uninstall(&app_id))
                        .await
                        .unwrap_or_else(|e| Err(anyhow::anyhow!("uninstall failed: {e}")));
                match uninstalled {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    info!("✓ Nova service handlers registered");
    Ok(())
}

// ==================== INSTALLER SERVICE HANDLERS ====================
// `.ipk`/opkg package installation. Owns the package layer only — every
// mutation ends by refreshing Nova's registry so the launcher stays in
// sync (see services/installer.rs's module doc for that split).
async fn register_installer_handlers(
    bus: &Arc<IPCBus>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Registering Installer service handlers");

    services::InstallerService::global().set_bus(bus.clone());

    // ListAvailable(dir?) -> { packages: [...], count }
    bus.router().register_handler(
        "org.geckoOS.installer",
        "ListAvailable",
        handler(move |msg| {
            Box::pin(async move {
                let dir = msg.body.get("dir").and_then(|v| v.as_str());
                match services::InstallerService::global().list_available(dir) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    // Inspect(packagePath) -> package metadata (no install)
    bus.router().register_handler(
        "org.geckoOS.installer",
        "Inspect",
        handler(move |msg| {
            Box::pin(async move {
                let path = msg
                    .body
                    .get("packagePath")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match services::InstallerService::global().inspect(path) {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    // Install(packagePath, force?) -> { package, version, launchable, ... }
    bus.router().register_handler(
        "org.geckoOS.installer",
        "Install",
        handler(move |msg| {
            Box::pin(async move {
                let path = msg
                    .body
                    .get("packagePath")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let force = msg
                    .body
                    .get("force")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match services::InstallerService::global()
                    .install(&path, force)
                    .await
                {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    // Remove(package, purge?) -> { package, registeredApps }
    bus.router().register_handler(
        "org.geckoOS.installer",
        "Remove",
        handler(move |msg| {
            Box::pin(async move {
                let package = msg
                    .body
                    .get("package")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let purge = msg
                    .body
                    .get("purge")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match services::InstallerService::global()
                    .remove(&package, purge)
                    .await
                {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    // ListInstalled() -> { packages: [...], count }
    bus.router().register_handler(
        "org.geckoOS.installer",
        "ListInstalled",
        handler(move |msg| {
            Box::pin(async move {
                match services::InstallerService::global().list_installed() {
                    Ok(result) => Ok(Message::method_return(msg.header.message_id, result)),
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
                }
            })
        })
        .into(),
    );

    info!("✓ Installer service handlers registered");
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
                    Err(e) => Ok(Message::error(msg.header.message_id, &format!("{e:#}"))),
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
