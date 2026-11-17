use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing::{debug, error, info, trace, warn};
use crate::{Message, MessageType, HandlerFn};

/// A route pattern with a priority
#[derive(Debug, Clone)]
pub struct Route {
    pub pattern: String,
    pub priority: u8,
}

/// Message router for delivery to destinations
pub struct MessageRouter {
    routes: Arc<DashMap<String, Vec<Route>>>,
    handlers: Arc<DashMap<String, Arc<HandlerFn>>>,
    pending_replies: Arc<DashMap<String, oneshot::Sender<Message>>>,
}

impl MessageRouter {
    /// Initialize a new router
    pub fn new() -> Result<Self> {
        info!("Initializing Message Router");
        Ok(MessageRouter {
            routes: Arc::new(DashMap::new()),
            handlers: Arc::new(DashMap::new()),
            pending_replies: Arc::new(DashMap::new()),
        })
    }

    /// Route a message based on its type
    pub async fn route_message(&self, message: Message) -> Result<Message> {
        info!(
            "Routing message to {} ({})",
            message.header.destination,
            message.header.method
        );

        match message.header.message_type {
            MessageType::MethodCall | MessageType::EchoServiceCall | MessageType::EchoSubscribe | MessageType::EchoUnsubscribe | MessageType::GeckoosPBridgeEvent | MessageType::Ping | MessageType::Status => {
                let handler_key = format!("{}.{}", message.header.destination, message.header.method);
                // These were at INFO, which meant every single message wrote
                // the full body AND a ~100-entry dump of every registered
                // handler key into echo.log — kilobytes per call, on the hot
                // path, drowning the lines that matter. Kept at debug/trace
                // because the handler dump is genuinely useful when a call
                // gets "No handler registered", just not 90 times a second.
                debug!("handler key {} and message {:?}", handler_key, message);
                if tracing::enabled!(tracing::Level::TRACE) {
                    trace!(
                        "All handlers: {:?}",
                        self.handlers.iter().map(|e| e.key().clone()).collect::<Vec<_>>()
                    );
                }

                if let Some(handler) = self.handlers.get(&handler_key) {
                    match handler(message.clone()).await {
                        Ok(response) => Ok(response),
                        Err(e) => {
                            error!("Handler error: {}", e);
                            Ok(Message::error(
                                message.header.message_id,
                                &format!("Handler failed: {}", e),
                            ))
                        }
                    }
                } else {
                    warn!("No handler for: {}", handler_key);
                    Ok(Message::error(
                        message.header.message_id,
                        &format!("No handler registered for {}", handler_key),
                    ))
                }
            }
            MessageType::MethodReturn | MessageType::Error | MessageType::HandshakeAck | MessageType::Pong | MessageType::Success => {
                if let Some(reply_to_id) = &message.header.reply_to {
                    if let Some((_, tx)) = self.pending_replies.remove(reply_to_id) {
                        let _ = tx.send(message.clone());
                    }
                }
                Ok(message)
            }
            MessageType::Signal | MessageType::Handshake => Ok(message),
        }
    }

    /// Register a handler for a specific service and method
    pub fn register_handler(&self, service: &str, method: &str, handler: Arc<HandlerFn>) {
        let key = format!("{}.{}", service, method);
        self.handlers.insert(key.clone(), handler);
        debug!("Registered handler: {}", key);
    }

    /// NEW: check if a handler exists for service.method
    pub fn has_handler(&self, service: &str, method: &str) -> bool {
        let key = format!("{}.{}", service, method);
        self.handlers.contains_key(&key)
    }

    /// Unregister a handler
    pub fn unregister_handler(&self, service: &str, method: &str) {
        let key = format!("{}.{}", service, method);
        self.handlers.remove(&key);
        debug!("Unregistered handler: {}", key);
    }

    /// Register a pending reply channel
    pub fn register_pending_reply(&self, message_id: String, sender: oneshot::Sender<Message>) {
        self.pending_replies.insert(message_id, sender);
    }

