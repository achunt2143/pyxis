pub mod message;
pub mod registry;
pub mod transport;
pub mod dbus;
pub mod routing;
pub mod signals;
pub mod services;

pub use message::{Message, MessageType, MessageHeader};
pub use registry::{ServiceRegistry, ServiceInfo};
pub use transport::{Transport, UnixSocketTransport};
pub use dbus::DBusCompatLayer;
pub use routing::MessageRouter;
pub use signals::SignalBus;
pub use services::{FilesystemService, AppManagerService, AppHost, AppManifest, AppSession, AppState, APPHOST_IFACE, PowerService, SettingsService, NetworkService, DeviceInfoService, NotificationStore, ProcessService};

use anyhow::Result;
use dashmap::DashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing;
use uuid::Uuid;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, mpsc, oneshot};

/// IPC Bus: Message passing framework for GeckoOS
/// Enables inter-process communication between system components
pub struct IPCBus {
    registry: Arc<ServiceRegistry>,
    router: Arc<MessageRouter>,
    transport: Arc<UnixSocketTransport>,
    dbus_compat: Arc<DBusCompatLayer>,
    signal_bus: Arc<SignalBus>,
    active_connections: Arc<DashMap<String, BusConnection>>,
    message_handlers: Arc<DashMap<String, Arc<HandlerFn>>>,
    connection_senders: Arc<DashMap<String, mpsc::Sender<ForwardRequest>>>,
    // REAL GAP, found via a real report (2026-07-07): SignalBus::broadcast()
    // (signals.rs) only invokes IN-PROCESS Rust callback subscribers
    // (services statically linked into this same daemon binary, e.g. Evo's
    // own nova-lifecycle subscription) -- it never writes anything to a
    // remote socket client's connection at all. That left every JS bridge
    // client (PyxisBridgeParent.sys.mjs's EchoConnection) with no way to
    // ever receive a signal over the wire, even after correctly completing
    // a method call (e.g. terminal.Open) and registering a subscription --
    // confirmed by adding parent-side diagnostics and observing the
    // subscription registered fine, but the signal-dispatch function was
    // never even called, because nothing was pushing the SIGNAL message
    // back out over any socket. `broadcast_senders` is a parallel,
    // fire-and-forget analogue of `connection_senders` (which is a
    // request/response mechanism via `ForwardRequest`'s mandatory
    // `response_tx` -- signals have no reply, so reusing that would leak a
    // pending_replies entry per signal with nothing to ever resolve it).
    broadcast_senders: Arc<DashMap<String, mpsc::Sender<Message>>>,
    external_services: Arc<DashMap<String, String>>,
}

// pub type HandlerFn = Box<dyn Fn(Message) -> Pin<Box<dyn Future<Output = Result<Message>> + Send>> + Send + Sync>;
// MARKER TRAIT: Makes handlers cloneable
pub trait CloneableHandler: Fn(Message) -> Pin<Box<dyn Future<Output = Result<Message>> + Send>> + Send + Sync + 'static {}
impl<F> CloneableHandler for F 
where 
    F: Fn(Message) -> Pin<Box<dyn Future<Output = Result<Message>> + Send>> + Send + Sync + Clone + 'static,
{}

// NEW HandlerFn - now cloneable!
pub type HandlerFn = Arc<dyn CloneableHandler>;

// Helper to convert closures to HandlerFn
pub fn handler<F>(f: F) -> HandlerFn 
where 
    F: Fn(Message) -> Pin<Box<dyn Future<Output = Result<Message>> + Send>> + Send + Sync + Clone + 'static,
{
    Arc::new(f)
}


#[derive(Debug, Clone)]
pub struct BusConnection {
    pub id: String,
    pub service_name: Option<String>,
    pub connected_at: String,
}

pub struct ForwardRequest {
    pub message: Message,
    pub response_tx: oneshot::Sender<anyhow::Result<Message>>,
}

