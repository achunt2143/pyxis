// gaia/src/taskbar.rs
//! Taskbar manager for Gaia Shell

use anyhow::Result;
use std::sync::Arc;
use std::collections::HashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};
use serde_json::json;

use crate::services::{ShellService, AppInstance};

/// Task manager for Gaia
pub struct TaskbarManager {
    service: Arc<ShellService>,
    pinned_apps: Arc<RwLock<Vec<String>>>,
    taskbar_items: Arc<RwLock<HashMap<String, TaskbarItem>>>,
    tray_items: Arc<RwLock<Vec<TrayItem>>>,
    focused_app: Arc<RwLock<Option<String>>>,
}

/// Taskbar item representing an application
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskbarItem {
    pub app_id: String,
    pub title: String,
    pub icon: String,
    pub window_count: u32,
    pub focused: bool,
    pub pinned: bool,
    pub status: String,
}

/// System tray item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrayItem {
    pub id: String,
    pub label: String,
    pub icon: String,
    pub value: String,
    pub clickable: bool,
    pub tooltip: Option<String>,
}

/// Taskbar configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskbarConfig {
    pub position: String,
    pub auto_hide: bool,
    pub icon_size: u32,
    pub show_labels: bool,
    pub show_tray: bool,
}

impl Default for TaskbarConfig {
    fn default() -> Self {
        TaskbarConfig {
            position: "bottom".to_string(),
            auto_hide: false,
            icon_size: 32,
            show_labels: true,
            show_tray: true,
        }
    }
}

impl TaskbarManager {
    /// Create new taskbar manager
    pub async fn new(service: Arc<ShellService>) -> Result<Self> {
        info!("Initializing Taskbar Manager");
        Ok(TaskbarManager {
            service,
            pinned_apps: Arc::new(RwLock::new(Vec::new())),
            taskbar_items: Arc::new(RwLock::new(HashMap::new())),
            tray_items: Arc::new(RwLock::new(Vec::new())),
            focused_app: Arc::new(RwLock::new(None)),
        })
    }

    // ========================================================================
    // INITIALIZATION & SETUP
    // ========================================================================

    /// Setup taskbar
    pub async fn setup(&self) -> Result<()> {
        debug!("Setting up taskbar");

        // Initialize system tray items
        let tray_items = vec![
            TrayItem {
                id: "battery".to_string(),
                label: "Battery".to_string(),
                icon: "battery-100.svg".to_string(),
                value: "100%".to_string(),
                clickable: false,
                tooltip: Some("Battery status".to_string()),
            },
            TrayItem {
                id: "network".to_string(),
                label: "Network".to_string(),
                icon: "wifi-connected.svg".to_string(),
                value: "Connected".to_string(),
                clickable: true,
                tooltip: Some("Network settings".to_string()),
            },
            TrayItem {
                id: "audio".to_string(),
                label: "Audio".to_string(),
                icon: "volume-100.svg".to_string(),
                value: "100%".to_string(),
                clickable: true,
                tooltip: Some("Audio settings".to_string()),
            },
            TrayItem {
                id: "clock".to_string(),
                label: "Clock".to_string(),
                icon: "clock.svg".to_string(),
                value: chrono::Local::now().format("%H:%M").to_string(),
                clickable: true,
                tooltip: Some("Date and time".to_string()),
            },
        ];

        *self.tray_items.write() = tray_items;

        info!("Taskbar setup complete");
        Ok(())
    }

    // ========================================================================
    // TASKBAR ITEMS MANAGEMENT
    // ========================================================================

