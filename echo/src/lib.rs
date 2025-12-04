pub mod message;
pub mod registry;
pub mod transport;
pub mod dbus;
pub mod routing;
pub mod signals;

pub use message::{Message, MessageType, MessageHeader};
pub use registry::{ServiceRegistry, ServiceInfo};
pub use transport::{Transport, UnixSocketTransport};
pub use dbus::DBusCompatLayer;
pub use routing::MessageRouter;
pub use signals::SignalBus;

use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing;
use uuid::Uuid;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

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
}

pub type HandlerFn = Box<dyn Fn(Message) -> Result<Message> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct BusConnection {
    pub id: String,
    pub service_name: Option<String>,
    pub connected_at: String,
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
        };

        tracing::info!("IPC Bus initialized successfully");
        Ok(bus)
    }

    /// Start the IPC bus daemon
    pub async fn start(&self) -> Result<()> {
        self.transport.bind().await?;
        tracing::info!("IPC Bus listening on: {:?}", self.transport.socket_path());
        Ok(())
    }

    /// Register a service on the bus
    pub async fn register_service(&self, name: &str, interface: ServiceInterface) -> Result<String> {
        tracing::info!("Registering service: {}", name);
        let service_id = self.registry.register_service(name, interface).await?;
        tracing::info!("Service registered: {} ({})", name, service_id);
        Ok(service_id)
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
        let response = self.send_message(message).await?;

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
        self.signal_bus.broadcast(message).await?;
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

    /// Remove a connection
    pub fn remove_connection(&self, connection_id: &str) {
        self.active_connections.remove(connection_id);
        tracing::debug!("Removed connection: {}", connection_id);
    }

    /// Get D-Bus compatibility layer
    pub fn dbus(&self) -> Arc<DBusCompatLayer> {
        self.dbus_compat.clone()
    }

    /// Register message handler
    pub fn register_handler(&self, service: &str, method: &str, handler: HandlerFn) {
        self.router.register_handler(service, method, handler);
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
    pub fn router(&self) -> &MessageRouter {
        &self.router
    }

    /// Get signal bus reference
    pub fn signal_bus(&self) -> &SignalBus {
        &self.signal_bus
    }

    /// Get registry reference
    pub fn registry(&self) -> &ServiceRegistry {
        &self.registry
    }
}

/// Echo Client - For services to connect to the bus
pub struct EchoClient {
    stream: Arc<tokio::sync::Mutex<UnixStream>>,
    pending_replies: Arc<DashMap<String, oneshot::Sender<Message>>>,
    service_name: Option<String>,
}

impl EchoClient {
    /// Connect to Echo daemon
    pub async fn connect() -> Result<Self> {
        Self::connect_to(&Self::default_socket_path()).await
    }

    /// Connect to specific socket path
    pub async fn connect_to(socket_path: &Path) -> Result<Self> {
        tracing::info!("Connecting to Echo daemon: {:?}", socket_path);
        
        let transport = UnixSocketTransport::new(socket_path)?;
        let stream = transport.connect().await?;
        
        Ok(EchoClient {
            stream: Arc::new(tokio::sync::Mutex::new(stream)),
            pending_replies: Arc::new(DashMap::new()),
            service_name: None,
        })
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
        let message_id = message.header.message_id.clone();
        
        // Register pending reply
        let (tx, rx) = oneshot::channel();
        self.pending_replies.insert(message_id.clone(), tx);
        
        // Send message
        let mut stream = self.stream.lock().await;
        let data = serde_json::to_vec(&message)?;
        UnixSocketTransport::send_message(&mut *stream, &data).await?;
        
        // Wait for reply with timeout
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            rx
        ).await??;
        
        match response.header.message_type {
            MessageType::MethodReturn => Ok(response.body),
            MessageType::Error => Err(anyhow::anyhow!(
                "Method call failed: {}",
                response.body.get("message").and_then(|v| v.as_str()).unwrap_or("unknown")
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
        
        self.service_name = Some(name.to_string());
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
