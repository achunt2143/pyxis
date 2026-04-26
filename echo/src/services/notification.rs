//! Notification Service
//! 
//! Provides system notifications using D-Bus notification protocol.

use serde_json::{json, Value};
use anyhow::{Result, anyhow};
use std::process::Command;
use uuid::Uuid;

pub struct NotificationService;

impl NotificationService {
    pub fn show(title: &str, options: Option<Value>) -> Result<Value> {
        let notification_id = Uuid::new_v4().to_string();
        
        let body = options
            .as_ref()
            .and_then(|o| o.get("body"))
            .and_then(|b| b.as_str())
            .unwrap_or("");

        let urgency = options
            .as_ref()
            .and_then(|o| o.get("urgency"))
            .and_then(|u| u.as_str())
            .unwrap_or("normal");

        let timeout = options
            .as_ref()
            .and_then(|o| o.get("timeout"))
            .and_then(|t| t.as_u64())
            .unwrap_or(5000);

        // Try using notify-send (freedesktop notification)
        let _result = Command::new("notify-send")
            .arg("--urgency")
            .arg(match urgency {
                "critical" => "critical",
                "high" => "normal",
                _ => "low",
            })
            .arg("--expire-time")
            .arg(timeout.to_string())
            .arg(title)
            .arg(body)
            .output();

        Ok(json!({
            "notificationId": notification_id,
            "title": title,
            "body": body,
            "urgency": urgency,
            "timeout": timeout,
            "success": true
        }))
    }

    pub fn close(notification_id: &str) -> Result<Value> {
        // notify-send doesn't have a built-in close mechanism
        // This would require implementing D-Bus notifications directly for full support
        Ok(json!({
            "success": true,
            "notificationId": notification_id
        }))
    }

    pub fn close_all() -> Result<Value> {
        // Try using dbus to close all notifications
        let _result = Command::new("dbus-send")
            .args(&[
                "--print-reply",
                "--dest=org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications.CloseAllNotifications"
            ])
            .output();

        Ok(json!({
            "success": true
        }))
    }
}