    /// Get all taskbar items (both running and pinned)
    pub async fn get_items(&self) -> Result<Vec<TaskbarItem>> {
        debug!("Getting taskbar items");

        let apps = self.service.list_apps();
        let mut items = HashMap::new();
        let pinned = self.pinned_apps.read();
        let focused = self.focused_app.read();

        for app in apps {
            let item = TaskbarItem {
                app_id: app.app_id.clone(),
                title: app.name.clone(),
                icon: app.icon.unwrap_or_else(|| format!("icon-{}.png", app.app_id)),
                window_count: app.window_count,
                focused: focused.as_ref().map_or(false, |f| f == &app.app_id),
                pinned: pinned.contains(&app.app_id),
                status: app.status.clone(),
            };
            items.insert(app.app_id.clone(), item);
        }

        // Add pinned apps that might not be running
        for app_id in pinned.iter() {
            if !items.contains_key(app_id) {
                items.insert(
                    app_id.clone(),
                    TaskbarItem {
                        app_id: app_id.clone(),
                        title: app_id.clone(),
                        icon: format!("icon-{}.png", app_id),
                        window_count: 0,
                        focused: false,
                        pinned: true,
                        status: "stopped".to_string(),
                    },
                );
            }
        }

        let mut result: Vec<_> = items.into_values().collect();

        // Sort: focused first, then running, then pinned
        result.sort_by(|a, b| {
            if a.focused {
                std::cmp::Ordering::Less
            } else if b.focused {
                std::cmp::Ordering::Greater
            } else if a.status == "running" && b.status != "running" {
                std::cmp::Ordering::Less
            } else if b.status == "running" && a.status != "running" {
                std::cmp::Ordering::Greater
            } else if a.pinned && !b.pinned {
                std::cmp::Ordering::Less
            } else if b.pinned && !a.pinned {
                std::cmp::Ordering::Greater
            } else {
                a.app_id.cmp(&b.app_id)
            }
        });

        Ok(result)
    }

    /// Add application to taskbar (when process created)
    pub async fn add_application(&self, app_id: &str) -> Result<()> {
        info!("Adding application to taskbar: {}", app_id);

        if let Ok(Some(app)) = self.service.get_app(app_id).await {
            let item = TaskbarItem {
                app_id: app.app_id.clone(),
                title: app.name.clone(),
                icon: app.icon.unwrap_or_else(|| format!("icon-{}.png", app_id)),
                window_count: app.window_count,
                focused: false,
                pinned: self.pinned_apps.read().contains(&app.app_id),
                status: app.status.clone(),
            };

            self.taskbar_items.write().insert(app_id.to_string(), item);
            debug!("Application added to taskbar: {}", app_id);
        }

        Ok(())
    }

    /// Remove application from taskbar (when process terminates)
    pub async fn remove_application(&self, app_id: &str) -> Result<()> {
        info!("Removing application from taskbar: {}", app_id);

        // Only remove if not pinned
        if !self.pinned_apps.read().contains(&app_id.to_string()) {
            self.taskbar_items.write().remove(app_id);
        } else {
            // Update status to stopped but keep pinned
            if let Some(item) = self.taskbar_items.write().get_mut(app_id) {
                item.status = "stopped".to_string();
                item.window_count = 0;
            }
        }

        // Clear focused if this was the focused app
        let mut focused = self.focused_app.write();
        if focused.as_ref().map_or(false, |f| f == app_id) {
            *focused = None;
        }

        debug!("Application removed from taskbar: {}", app_id);
        Ok(())
    }

    /// Update application status in taskbar
    pub async fn update_application_status(&self, app_id: &str, status: &str) -> Result<()> {
        debug!("Updating application status: {} -> {}", app_id, status);

        let mut items = self.taskbar_items.write();
        if let Some(item) = items.get_mut(app_id) {
            item.status = status.to_string();
        }

        Ok(())
    }

    /// Set application as focused
    pub async fn set_focused_app(&self, app_id: &str) -> Result<()> {
        debug!("Setting focused app: {}", app_id);

        // Update old focused
        if let Some(old_focused) = self.focused_app.read().as_ref() {
            if let Some(item) = self.taskbar_items.write().get_mut(old_focused) {
                item.focused = false;
            }
        }

        // Update new focused
        if let Some(item) = self.taskbar_items.write().get_mut(app_id) {
            item.focused = true;
        }

        *self.focused_app.write() = Some(app_id.to_string());
        Ok(())
    }

    /// Get focused application
    pub async fn get_focused_app(&self) -> Result<Option<String>> {
        Ok(self.focused_app.read().clone())
    }

    // ========================================================================
    // PINNED APPLICATIONS
    // ========================================================================

