use serde::{Deserialize, Serialize};
use anyhow::Result;
use uuid::Uuid;
use chrono::Utc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub header: MessageHeader,
    pub body: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageHeader {
    pub message_id: String,
    pub message_type: MessageType,
    pub sender: Option<String>,
    pub destination: String,
    pub interface: String,
    pub method: String,
    pub timestamp: String,
    pub reply_to: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageType {
    MethodCall,
    MethodReturn,
    Error,
    Signal,
}

impl Message {
    /// Create a method call message
    pub fn method_call(
        destination: &str,
        interface: &str,
        method: &str,
        args: serde_json::Value,
    ) -> Result<Self> {
        Ok(Message {
            header: MessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: MessageType::MethodCall,
                sender: None,
                destination: destination.to_string(),
                interface: interface.to_string(),
                method: method.to_string(),
                timestamp: Utc::now().to_rfc3339(),
                reply_to: None,
                error_code: None,
            },
            body: args,
        })
    }

    /// Create a method return message
    pub fn method_return(reply_to: &str, result: serde_json::Value) -> Result<Self> {
        Ok(Message {
            header: MessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: MessageType::MethodReturn,
                sender: None,
                destination: String::new(),
                interface: String::new(),
                method: String::new(),
                timestamp: Utc::now().to_rfc3339(),
                reply_to: Some(reply_to.to_string()),
                error_code: None,
            },
            body: result,
        })
    }

    /// Create an error message
    pub fn error(reply_to: &str, error_code: &str, message: &str) -> Result<Self> {
        Ok(Message {
            header: MessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: MessageType::Error,
                sender: None,
                destination: String::new(),
                interface: String::new(),
                method: String::new(),
                timestamp: Utc::now().to_rfc3339(),
                reply_to: Some(reply_to.to_string()),
                error_code: Some(error_code.to_string()),
            },
            body: serde_json::json!({ "message": message }),
        })
    }

    /// Create a signal message
    pub fn signal(interface: &str, signal_name: &str, data: serde_json::Value) -> Result<Self> {
        Ok(Message {
            header: MessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: MessageType::Signal,
                sender: None,
                destination: String::new(),
                interface: interface.to_string(),
                method: signal_name.to_string(),
                timestamp: Utc::now().to_rfc3339(),
                reply_to: None,
                error_code: None,
            },
            body: data,
        })
    }

    /// Check if message matches filter
    pub fn matches_filter(&self, interface: &str, method: &str) -> bool {
        (self.header.interface.is_empty() || self.header.interface == interface)
            && (method.is_empty() || self.header.method == method)
    }

    /// Get message size in bytes
    pub fn size(&self) -> usize {
        serde_json::to_vec(self)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// Get message priority (0-255, higher = more important)
    pub fn priority(&self) -> u8 {
        match self.header.message_type {
            MessageType::MethodCall => 100,
            MessageType::MethodReturn => 80,
            MessageType::Signal => 50,
            MessageType::Error => 120,
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
