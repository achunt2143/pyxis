use anyhow::Result;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tracing;

use crate::ShellService;

/// Task manager for Gaia
pub struct TaskbarManager {
    service: Arc<ShellService>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskbarItem {
    pub app_id: String,
    pub title: String,
    pub icon: String,
    pub window_count: u32,
    pub focused: bool,
}

impl TaskbarManager {
    pub async fn new(service: Arc<ShellService>) -> Result<Self> {
        tracing::info!("Initializing Taskbar Manager");

        Ok(TaskbarManager { service })
    }

    /// Setup taskbar
    pub async fn setup(&self) -> Result<()> {
        tracing::debug!("Setting up taskbar");
        Ok(())
    }

    /// Get taskbar items (running apps)
    pub async fn get_items(&self) -> Result<Vec<TaskbarItem>> {
        let apps = self.service.get_apps().await?;
        
        let items = apps
            .into_iter()
            .map(|app| TaskbarItem {
                app_id: app.app_id.clone(),
                title: app.app_id.clone(),
                icon: format!("icon-{}.png", app.app_id),
                window_count: app.window_count,
                focused: false,
            })
            .collect();

        Ok(items)
    }

    /// Pin app to taskbar
    pub async fn pin_app(&self, app_id: &str) -> Result<()> {
        tracing::info!("Pinned app to taskbar: {}", app_id);
        Ok(())
    }

    /// Unpin app from taskbar
    pub async fn unpin_app(&self, app_id: &str) -> Result<()> {
        tracing::info!("Unpinned app from taskbar: {}", app_id);
        Ok(())
    }

    /// Get system tray items
    pub async fn get_tray_items(&self) -> Result<Vec<TrayItem>> {
        Ok(vec![
            TrayItem {
                id: "battery".to_string(),
                label: "Battery".to_string(),
                icon: "battery-100.svg".to_string(),
                value: "100%".to_string(),
            },
            TrayItem {
                id: "network".to_string(),
                label: "Network".to_string(),
                icon: "wifi-connected.svg".to_string(),
                value: "Connected".to_string(),
            },
            TrayItem {
                id: "audio".to_string(),
                label: "Audio".to_string(),
                icon: "volume-100.svg".to_string(),
                value: "100%".to_string(),
            },
        ])
    }

    /// Show all windows
    pub async fn show_all_windows(&self) -> Result<()> {
        let windows = self.service.get_windows().await?;
        for window in windows {
            self.service.restore_window(window.window_id).await?;
        }
        Ok(())
    }

    /// Minimize all windows
    pub async fn minimize_all_windows(&self) -> Result<()> {
        let windows = self.service.get_windows().await?;
        for window in windows {
            self.service.minimize_window(window.window_id).await?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrayItem {
    pub id: String,
    pub label: String,
    pub icon: String,
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_taskbar_init() {
        let service = Arc::new(ShellService::new().await.unwrap());
        let taskbar = TaskbarManager::new(service).await.unwrap();
        assert!(taskbar.get_items().await.is_ok());
    }
}