    /// Pin app to taskbar (persistent)
    pub async fn pin_app(&self, app_id: &str) -> Result<()> {
        info!("Pinning app to taskbar: {}", app_id);

        let mut pinned = self.pinned_apps.write();
        if !pinned.contains(&app_id.to_string()) {
            pinned.push(app_id.to_string());
        }

        // Update item
        if let Some(item) = self.taskbar_items.write().get_mut(app_id) {
            item.pinned = true;
        }

        Ok(())
    }

    /// Unpin app from taskbar
    pub async fn unpin_app(&self, app_id: &str) -> Result<()> {
        info!("Unpinning app from taskbar: {}", app_id);

        let mut pinned = self.pinned_apps.write();
        pinned.retain(|a| a != app_id);

        // Update item
        if let Some(item) = self.taskbar_items.write().get_mut(app_id) {
            item.pinned = false;
        }

        Ok(())
    }

    /// Get pinned applications
    pub async fn get_pinned_apps(&self) -> Result<Vec<String>> {
        Ok(self.pinned_apps.read().clone())
    }

    // ========================================================================
    // SYSTEM TRAY
    // ========================================================================

    /// Get system tray items
    pub async fn get_tray_items(&self) -> Result<Vec<TrayItem>> {
        Ok(self.tray_items.read().clone())
    }

    /// Update tray item value
    pub async fn update_tray_item(&self, id: &str, value: &str) -> Result<()> {
        debug!("Updating tray item: {} = {}", id, value);

        if let Some(item) = self.tray_items.write().iter_mut().find(|t| t.id == id) {
            item.value = value.to_string();
        }

        Ok(())
    }

    /// Add tray item
    pub async fn add_tray_item(&self, item: TrayItem) -> Result<()> {
        info!("Adding tray item: {}", item.id);

        self.tray_items.write().push(item);
        Ok(())
    }

    /// Remove tray item
    pub async fn remove_tray_item(&self, id: &str) -> Result<()> {
        info!("Removing tray item: {}", id);

        self.tray_items.write().retain(|t| t.id != id);
        Ok(())
    }

    // ========================================================================
    // WINDOW MANAGEMENT
    // ========================================================================

    /// Show all windows (minimize toggle)
    pub async fn show_all_windows(&self) -> Result<()> {
        debug!("Showing all windows");

        let windows = self.service.get_windows().await?;
        for window in windows {
            self.service.restore_window(window.window_id).await?;
        }

        info!("All windows shown");
        Ok(())
    }

    /// Minimize all windows (show desktop)
    pub async fn minimize_all_windows(&self) -> Result<()> {
        debug!("Minimizing all windows");

        let windows = self.service.get_windows().await?;
        for window in windows {
            self.service.minimize_window(window.window_id).await?;
        }

        info!("All windows minimized");
        Ok(())
    }

    /// Toggle window visibility
    pub async fn toggle_app_windows(&self, app_id: &str) -> Result<()> {
        debug!("Toggling app windows: {}", app_id);

        let windows = self.service.get_windows().await?;
        let app_windows: Vec<_> = windows
            .into_iter()
            .filter(|w| w.app_id == app_id)
            .collect();

        // If any window is visible, minimize all. Otherwise show all.
        let any_visible = app_windows.iter().any(|w| w.visible);

        for window in app_windows {
            if any_visible {
                self.service.minimize_window(window.window_id).await?;
            } else {
                self.service.restore_window(window.window_id).await?;
            }
        }

        Ok(())
    }

    /// Focus window for application
    pub async fn focus_app(&self, app_id: &str) -> Result<()> {
        debug!("Focusing app: {}", app_id);

        let windows = self.service.get_windows().await?;
        if let Some(window) = windows.iter().find(|w| w.app_id == app_id) {
            self.service.focus_window(window.window_id).await?;
            self.set_focused_app(app_id).await?;
        }

        Ok(())
    }

    // ========================================================================
    // TASKBAR STATE
    // ========================================================================

    /// Get taskbar state as JSON
    pub async fn get_state_json(&self) -> Result<serde_json::Value> {
        let items = self.get_items().await?;
        let tray = self.get_tray_items().await?;
        let focused = self.get_focused_app().await?;

        Ok(json!({
            "items": items,
            "tray": tray,
            "focused": focused,
            "item_count": items.len(),
        }))
    }