impl IPCBus {
    /// Initialize IPC Bus
    pub async fn new(socket_path: PathBuf) -> Result<Self> {
        tracing::info!("Initializing IPC Bus");
        
        let registry = Arc::new(ServiceRegistry::new()?);
        let router = Arc::new(MessageRouter::new()?);
        let transport = Arc::new(UnixSocketTransport::new(&socket_path)?);
        let dbus_compat = Arc::new(DBusCompatLayer::new()?);
        let signal_bus = Arc::new(SignalBus::new()?);

        let bus = IPCBus {
            registry,
            router,
            transport,
            dbus_compat,
            signal_bus,
            active_connections: Arc::new(DashMap::new()),
            message_handlers: Arc::new(DashMap::new()),
            external_services: Arc::new(DashMap::new()),
            connection_senders: Arc::new(DashMap::new()),
            broadcast_senders: Arc::new(DashMap::new()),
        };

        tracing::info!("IPC Bus initialized successfully");
        Ok(bus)
    }

    /// Start the IPC bus daemon
    pub async fn start(&self) -> Result<()> {
        self.transport.bind().await?;
        // tracing::info!("IPC Bus listening on: {:?}", self.transport. .socket_path());
        Ok(())
    }

    /// Register a service on the bus
    pub async fn register_service(&self, name: &str, interface: ServiceInterface) -> Result<String> {
        tracing::info!("Registering service: {}", name);
        let service_id = self.registry.register_service(name, interface).await?;
        tracing::info!("Service registered: {} ({})", name, service_id);
        Ok(service_id)
    }

     pub async fn register_external_service(
        &self,
        service_name: &str,
        connection_id: &str,
    ) {
        self.external_services
            .insert(service_name.to_string(), connection_id.to_string());
        if let Some(mut conn) = self.active_connections.get_mut(connection_id) {
            conn.service_name = Some(service_name.to_string());
        }
        tracing::info!(
            "External service '{}' registered by connection {}",
            service_name,
            connection_id
        );
    }

    pub async fn get_connection_for_service(
        &self,
        service_name: &str,
    ) -> Option<String> {
        self.external_services
            .get(service_name)
            .map(|v| v.value().clone())
    }

    /// Unregister a service
    pub async fn unregister_service(&self, service_id: &str) -> Result<()> {
        tracing::info!("Unregistering service: {}", service_id);
        self.registry.unregister_service(service_id).await?;
        Ok(())
    }

    /// Get service information
    pub async fn get_service(&self, service_name: &str) -> Result<ServiceInfo> {
        self.registry.get_service(service_name).await
    }

    /// List all registered services
    pub async fn list_services(&self) -> Result<Vec<ServiceInfo>> {
        self.registry.list_services().await
    }

    /// Send a message to a service
    pub async fn send_message(&self, message: Message) -> Result<Message> {
        tracing::debug!(
            "Sending message: destination={}, method={}",
            message.header.destination,
            message.header.method
        );

        // Route the message
        let response = self.router.route_message(message).await?;
        Ok(response)
    }

    /// Register a sender for a connection so others can forward to it
    pub fn add_connection_sender(&self, id: String, tx: mpsc::Sender<ForwardRequest>) {
        self.connection_senders.insert(id, tx);
    }

    /// Register a fire-and-forget sender for a connection, used to push
    /// SIGNAL messages out over that connection's socket (see this file's
    /// `broadcast_senders` field doc comment for why this is separate from
    /// `connection_senders`/`ForwardRequest`).
    pub fn add_broadcast_sender(&self, id: String, tx: mpsc::Sender<Message>) {
        self.broadcast_senders.insert(id, tx);
    }

    pub fn remove_connection(&self, id: &str) {
        self.active_connections.remove(id);
        self.connection_senders.remove(id);
        self.broadcast_senders.remove(id);
        self.external_services.retain(|_, conn_id| conn_id != id);
    }

    /// Pushes `message` out over every currently-connected socket client
    /// (fire-and-forget, no reply expected) -- the real wire-level signal
    /// delivery `SignalBus::broadcast()` alone does not provide. A send
    /// failure just means that connection's task has already exited/is
    /// backed up; harmless to ignore since `remove_connection` handles
    /// actual cleanup when a connection closes.
    pub async fn broadcast_to_all_connections(&self, message: &Message) {
        for entry in self.broadcast_senders.iter() {
            let _ = entry.value().send(message.clone()).await;
        }
    }

