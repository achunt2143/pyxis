use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{self, debug, info, warn};

use crate::{Message, HandlerFn};

/// Signal bus for broadcasting signals across services
pub struct SignalBus {
    subscribers: Arc<DashMap<String, Vec<Subscriber>>>,
}

struct Subscriber {
    id: String,
    callback: Arc<HandlerFn>,
}

impl SignalBus {
    pub fn new() -> Result<Self> {
        info!("Initializing Signal Bus");
        Ok(SignalBus {
            subscribers: Arc::new(DashMap::new()),
        })
    }

    pub async fn broadcast(&self, message: Message) -> Result<()> {
    let interface = &message.header.interface;
    if let Some(subs) = self.subscribers.get(interface) {
        for sub in subs.iter() {
            // Since callback is async, we need to await it
            match (sub.callback)(message.clone()).await {
                Ok(_) => {},
                Err(e) => warn!("Signal subscriber error: {}", e),
            }
        }
    }
    Ok(())
}


    pub async fn subscribe(
        &self,
        interface: &str,
        subscription_id: String,
        callback: HandlerFn,
    ) -> Result<()> {
        let subscriber = Subscriber {
            id: subscription_id,
            callback: Arc::new(callback),
        };
        
        self.subscribers
            .entry(interface.to_string())
            .or_insert_with(Vec::new)
            .push(subscriber);
        
        debug!("Subscribed to signal: {}", interface);
        Ok(())
    }

    pub async fn unsubscribe(&self, subscription_id: &str) -> Result<()> {
        for mut entry in self.subscribers.iter_mut() {
            entry.value_mut().retain(|s| s.id != subscription_id);
        }
        debug!("Unsubscribed: {}", subscription_id);
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

        // let handler: HandlerFn = Box::new(|_msg| {
            // Ok(Message::signal("test", "test", serde_json::json!({}))?) => {}
        // });

        // bus.subscribe("org.geckoOS.Process", "sub1".to_string(), handler)
        //     .await
        //     .unwrap();

        assert_eq!(bus.subscriber_count("org.geckoOS.Process"), 0);
    }
}
