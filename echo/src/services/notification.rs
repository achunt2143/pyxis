//! Notification service: a store that the shell renders.
//!
//! `show` records a notification and signals `NotificationShown`, and the
//! shell (Bloom) draws the toast and keeps the notification centre. `list` is
//! the centre's history. `close` and `closeAll` dismiss entries, and each
//! dismissal signals `NotificationClosed`, so every open view of the centre
//! stays in step.
//!
//! This used to hand every call to `notify-send` or `dbus-send` and throw the
//! result away. Pyxis ships neither tool, and has no session bus for them to
//! talk to. So every notification vanished while `show` reported success,
//! and the shell's `list` call went to a method nothing had registered.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

pub const NOTIFICATION_IFACE: &str = "org.geckoOS.notification";
pub const SIGNAL_SHOWN: &str = "NotificationShown";
pub const SIGNAL_CLOSED: &str = "NotificationClosed";

/// How many notifications the centre keeps. Past that, the oldest go first:
/// a centre that grows forever is a memory leak with a UI.
pub const HISTORY_LIMIT: usize = 50;

const DEFAULT_TIMEOUT_MS: u64 = 5_000;
/// The longest a toast may ask to stay up. Anything longer is not a toast;
/// ask for `timeout: 0` (sticky) instead.
const MAX_TIMEOUT_MS: u64 = 60_000;
/// Anything longer is cut, so that one call cannot flood the shell.
const MAX_TITLE_CHARS: usize = 200;
const MAX_BODY_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub id: String,
    pub title: String,
    pub body: String,
    pub urgency: Urgency,
    /// How long the toast stays up, in ms. 0 means it stays until
    /// dismissed, and critical notifications always do.
    pub timeout: u64,
    /// The app that raised it. The vela bridge stamps this from the caller's
    /// `pyxis://` origin, so an app cannot pose as another (see
    /// PyxisBridgeParent). It is `None` for a direct Echo client, such as
    /// echo-cli or a daemon. Those can claim anything, which is the known
    /// bus-level enforcement gap in CLAUDE.md.
    pub app_id: Option<String>,
    pub icon: Option<String>,
    /// Milliseconds since the Unix epoch, the unit `Date.now()` uses.
    pub timestamp: u64,
}

pub struct NotificationStore {
    items: Mutex<VecDeque<Notification>>,
}