    /// Forwarding logic: sends a request to the target connection's task
    pub async fn forward_call(&self, target_id: &str, msg: Message) -> anyhow::Result<Message> {
        tracing::info!("Trying to forward to this connection now {}", target_id);
        let sender = self.connection_senders.get(target_id)
            .ok_or_else(|| anyhow::anyhow!("Connection {} no longer active", target_id))?
            .clone();

        let (tx, rx) = oneshot::channel();
        sender.send(ForwardRequest { message: msg, response_tx: tx }).await
            .map_err(|_| anyhow::anyhow!("Failed to communicate with connection task"))?;

        // Wait for the specific response to this message
        rx.await.map_err(|_| anyhow::anyhow!("Connection task dropped the request"))?
    }

    // pub async fn forward_to_connection(
    //     &self,
    //     target_conn_id: &str,
    //     msg: Message,
    // ) -> anyhow::Result<Message> {
    //     use crate::transport::UnixSocketTransport;

    //     // get raw UnixStream from transport
    //     let mut stream = self
    //         .transport
    //         .get_stream(target_conn_id)
    //         .await
    //         .ok_or_else(|| anyhow::anyhow!("Connection not found for service"))?;

    //     let data = serde_json::to_vec(&msg)?;
    //     UnixSocketTransport::send_message(&mut stream, &data).await?;

    //     let resp_bytes = UnixSocketTransport::recv_message(&mut stream).await?;
    //     let resp: Message = serde_json::from_slice(&resp_bytes)?;
    //     Ok(resp)
    // }

    /// Call a method on a service and wait for response
    pub async fn call_method(
        &self,
        service: &str,
        interface: &str,
        method: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        tracing::debug!("Method call: {}.{}.{}", service, interface, method);

        let message = Message::method_call(service, interface, method, args)?;

        // Route to an in-process handler if one exists, otherwise FORWARD to
        // the socket connection that owns the service.
        //
        // Without the forward branch this only ever consulted the in-process
        // router, so any in-process service calling an *external* one got
        // "No handler registered for <service>.<method>" even though the
        // service was registered and connected. `daemon.rs`'s socket path has
        // always done this lookup for messages arriving over a socket — the
        // gap was only for calls originating inside the daemon.
        //
        // Concretely, this is the last link of alpha2 #10: `Nova::focus` runs
        // in-process and calls `org.geckoOS.evo` `FocusWindow`; Evo is an
        // external client, so the call was rejected by the router and
        // restore-from-dock could never work — even once the session and its
        // surfaces were correct. Measured on-device as
        // "Method call failed: No handler registered for
        // org.geckoOS.evo.FocusWindow" with the right window id in hand.
        let response = if self.router().has_handler(service, method) {
            self.send_message(message).await?
        } else if let Some(conn_id) = self.get_connection_for_service(service).await {
            tracing::debug!(
                "no in-process handler for {}.{}; forwarding to connection {}",
                service,
                method,
                conn_id
            );
            self.forward_call(&conn_id, message).await?
        } else {
            // Fall through to the router so the caller still gets its
            // existing "No handler registered" error rather than a new one.
            self.send_message(message).await?
        };

        match response.header.message_type {
            MessageType::MethodReturn => Ok(response.body),
            MessageType::Error => Err(anyhow::anyhow!(
                "Method call failed: {}",
                response.body.get("message").and_then(|v| v.as_str()).unwrap_or("unknown error")
            )),
            _ => Err(anyhow::anyhow!("Unexpected response type")),
        }
    }

    /// Emit a signal on the bus
    pub async fn emit_signal(
        &self,
        interface: &str,
        signal_name: &str,
        data: serde_json::Value,
    ) -> Result<()> {
        tracing::debug!("Emitting signal: {}.{}", interface, signal_name);
        let message = Message::signal(interface, signal_name, data)?;
        self.signal_bus.broadcast(message.clone()).await?;
        // Real fix (2026-07-07): signal_bus().broadcast() alone only
        // reaches in-process Rust subscribers -- this is the single
        // shared entry point every in-process service (Terminal, etc.)
        // calls to emit a signal, so this is where remote socket clients
        // (the JS bridge's EchoConnection, in particular) need to get
        // pushed the message too. See broadcast_to_all_connections's doc
        // comment for the full story.
        self.broadcast_to_all_connections(&message).await;
        Ok(())
    }

