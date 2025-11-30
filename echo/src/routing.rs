use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;
use tracing;

use crate::Message;

/// Message router for delivery to destinations
pub struct MessageRouter {
    routes: Arc<DashMap<String, Vec<Route>>>,
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
        })
    }

    pub async fn route_message(&self, message: Message) -> Result<Message> {
        tracing::trace!(
            "Routing message: dest={}, method={}",
            message.header.destination,
            message.header.method
        );

        // For now, return a success response
        // In production, this would:
        // 1. Look up destination service
        // 2. Find delivery route
        // 3. Send to service
        // 4. Return response

        Ok(Message::method_return(
            &message.header.message_id,
            serde_json::json!({ "status": "ok" }),
        )?)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_router_init() {
        let router = MessageRouter::new().unwrap();
        assert!(router.get_all_routes().is_empty());
    }

    #[tokio::test]
    async fn test_route_addition() {
        let router = MessageRouter::new().unwrap();
        router.add_route("org.geckoOS.calculator", "Math.*", 100);

        let routes = router.get_routes("org.geckoOS.calculator");
        assert_eq!(routes.len(), 1);
    }
}
