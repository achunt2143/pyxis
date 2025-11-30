use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;
use tracing;

use crate::{Message, HandlerFn};

/// Signal bus for broadcasting signals across services
pub struct SignalBus {
    subscribers: Arc<DashMap<String, Vec<(String, Arc<HandlerFn>)>>>,
}

impl SignalBus {
    pub fn new() -> Result<Self> {
        tracing::info!("Initializing Signal Bus");

        Ok(SignalBus {
            subscribers: Arc::new(DashMap::new()),
        })
    }

    pub async fn subscribe(
        &self,
        interface: &str,
        subscription_id: String,
        handler: HandlerFn,
    ) -> Result<()> {
        let handler_arc = Arc::new(handler);

        self.subscribers
            .entry(interface.to_string())
            .or_insert_with(Vec::new)
            .push((subscription_id.clone(), handler_arc));

        tracing::debug!("Subscribed to signal: {} ({})", interface, subscription_id);
        Ok(())
    }

    pub async fn unsubscribe(&self, subscription_id: &str) -> Result<()> {
        for mut entry in self.subscribers.iter_mut() {
            entry
                .value_mut()
                .retain(|(id, _)| id != subscription_id);
        }

        tracing::debug!("Unsubscribed from signal: {}", subscription_id);
        Ok(())
    }

    pub async fn broadcast(&self, message: Message) -> Result<()> {
        let interface = &message.header.interface;

        tracing::trace!("Broadcasting signal: {}", interface);

        if let Some(subscribers) = self.subscribers.get(interface) {
            for (_, handler) in subscribers.iter() {
                match handler(message.clone()) {
                    Ok(_) => {
                        tracing::trace!("Signal delivered");
                    }
                    Err(e) => {
                        tracing::warn!("Error delivering signal: {}", e);
                    }
                }
            }
        }

        Ok(())
    }

    pub fn subscriber_count(&self, interface: &str) -> usize {
        self.subscribers
            .get(interface)
            .map(|entry| entry.len())
            .unwrap_or(0)
    }

    pub fn total_subscribers(&self) -> usize {
        self.subscribers
            .iter()
            .map(|entry| entry.value().len())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_signal_bus_init() {
        let bus = SignalBus::new().unwrap();
        assert_eq!(bus.total_subscribers(), 0);
    }

    #[tokio::test]
    async fn test_signal_subscription() {
        let bus = SignalBus::new().unwrap();

        let handler: HandlerFn = Box::new(|_msg| {
            Ok(Message::signal("test", "test", serde_json::json!({}))?)
        });

        bus.subscribe("org.geckoOS.Process", "sub1".to_string(), handler)
            .await
            .unwrap();

        assert_eq!(bus.subscriber_count("org.geckoOS.Process"), 1);
    }
}