    /// Subscribe to signals from an interface
    pub async fn subscribe_signal(&self, interface: &str, callback: HandlerFn) -> Result<String> {
        let subscription_id = Uuid::new_v4().to_string();
        self.signal_bus.subscribe(interface, subscription_id.clone(), callback).await?;
        tracing::debug!("Subscribed to signal: {} ({})", interface, subscription_id);
        Ok(subscription_id)
    }

    /// Unsubscribe from signals
    pub async fn unsubscribe_signal(&self, subscription_id: &str) -> Result<()> {
        self.signal_bus.unsubscribe(subscription_id).await?;
        Ok(())
    }

    /// Handle new client connection
    pub async fn accept_connection(&self) -> Result<(String, UnixStream)> {
        let stream = self.transport.accept().await?;
        let connection_id = Uuid::new_v4().to_string();
        
        let connection = BusConnection {
            id: connection_id.clone(),
            service_name: None,
            connected_at: chrono::Utc::now().to_rfc3339(),
        };
        
        self.active_connections.insert(connection_id.clone(), connection);
        
        tracing::debug!("Accepted new connection: {}", connection_id);
        Ok((connection_id, stream))
    }

    pub async fn register_connection(&self, connection_id: String) {
        if let Some(mut conn) = self.active_connections.get_mut(&connection_id) {
            conn.service_name = None;
        }
        tracing::debug!("Registered connection {}", connection_id);
    }

    // /// Remove a connection
    // pub fn remove_connection(&self, connection_id: &str) {
    //     self.active_connections.remove(connection_id);
    //     // Remove all services owned by this connection
    //     let to_remove: Vec<String> = self
    //         .external_services
    //         .iter()
    //         .filter(|entry| entry.value() == connection_id)
    //         .map(|entry| entry.key().clone())
    //         .collect();
    //     for svc in to_remove {
    //         self.external_services.remove(&svc);
    //         tracing::debug!("Unregistered external service '{}' from {}", svc, connection_id);
    //     }
    //     tracing::debug!("Removed connection: {}", connection_id);
    // }

    /// Get D-Bus compatibility layer
    pub fn dbus(&self) -> Arc<DBusCompatLayer> {
        self.dbus_compat.clone()
    }

    /// Register message handler
    pub async fn register_handler(&self, service: &str, method: &str, handler: HandlerFn) {
        let key = format!("{}.{}", service, method);
        self.message_handlers.insert(key, handler.clone().into());
        self.router.register_handler(service, method, handler.into());
        tracing::debug!("Handler registered: {}.{}", service, method);
    }

    /// Get all connections count
    pub fn connection_count(&self) -> usize {
        self.active_connections.len()
    }

    /// Get all services count
    pub async fn service_count(&self) -> Result<usize> {
        self.registry.service_count().await
    }

    /// Get router reference
    pub fn router(&self) -> Arc<MessageRouter> {
        self.router.clone()
    }

    /// Get signal bus reference
    pub fn signal_bus(&self) -> Arc<SignalBus> {
        self.signal_bus.clone()
    }

    /// Get registry reference
    pub fn registry(&self) -> &ServiceRegistry {
        &self.registry
    }

    /// Convenience helper: check if a local handler is registered
    pub fn has_handler(&self, service: &str, method: &str) -> bool {
        self.router.has_handler(service, method)
    }
}

/// Echo Client - For services to connect to the bus
#[derive(Debug, Clone)]
pub struct EchoClient {
    stream: Arc<tokio::sync::Mutex<UnixStream>>,
    // pending_replies: Arc<DashMap<String, oneshot::Sender<Message>>>,
    // service_name: Option<String>,
}

impl EchoClient {
    /// Connect to Echo daemon
    pub async fn connect() -> Result<Self> {
        let socket_path = get_socket_path();
        let transport = UnixSocketTransport::new(&socket_path)?;
        let stream = transport.connect().await?;
        Ok(EchoClient {
            stream: Arc::new(Mutex::new(stream)),
        })
    }

    /// Connect to specific socket path
    pub async fn connect_to(socket_path: &Path) -> Result<Self> {
        tracing::info!("Connecting to Echo daemon: {:?}", socket_path);
        
        let transport = UnixSocketTransport::new(socket_path)?;
        let stream = transport.connect().await?;
        
        Ok(EchoClient {
            stream: Arc::new(tokio::sync::Mutex::new(stream)),
            // pending_replies: Arc::new(DashMap::new()),
            // service_name: None,
        })
    }

