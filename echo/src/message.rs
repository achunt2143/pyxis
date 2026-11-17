use serde::{Deserialize, Serialize};
use anyhow::Result;
use uuid::Uuid;
use chrono::Utc;

/// Represents a full message with header and body
///
/// `Debug` is written by hand (below) so a call that carries a password never
/// prints its body: the router and the daemon log whole messages at debug, and
/// echo-daemon runs at that level.
#[derive(Clone, Serialize, Deserialize)]
pub struct Message {
    pub header: MessageHeader,
    pub body: serde_json::Value,
}

/// The calls whose arguments are a password, as `(destination, method)`. Their
/// bodies are never logged (see `Message::body_is_secret`).
pub const SECRET_CALLS: &[(&str, &str)] = &[
    (crate::services::session::SESSION_IFACE, crate::services::session::METHOD_UNLOCK),
    (crate::services::accounts::ACCOUNTS_IFACE, crate::services::accounts::METHOD_CREATE),
    (crate::services::accounts::ACCOUNTS_IFACE, crate::services::accounts::METHOD_SET_PASSWORD),
    (crate::services::privilege::PRIVILEGE_SERVICE, "Authenticate"),
    (crate::services::privilege::PRIVILEGE_SERVICE, "authenticate"),
];

/// What a logged secret body reads as.
pub const REDACTED: &str = "<redacted>";

impl Message {
    /// Whether this message's body holds a password. Compared without regard
    /// to case, so a miscased call (which finds no handler, and is logged as
    /// "no handler") can't print one either.
    pub fn body_is_secret(&self) -> bool {
        SECRET_CALLS.iter().any(|(destination, method)| {
            self.header.destination.eq_ignore_ascii_case(destination)
                && self.header.method.eq_ignore_ascii_case(method)
        })
    }
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut shown = f.debug_struct("Message");
        shown.field("header", &self.header);
        if self.body_is_secret() {
            shown.field("body", &format_args!("{REDACTED}"));
        } else {
            shown.field("body", &self.body);
        }
        shown.finish()
    }
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

    #[test]
    fn a_password_call_never_prints_its_body() {
        let secret = "hunter2-do-not-log";
        let calls = [
            ("org.geckoOS.session", "unlock", serde_json::json!({ "password": secret })),
            ("org.geckoOS.accounts", "setPassword", serde_json::json!({ "user": "u", "current": secret, "new": secret })),
            // A miscased name finds no handler but must not print either.
            ("org.geckoOS.SESSION", "Unlock", serde_json::json!({ "password": secret })),
        ];
        for (destination, method, body) in calls {
            let msg = Message::method_call(destination, destination, method, body).unwrap();
            assert!(msg.body_is_secret(), "{method}");
            for shown in [format!("{msg:?}"), format!("{msg:#?}")] {
                assert!(!shown.contains(secret), "{method}: {shown}");
                assert!(shown.contains(REDACTED), "{method}: {shown}");
                // The header is still there to debug with.
                assert!(shown.contains(method), "{method}: {shown}");
            }
            // Redaction is for logs: the wire body is untouched.
            assert!(serde_json::to_string(&msg).unwrap().contains(secret));
        }
    }

    #[test]
    fn other_calls_print_as_before() {
        let msg = Message::method_call("org.geckoOS.session", "org.geckoOS.session", "lock",
            serde_json::json!({ "why": "visible" })).unwrap();
        assert!(!msg.body_is_secret());
        assert!(format!("{msg:?}").contains("visible"));
        let msg = Message::method_call("org.geckoOS.accounts", "org.geckoOS.accounts", "setProfile",
            serde_json::json!({ "display_name": "visible" })).unwrap();
        assert!(format!("{msg:?}").contains("visible"));
    }
}