    /// Get taskbar info
    pub async fn get_info(&self) -> Result<serde_json::Value> {
        let items = self.get_items().await?;
        let tray = self.get_tray_items().await?;

        Ok(json!({
            "application_count": items.len(),
            "running_apps": items.iter().filter(|i| i.status == "running").count(),
            "pinned_apps": items.iter().filter(|i| i.pinned).count(),
            "tray_items": tray.len(),
            "total_windows": items.iter().map(|i| i.window_count).sum::<u32>(),
        }))
    }

    // ========================================================================
    // UTILITIES
    // ========================================================================

    /// Clear taskbar (remove all running items)
    pub async fn clear_running(&self) -> Result<()> {
        debug!("Clearing running taskbar items");

        let mut items = self.taskbar_items.write();
        let pinned = self.pinned_apps.read();

        // Keep only pinned items
        items.retain(|app_id, _| pinned.contains(app_id));

        Ok(())
    }

    /// Get count of running applications
    pub fn get_running_count(&self) -> usize {
        self.taskbar_items
            .read()
            .values()
            .filter(|item| item.status == "running" && !item.pinned)
            .count()
    }

    /// Check if app is in taskbar
    pub fn has_app(&self, app_id: &str) -> bool {
        self.taskbar_items.read().contains_key(app_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_taskbar_init() {
        let service = Arc::new(ShellService::new().await.unwrap());
        let taskbar = TaskbarManager::new(service).await.unwrap();
        taskbar.setup().await.unwrap();

        let items = taskbar.get_items().await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn test_pin_unpin_app() {
        let service = Arc::new(ShellService::new().await.unwrap());
        let taskbar = TaskbarManager::new(service).await.unwrap();

        taskbar.pin_app("firefox").await.unwrap();
        let pinned = taskbar.get_pinned_apps().await.unwrap();
        assert!(pinned.contains(&"firefox".to_string()));

        taskbar.unpin_app("firefox").await.unwrap();
        let pinned = taskbar.get_pinned_apps().await.unwrap();
        assert!(!pinned.contains(&"firefox".to_string()));
    }

    #[tokio::test]
    async fn test_focused_app() {
        let service = Arc::new(ShellService::new().await.unwrap());
        let taskbar = TaskbarManager::new(service).await.unwrap();

        taskbar.add_application("app1").await.unwrap();
        taskbar.set_focused_app("app1").await.unwrap();

        let focused = taskbar.get_focused_app().await.unwrap();
        assert_eq!(focused, Some("app1".to_string()));
    }

    #[tokio::test]
    async fn test_tray_items() {
        let service = Arc::new(ShellService::new().await.unwrap());
        let taskbar = TaskbarManager::new(service).await.unwrap();
        taskbar.setup().await.unwrap();

        let tray = taskbar.get_tray_items().await.unwrap();
        assert!(!tray.is_empty());
        assert!(tray.iter().any(|t| t.id == "battery"));
    }

    #[tokio::test]
    async fn test_update_tray_item() {
        let service = Arc::new(ShellService::new().await.unwrap());
        let taskbar = TaskbarManager::new(service).await.unwrap();
        taskbar.setup().await.unwrap();

        taskbar.update_tray_item("battery", "50%").await.unwrap();
        let tray = taskbar.get_tray_items().await.unwrap();
        assert!(tray.iter().any(|t| t.id == "battery" && t.value == "50%"));
    }

    #[tokio::test]
    async fn test_state_json() {
        let service = Arc::new(ShellService::new().await.unwrap());
        let taskbar = TaskbarManager::new(service).await.unwrap();
        taskbar.setup().await.unwrap();

        let state = taskbar.get_state_json().await.unwrap();
        assert!(state["items"].is_array());
        assert!(state["tray"].is_array());
    }

    #[tokio::test]
    async fn test_taskbar_info() {
        let service = Arc::new(ShellService::new().await.unwrap());
        let taskbar = TaskbarManager::new(service).await.unwrap();
        taskbar.setup().await.unwrap();

        let info = taskbar.get_info().await.unwrap();
        assert!(info["application_count"].is_number());
        assert!(info["tray_items"].is_number());
    }
}