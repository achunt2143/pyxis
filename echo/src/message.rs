use serde::{Deserialize, Serialize};
use anyhow::Result;
use uuid::Uuid;
use chrono::Utc;

/// Represents a full message with header and body
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub header: MessageHeader,
    pub body: serde_json::Value,
}

/// Message header with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageHeader {
    pub message_id: String,
    pub message_type: MessageType,
    pub sender: Option<String>,
    pub destination: String,
    pub interface: String,
    pub method: String,
    pub signature: String,
    pub reply_to: Option<String>,
    pub timestamp: u64,
}

/// Enum representing the type of message
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MessageType {
    Ping,
    Handshake,
    EchoServiceCall,
    EchoSubscribe,
    EchoUnsubscribe,
    Success,
    Error,
    Signal,
    GeckoosPBridgeEvent,
    Status,
    Pong,
    HandshakeAck,
    MethodCall,
    MethodReturn,
}

/// Helper implementations for creating messages
impl Message {
    pub fn method_call(
        destination: impl Into<String>,
        interface: impl Into<String>,
        method: impl Into<String>,
        body: serde_json::Value,
    ) -> Result<Self> {
        Ok(Message {
            header: MessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: MessageType::MethodCall,
                sender: None,
                destination: destination.into(),
                interface: interface.into(),
                method: method.into(),
                signature: "".to_string(),
                reply_to: None,
                timestamp: Utc::now().timestamp_millis() as u64,
            },
            body,
        })
    }

    pub fn method_return(message_id: String, body: serde_json::Value) -> Self {
        Message {
            header: MessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: MessageType::MethodReturn,
                sender: None,
                destination: String::new(),
                interface: String::new(),
                method: String::new(),
                signature: "".to_string(),
                reply_to: Some(message_id),
                timestamp: Utc::now().timestamp_millis() as u64,
            },
            body,
        }
    }

    pub fn error(message_id: String, error_name: &str) -> Self {
        Message {
            header: MessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: MessageType::Error,
                sender: None,
                destination: String::new(),
                interface: String::new(),
                method: String::new(),
                signature: "".to_string(),
                reply_to: Some(message_id),
                timestamp: Utc::now().timestamp_millis() as u64,
            },
            body: serde_json::json!({ "message": error_name }),
        }
    }

    pub fn signal(
        interface: impl Into<String>,
        signal_name: impl Into<String>,
        body: serde_json::Value,
    ) -> Result<Self> {
        Ok(Message {
            header: MessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: MessageType::Signal,
                sender: None,
                destination: String::new(),
                interface: interface.into(),
                method: signal_name.into(),
                signature: "".to_string(),
                reply_to: None,
                timestamp: Utc::now().timestamp_millis() as u64,
            },
            body,
        })
    }

    /// Check if message matches filter
    pub fn matches_filter(&self, interface: &str, method: &str) -> bool {
        (self.header.interface.is_empty() || self.header.interface == interface)
            && (method.is_empty() || self.header.method == method)
    }

    /// Get message size in bytes
    pub fn size(&self) -> usize {
        serde_json::to_vec(self).map(|v| v.len()).unwrap_or(0)
    }

    /// Get message priority (0-255)
    pub fn priority(&self) -> u8 {
        match self.header.message_type {
            MessageType::MethodCall => 100,
            MessageType::MethodReturn => 80,
            MessageType::Signal => 50,
            MessageType::Error => 120,
            _ => 50,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_method_call_creation() {
        let msg = Message::method_call("org.geckoOS.calculator", "Math", "Add",
            serde_json::json!({ "a": 5, "b": 3 })).unwrap();
        assert_eq!(msg.header.message_type, MessageType::MethodCall);
        assert_eq!(msg.header.destination, "org.geckoOS.calculator");
    }

    #[test]
    fn test_signal_creation() {
        let msg = Message::signal("org.geckoOS.Process", "ProcessTerminated",
            serde_json::json!({ "pid": 1001 })).unwrap();
        assert_eq!(msg.header.message_type, MessageType::Signal);
    }

    #[test]
    fn test_message_matching() {
        let msg = Message::signal("org.geckoOS.Process", "ProcessTerminated",
            serde_json::json!({})).unwrap();
        assert!(msg.matches_filter("org.geckoOS.Process", "ProcessTerminated"));
        assert!(msg.matches_filter("org.geckoOS.Process", ""));
        assert!(!msg.matches_filter("org.geckoOS.Other", ""));
    }
}
