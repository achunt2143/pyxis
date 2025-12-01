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
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

/// IPC Bus: Message passing framework for GeckoOS
/// Enables inter-process communication between system components
pub struct IPCBus {
    registry: Arc<ServiceRegistry>,
    router: Arc<MessageRouter>,
    transport: Arc<UnixSocketTransport>,
    dbus_compat: Arc<DBusCompatLayer>,
    signal_bus: Arc<SignalBus>,
    active_connections: Arc<DashMap<String, Arc<BusConnection>>>,
    message_handlers: Arc<DashMap<String, HandlerFn>>,
}

pub type HandlerFn = Box<dyn Fn(Message) -> Result<Message> + Send + Sync>;

pub struct BusConnection {
    id: String,
    service_name: Option<String>,
    socket_fd: i32,
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
    pub async fn send_message(&self, mut message: Message) -> Result<Message> {
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
                response.body.get("error").map(|e| e.to_string()).unwrap_or_default()
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
    pub async fn accept_connection(&self) -> Result<()> {
        // In production, this would:
        // 1. Accept connection on Unix socket
        // 2. Authenticate client
        // 3. Add to active connections
        // 4. Listen for messages

        tracing::debug!("Accepted new connection on IPC bus");
        Ok(())
    }

    /// Start the IPC bus listener
    pub async fn start(&self) -> Result<()> {
        tracing::info!("Starting IPC Bus listener");

        loop {
            self.accept_connection().await?;
        }
    }

    /// Get D-Bus compatibility layer
    pub fn dbus(&self) -> Arc<DBusCompatLayer> {
        self.dbus_compat.clone()
    }

    /// Register custom message handler
    pub fn register_handler(&self, message_type: &str, handler: HandlerFn) {
        self.message_handlers.insert(message_type.to_string(), handler);

        tracing::debug!("Registered message handler: {}", message_type);
    }

    /// Get all connections count
    pub fn connection_count(&self) -> usize {
        self.active_connections.len()
    }

    /// Get all services count
    pub async fn service_count(&self) -> Result<usize> {
        self.registry.service_count().await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ipc_bus_initialization() {
        let bus = IPCBus::new("/tmp/geckoOS-ipc.sock".into()).await.unwrap();
        assert_eq!(bus.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_service_count() {
        let bus = IPCBus::new("/tmp/geckoOS-ipc.sock".into()).await.unwrap();
        let count = bus.service_count().await.unwrap();
        assert_eq!(count, 0);
    }
}