    /// The next message the daemon sends this connection unasked: the bus
    /// broadcasts every signal to every connection. For `echo-cli watch`.
    pub async fn next_message(&self) -> Result<Message> {
        let mut stream = self.stream.lock().await;
        let data = UnixSocketTransport::recv_message(&mut *stream).await?;
        Ok(serde_json::from_slice(&data)?)
    }

    /// Get default socket path
    pub fn default_socket_path() -> PathBuf {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("geckoOS").join("echo.sock")
        } else {
            PathBuf::from("/tmp/geckoOS/echo.sock")
        }
    }

    /// Call a method on a service
    pub async fn call_method(
        &self,
        service: &str,
        interface: &str,
        method: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let message = Message::method_call(service, interface, method, args)?;
        let mut stream = self.stream.lock().await;
        let data = serde_json::to_vec(&message)?;
        UnixSocketTransport::send_message(&mut *stream, &data).await?;

        let response_data = UnixSocketTransport::recv_message(&mut *stream).await?;
        let response: Message = serde_json::from_slice(&response_data)?;

        match response.header.message_type {
            MessageType::MethodReturn => Ok(response.body),
            // Accept `error` as well as the canonical `message`. Evo wrote
            // the former for a long time, so every error it returned
            // surfaced as "Unknown error" and the actual reason was lost on
            // the wire. Evo is fixed, but reading both means the next
            // service to get this wrong degrades to a wrong-looking field
            // rather than to no information at all.
            MessageType::Error => Err(anyhow::anyhow!(
                "{}",
                response
                    .body
                    .get("message")
                    .or_else(|| response.body.get("error"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown error")
            )),
            _ => Err(anyhow::anyhow!("Unexpected response type")),
        }
    }

    /// Register a service
    pub async fn register_service(&mut self, name: &str, interface: ServiceInterface) -> Result<String> {
        let result = self.call_method(
            "org.geckoOS.Echo",
            "Bus",
            "RegisterService",
            serde_json::json!({
                "name": name,
                "interface": interface,
            }),
        ).await?;
        
        // self.service_name = Some(name.to_string());
        Ok(result["service_id"].as_str().unwrap_or("").to_string())
    }

    /// Emit a signal
    pub async fn emit_signal(
        &self,
        interface: &str,
        signal_name: &str,
        data: serde_json::Value,
    ) -> Result<()> {
        let message = Message::signal(interface, signal_name, data)?;
        
        let mut stream = self.stream.lock().await;
        let data = serde_json::to_vec(&message)?;
        UnixSocketTransport::send_message(&mut *stream, &data).await?;
        
        Ok(())
    }
}

fn get_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join("geckoOS/echo.sock")
    } else {
        PathBuf::from("/tmp/geckoOS/echo.sock")
    }
}

/// Service interface definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInterface {
    pub name: String,
    pub methods: Vec<MethodDefinition>,
    pub signals: Vec<SignalDefinition>,
    pub properties: Vec<PropertyDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodDefinition {
    pub name: String,
    pub input_args: Vec<ArgumentDefinition>,
    pub output_args: Vec<ArgumentDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalDefinition {
    pub name: String,
    pub arguments: Vec<ArgumentDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyDefinition {
    pub name: String,
    pub property_type: String,
    pub access: PropertyAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PropertyAccess {
    Read,
    Write,
    ReadWrite,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgumentDefinition {
    pub name: String,
    pub arg_type: String,
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use tempFile::TempDir;

//     #[tokio::test]
//     async fn test_ipc_bus_initialization() {
//         let temp_dir = TempDir::new().unwrap();
//         let socket_path = temp_dir.path().join("test.sock");
//         let bus = IPCBus::new(socket_path).await.unwrap();
//         assert_eq!(bus.connection_count(), 0);
//     }

//     #[tokio::test]
//     async fn test_service_count() {
//         let temp_dir = TempDir::new().unwrap();
//         let socket_path = temp_dir.path().join("test.sock");
//         let bus = IPCBus::new(socket_path).await.unwrap();
//         let count = bus.service_count().await.unwrap();
//         assert_eq!(count, 0);
//     }
// }
