use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing;
use crate::{Message, MessageType, HandlerFn};

/// Message router for delivery to destinations
pub struct MessageRouter {
    routes: Arc<DashMap<String, Vec<Route>>>,
    handlers: Arc<DashMap<String, Arc<HandlerFn>>>,
    pending_replies: Arc<DashMap<String, oneshot::Sender<Message>>>,
}

#[derive(Debug, Clone)]
struct Route {
    pattern: String,
    priority: u8,
}

impl MessageRouter {
    pub fn new() -> Result<Self> {
        tracing::info!("Initializing Message Router");
        Ok(MessageRouter {
            routes: Arc::new(DashMap::new()),
            handlers: Arc::new(DashMap::new()),
            pending_replies: Arc::new(DashMap::new()),
        })
    }

    pub async fn route_message(&self, mut message: Message) -> Result<Message> {
        tracing::trace!(
            "Routing message: dest={}, method={}",
            message.header.destination,
            message.header.method
        );

        match message.header.message_type {
            MessageType::MethodCall => {
                // Look up handler for destination service
                let handler_key = format!("{}.{}", message.header.destination, message.header.method);
                
                if let Some(handler) = self.handlers.get(&handler_key) {
                    // Invoke handler
                    match handler(message.clone()) {
                        Ok(response) => Ok(response),
                        Err(e) => {
                            tracing::error!("Handler error: {}", e);
                            Message::error(
                                &message.header.message_id,
                                "handler_error",
                                &format!("Handler failed: {}", e),
                            )
                        }
                    }
                } else {
                    // No handler registered - return error
                    tracing::warn!("No handler for: {}", handler_key);
                    Message::error(
                        &message.header.message_id,
                        "no_handler",
                        &format!("No handler registered for {}", handler_key),
                    )
                }
            }
            MessageType::MethodReturn | MessageType::Error => {
                // This is a reply to a previous method call
                if let Some(reply_to) = &message.header.reply_to {
                    if let Some((_key, sender)) = self.pending_replies.remove(reply_to) {
                        let _ = sender.send(message.clone());
                    }
                }
                Ok(message)
            }
            MessageType::Signal => {
                // Signals don't get responses
                Ok(message)
            }
        }
    }

    pub fn register_handler(&self, service: &str, method: &str, handler: HandlerFn) {
        let key = format!("{}.{}", service, method);
        self.handlers.insert(key.clone(), Arc::new(handler));
        tracing::debug!("Registered handler: {}", key);
    }

    pub fn unregister_handler(&self, service: &str, method: &str) {
        let key = format!("{}.{}", service, method);
        self.handlers.remove(&key);
        tracing::debug!("Unregistered handler: {}", key);
    }

    pub fn register_pending_reply(&self, message_id: String, sender: oneshot::Sender<Message>) {
        self.pending_replies.insert(message_id, sender);
    }

    pub fn add_route(&self, destination: &str, pattern: &str, priority: u8) {
        let route = Route {
            pattern: pattern.to_string(),
            priority,
        };
        self.routes
            .entry(destination.to_string())
            .or_insert_with(Vec::new)
            .push(route);
        tracing::debug!(
            "Added route: {} -> {} (priority={})",
            destination,
            pattern,
            priority
        );
    }

    pub fn remove_route(&self, destination: &str) {
        self.routes.remove(destination);
        tracing::debug!("Removed route: {}", destination);
    }

    pub fn get_routes(&self, destination: &str) -> Vec<Route> {
        self.routes
            .get(destination)
            .map(|entry| entry.clone())
            .unwrap_or_default()
    }

    pub fn get_all_routes(&self) -> Vec<(String, Vec<Route>)> {
        self.routes
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[tokio::test]
    async fn test_handler_registration() {
        let router = MessageRouter::new().unwrap();
        let handler: HandlerFn = Box::new(|msg| {
            Message::method_return(&msg.header.message_id, serde_json::json!({"result": "ok"}))
        });
        router.register_handler("org.geckoOS.test", "TestMethod", handler);
        assert_eq!(router.handler_count(), 1);
    }
}