    /// Number of registered handlers
    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }

    /// Every registered handler, as `(destination, method)` pairs.
    ///
    /// This is the only complete view of what the bus can actually answer.
    /// `ServiceRegistry` (registry.rs) holds *only* services that called the
    /// `RegisterService` method over the wire -- the in-process services
    /// echo-daemon installs at startup (filesystem, power, nova, ...) go
    /// straight into `self.handlers` via `register_handler` and never appear
    /// there. Anything reporting "what services exist" therefore has to read
    /// this table, not just the registry. See
    /// `services::system::SystemService::list_services`.
    ///
    /// Handler keys are stored as `"{destination}.{method}"` and destinations
    /// themselves contain dots (`org.geckoOS.filesystem`), so the split is on
    /// the LAST dot, not the first.
    pub fn handler_entries(&self) -> Vec<(String, String)> {
        self.handlers
            .iter()
            .filter_map(|entry| {
                let key = entry.key();
                key.rsplit_once('.')
                    .map(|(dest, method)| (dest.to_string(), method.to_string()))
            })
            .collect()
    }

    // --- ROUTES MANAGEMENT ---
    
    /// Add a route pattern for a destination
    pub fn add_route(&self, destination: &str, pattern: &str, priority: u8) {
        let route = Route {
            pattern: pattern.to_string(),
            priority,
        };
        self.routes
            .entry(destination.to_string())
            .or_insert_with(Vec::new)
            .push(route);
    }

    /// Get all routes for a specific destination
    pub fn get_routes(&self, destination: &str) -> Vec<Route> {
        self.routes
            .get(destination)
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Get all routes in the router
    pub fn get_all_routes(&self) -> Vec<(String, Vec<Route>)> {
        self.routes
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::Message;

    #[tokio::test]
    async fn test_router_init() {
        let router = MessageRouter::new().unwrap();
        assert!(router.get_all_routes().is_empty());
        assert_eq!(router.handler_count(), 0);
    }

    #[tokio::test]
    async fn test_route_addition() {
        let router = MessageRouter::new().unwrap();
        router.add_route("org.geckoOS.calculator", "Math.*", 100);
        let routes = router.get_routes("org.geckoOS.calculator");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].pattern, "Math.*");
        assert_eq!(routes[0].priority, 100);
    }

    // #[tokio::test]
    // async fn test_handler_registration() {
    //     let router = MessageRouter::new().unwrap();
    //     let handler: Arc<HandlerFn> = Box::new(|msg: Message| {
    //         Box::pin(async move {
    //     let path = msg.body.get("path").and_then(|v| v.as_str()).unwrap_or("/").to_string();
    //     // ... rest of handler
    //     Ok(Message::method_return(msg.header.message_id, json!({"entries": []})))
    // })
    //     });
    //     router.register_handler("org.geckoOS.test", "TestMethod", handler);
    //     assert_eq!(router.handler_count(), 1);
    // }

    /// A writer the test reads back what the subscriber logged from.
    #[derive(Clone, Default)]
    struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Captured;
        fn make_writer(&'a self) -> Captured {
            self.clone()
        }
    }

    /// echo-daemon logs at DEBUG, and the router prints the whole message
    /// there: a password in the body must not reach the log, whichever call
    /// carries it, and a call that finds no handler must not either.
    #[tokio::test]
    async fn routing_a_password_call_logs_no_password() {
        let secret = "hunter2-do-not-log";
        let log = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(log.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let router = MessageRouter::new().unwrap();
        for (destination, method) in [("org.geckoOS.session", "unlock"), ("org.geckoOS.accounts", "setPassword")] {
            router.register_handler(
                destination,
                method,
                Arc::new(crate::handler(|msg| {
                    Box::pin(async move { Ok(Message::method_return(msg.header.message_id, json!({"ok": true}))) })
                })),
            );
        }
        for (destination, method) in [
            ("org.geckoOS.session", "unlock"),
            ("org.geckoOS.accounts", "setPassword"),
            // Nothing registered here: the "No handler" path.
            ("org.geckoOS.session", "Unlock"),
        ] {
            let body = json!({ "password": secret, "current": secret, "new": secret });
            let msg = Message::method_call(destination, destination, method, body).unwrap();
            router.route_message(msg).await.unwrap();
        }

        let logged = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        // The router did log these calls (at debug), so the test can fail.
        assert!(logged.contains("handler key org.geckoOS.session.unlock"), "{logged}");
        assert!(logged.contains(crate::message::REDACTED), "{logged}");
        assert!(!logged.contains(secret), "{logged}");
    }
}
