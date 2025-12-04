// gaia/src/services.rs
//! Core shell services for Gaia

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

/// Shell state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellState {
    pub initialized: bool,
    pub desktop_ready: bool,
    pub theme: String,
    pub notifications_enabled: bool,
}

/// Window information
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
    pub visible: bool,
    pub created_at: String,
}

/// Application instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInstance {
    pub app_id: String,
    pub name: String,
    pub executable: String,
    pub icon: Option<String>,
    pub pid: u32,
    pub memory_mb: u32,
    pub cpu_percent: f32,
    pub window_count: u32,
    pub started_at: String,
    pub status: String,
}

impl ShellService {
    /// Create new shell service
    pub async fn new() -> Result<Self> {
        tracing::info!("Initializing Shell Service");
        Ok(ShellService {
            open_windows: Arc::new(DashMap::new()),
            running_apps: Arc::new(DashMap::new()),
        })
    }

    // ========================================================================
    // WINDOW MANAGEMENT
    // ========================================================================

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

    /// Focus window
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
            entry.visible = false;
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
            entry.visible = true;
        }
        Ok(())
    }

    /// Get window count
    pub async fn window_count(&self) -> usize {
        self.open_windows.len()
    }

    // ========================================================================
    // APPLICATION MANAGEMENT
    // ========================================================================

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

    /// Get app count
    pub async fn app_count(&self) -> usize {
        self.running_apps.len()
    }

    /// List all running apps (convenience method)
    pub fn list_apps(&self) -> Vec<AppInstance> {
        self.running_apps
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }
}

impl ShellState {
    /// Create new shell state
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
        assert_eq!(service.app_count().await, 0);
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
            visible: true,
            created_at: Utc::now().to_rfc3339(),
        };

        service.register_window(window).await.unwrap();
        assert_eq!(service.window_count().await, 1);
    }

    #[tokio::test]
    async fn test_app_registration() {
        let service = ShellService::new().await.unwrap();
        let app = AppInstance {
            app_id: "firefox".to_string(),
            name: "Firefox".to_string(),
            executable: "/usr/bin/firefox".to_string(),
            icon: Some("firefox.png".to_string()),
            pid: 1234,
            memory_mb: 256,
            cpu_percent: 15.5,
            window_count: 1,
            started_at: Utc::now().to_rfc3339(),
            status: "running".to_string(),
        };

        service.register_app(app).await.unwrap();
        assert_eq!(service.app_count().await, 1);
    }

    #[tokio::test]
    async fn test_focus_window() {
        let service = ShellService::new().await.unwrap();
        
        let w1 = WindowInfo {
            window_id: 1,
            app_id: "app1".to_string(),
            title: "Window 1".to_string(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
            focused: false,
            minimized: false,
            maximized: false,
            visible: true,
            created_at: Utc::now().to_rfc3339(),
        };

        let w2 = WindowInfo {
            window_id: 2,
            app_id: "app2".to_string(),
            title: "Window 2".to_string(),
            x: 100,
            y: 100,
            width: 800,
            height: 600,
            focused: false,
            minimized: false,
            maximized: false,
            visible: true,
            created_at: Utc::now().to_rfc3339(),
        };

        service.register_window(w1).await.unwrap();
        service.register_window(w2).await.unwrap();

        service.focus_window(2).await.unwrap();

        let windows = service.get_windows().await.unwrap();
        assert!(windows.iter().find(|w| w.window_id == 2).unwrap().focused);
        assert!(!windows.iter().find(|w| w.window_id == 1).unwrap().focused);
    }

    #[tokio::test]
    async fn test_minimize_window() {
        let service = ShellService::new().await.unwrap();
        let window = WindowInfo {
            window_id: 1,
            app_id: "app1".to_string(),
            title: "Test Window".to_string(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
            focused: true,
            minimized: false,
            maximized: false,
            visible: true,
            created_at: Utc::now().to_rfc3339(),
        };

        service.register_window(window).await.unwrap();
        service.minimize_window(1).await.unwrap();

        let window = service.get_window(1).await.unwrap().unwrap();
        assert!(window.minimized);
        assert!(!window.visible);
    }

    #[tokio::test]
    async fn test_restore_window() {
        let service = ShellService::new().await.unwrap();
        let window = WindowInfo {
            window_id: 1,
            app_id: "app1".to_string(),
            title: "Test Window".to_string(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
            focused: true,
            minimized: true,
            maximized: false,
            visible: false,
            created_at: Utc::now().to_rfc3339(),
        };

        service.register_window(window).await.unwrap();
        service.restore_window(1).await.unwrap();

        let window = service.get_window(1).await.unwrap().unwrap();
        assert!(!window.minimized);
        assert!(!window.maximized);
        assert!(window.visible);
    }
}