impl Default for NotificationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationStore {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(VecDeque::new()),
        }
    }

    /// The one store echo-daemon serves.
    pub fn global() -> &'static NotificationStore {
        static STORE: OnceLock<NotificationStore> = OnceLock::new();
        STORE.get_or_init(NotificationStore::new)
    }

    /// Record a notification from a `show` call's params, and return it with
    /// its new id.
    ///
    /// Accepts `{ title, options: { body, urgency, timeout, icon } }`, which
    /// is the shape the SDK has always sent, and the same fields flat at the
    /// top level. `appId` is only ever read from the top level, where the
    /// bridge puts it.
    pub fn show(&self, params: &Value) -> Notification {
        let field = |name: &str| {
            params
                .get("options")
                .and_then(|options| options.get(name))
                .or_else(|| params.get(name))
        };
        let text = |name: &str| field(name).and_then(Value::as_str);

        let urgency = match text("urgency") {
            Some("critical") => Urgency::Critical,
            Some("low") => Urgency::Low,
            _ => Urgency::Normal,
        };
        let timeout = if urgency == Urgency::Critical {
            0
        } else {
            field("timeout")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_MS)
                .min(MAX_TIMEOUT_MS)
        };

        let notification = Notification {
            id: Uuid::new_v4().to_string(),
            title: truncate(text("title").unwrap_or("Notification"), MAX_TITLE_CHARS),
            body: truncate(text("body").unwrap_or(""), MAX_BODY_CHARS),
            urgency,
            timeout,
            app_id: params
                .get("appId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned),
            icon: text("icon").map(str::to_owned),
            timestamp: now_ms(),
        };

        let mut items = self.items.lock().unwrap_or_else(|p| p.into_inner());
        items.push_front(notification.clone());
        items.truncate(HISTORY_LIMIT);
        notification
    }

    /// The centre's history, newest first.
    pub fn list(&self) -> Vec<Notification> {
        self.items
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Dismiss one notification. Returns false if there was no such id,
    /// including one already dismissed or aged out.
    pub fn close(&self, id: &str) -> bool {
        let mut items = self.items.lock().unwrap_or_else(|p| p.into_inner());
        let before = items.len();
        items.retain(|n| n.id != id);
        items.len() != before
    }

    /// Dismiss every notification, and return the ids that were dismissed.
    pub fn close_all(&self) -> Vec<String> {
        self.items
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
            .map(|n| n.id)
            .collect()
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn show_reads_the_sdk_shape_and_stores_newest_first() {
        let store = NotificationStore::new();
        let first = store.show(&json!({ "title": "one", "options": { "body": "b1" } }));
        let second = store.show(&json!({ "title": "two" }));

        assert_eq!(first.body, "b1");
        assert_eq!(first.urgency, Urgency::Normal);
        assert_eq!(first.timeout, DEFAULT_TIMEOUT_MS);
        let listed: Vec<_> = store.list().into_iter().map(|n| n.id).collect();
        assert_eq!(listed, vec![second.id, first.id]);
    }

    #[test]
    fn flat_fields_work_and_options_win_over_them() {
        let store = NotificationStore::new();
        let n = store.show(&json!({
            "title": "t", "body": "flat", "icon": "i.png",
            "options": { "body": "nested" }
        }));
        assert_eq!(n.body, "nested");
        assert_eq!(n.icon.as_deref(), Some("i.png"));
    }

    #[test]
    fn critical_is_sticky_and_timeouts_are_capped() {
        let store = NotificationStore::new();
        let critical = store.show(&json!({ "title": "t", "urgency": "critical", "timeout": 3000 }));
        assert_eq!(critical.timeout, 0);
        let long = store.show(&json!({ "title": "t", "timeout": 10_000_000u64 }));
        assert_eq!(long.timeout, MAX_TIMEOUT_MS);
        let sticky = store.show(&json!({ "title": "t", "timeout": 0 }));
        assert_eq!(sticky.timeout, 0);
    }

    #[test]
    fn app_id_comes_only_from_the_top_level() {
        let store = NotificationStore::new();
        // An app putting appId inside `options` must not get it recorded.
        // Only the bridge writes the top-level field.
        let n = store.show(&json!({ "title": "t", "options": { "appId": "bloom" } }));
        assert_eq!(n.app_id, None);
        let n = store.show(&json!({ "title": "t", "appId": "files" }));
        assert_eq!(n.app_id.as_deref(), Some("files"));
    }

    #[test]
    fn close_removes_one_and_reports_misses() {
        let store = NotificationStore::new();
        let a = store.show(&json!({ "title": "a" }));
        let b = store.show(&json!({ "title": "b" }));
        assert!(store.close(&a.id));
        assert!(!store.close(&a.id), "a second close of the same id is a miss");
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.close_all(), vec![b.id]);
        assert!(store.list().is_empty());
    }

    #[test]
    fn history_is_bounded_and_drops_the_oldest() {
        let store = NotificationStore::new();
        let first = store.show(&json!({ "title": "first" }));
        for i in 0..HISTORY_LIMIT {
            store.show(&json!({ "title": format!("n{i}") }));
        }
        let list = store.list();
        assert_eq!(list.len(), HISTORY_LIMIT);
        assert!(list.iter().all(|n| n.id != first.id));
    }

    #[test]
    fn long_text_is_cut_on_a_char_boundary() {
        let store = NotificationStore::new();
        let n = store.show(&json!({ "title": "é".repeat(MAX_TITLE_CHARS + 5) }));
        assert_eq!(n.title.chars().count(), MAX_TITLE_CHARS + 1); // + the ellipsis
        assert!(n.title.ends_with('…'));
    }
}
