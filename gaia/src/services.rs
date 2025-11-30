use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tracing;
use chrono::Utc;

/// Core shell service for Gaia
pub struct ShellService {
    open_windows: Arc<DashMap<u32, WindowInfo>>,
    running_apps: Arc<DashMap<String, AppInstance>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellState {
    pub initialized: bool,
    pub desktop_ready: bool,
    pub theme: String,
    pub notifications_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub window_id: u32,
    pub app_id: String,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub focused: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInstance {
    pub app_id: String,
    pub executable: String,
    pub pid: u32,
    pub memory_mb: u32,
    pub cpu_percent: f32,
    pub window_count: u32,
    pub started_at: String,
}

impl ShellService {
    pub async fn new() -> Result<Self> {
        tracing::info!("Initializing Shell Service");

        Ok(ShellService {
            open_windows: Arc::new(DashMap::new()),
            running_apps: Arc::new(DashMap::new()),
        })
    }

    /// Register a new window
    pub async fn register_window(&self, window: WindowInfo) -> Result<()> {
        self.open_windows.insert(window.window_id, window);
        tracing::debug!("Window registered");
        Ok(())
    }

    /// Unregister a window
    pub async fn unregister_window(&self, window_id: u32) -> Result<()> {
        self.open_windows.remove(&window_id);
        tracing::debug!("Window unregistered");
        Ok(())
    }

    /// Get all windows
    pub async fn get_windows(&self) -> Result<Vec<WindowInfo>> {
        Ok(self
            .open_windows
            .iter()
            .map(|entry| entry.value().clone())
            .collect())
    }

    /// Get window by ID
    pub async fn get_window(&self, window_id: u32) -> Result<Option<WindowInfo>> {
        Ok(self.open_windows.get(&window_id).map(|entry| entry.value().clone()))
    }

    /// Register a running app
    pub async fn register_app(&self, app: AppInstance) -> Result<()> {
        self.running_apps.insert(app.app_id.clone(), app);
        tracing::debug!("App registered");
        Ok(())
    }

    /// Unregister an app
    pub async fn unregister_app(&self, app_id: &str) -> Result<()> {
        self.running_apps.remove(app_id);
        tracing::debug!("App unregistered");
        Ok(())
    }

    /// Get all running apps
    pub async fn get_apps(&self) -> Result<Vec<AppInstance>> {
        Ok(self
            .running_apps
            .iter()
            .map(|entry| entry.value().clone())
            .collect())
    }

    /// Get app by ID
    pub async fn get_app(&self, app_id: &str) -> Result<Option<AppInstance>> {
        Ok(self.running_apps.get(app_id).map(|entry| entry.value().clone()))
    }

    /// Update window focus
    pub async fn focus_window(&self, window_id: u32) -> Result<()> {
        // Unfocus all windows
        for mut entry in self.open_windows.iter_mut() {
            entry.value_mut().focused = false;
        }

        // Focus the target window
        if let Some(mut entry) = self.open_windows.get_mut(&window_id) {
            entry.focused = true;
        }

        tracing::debug!("Window focused: {}", window_id);
        Ok(())
    }

    /// Minimize window
    pub async fn minimize_window(&self, window_id: u32) -> Result<()> {
        if let Some(mut entry) = self.open_windows.get_mut(&window_id) {
            entry.minimized = true;
        }
        Ok(())
    }

    /// Maximize window
    pub async fn maximize_window(&self, window_id: u32) -> Result<()> {
        if let Some(mut entry) = self.open_windows.get_mut(&window_id) {
            entry.maximized = true;
        }
        Ok(())
    }

    /// Restore window
    pub async fn restore_window(&self, window_id: u32) -> Result<()> {
        if let Some(mut entry) = self.open_windows.get_mut(&window_id) {
            entry.minimized = false;
            entry.maximized = false;
        }
        Ok(())
    }

    /// Get window count
    pub async fn window_count(&self) -> usize {
        self.open_windows.len()
    }

    /// Get app count
    pub async fn app_count(&self) -> usize {
        self.running_apps.len()
    }
}

impl ShellState {
    pub fn new() -> Self {
        ShellState {
            initialized: false,
            desktop_ready: false,
            theme: "light".to_string(),
            notifications_enabled: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_service_init() {
        let service = ShellService::new().await.unwrap();
        assert_eq!(service.window_count().await, 0);
    }

    #[tokio::test]
    async fn test_window_registration() {
        let service = ShellService::new().await.unwrap();
        
        let window = WindowInfo {
            window_id: 1,
            app_id: "calculator".to_string(),
            title: "Calculator".to_string(),
            x: 100,
            y: 100,
            width: 400,
            height: 300,
            focused: true,
            minimized: false,
            maximized: false,
            created_at: Utc::now().to_rfc3339(),
        };

        service.register_window(window).await.unwrap();
        assert_eq!(service.window_count().await, 1);
    }
}
