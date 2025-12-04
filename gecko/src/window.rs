// gecko/src/window.rs
//! Window model and lifecycle management

use std::sync::Arc;
use parking_lot::RwLock;
use tracing::debug;

/// Application type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppType {
    /// Gaia JavaScript application
    Gaia,
    /// Native Wayland application
    Native,
}

/// Window state in lifecycle
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    /// Window created, not yet visible
    Created,
    /// Window is visible and focused
    Focused,
    /// Window is visible but not focused
    Visible,
    /// Window is hidden
    Hidden,
    /// Window is closing
    Closing,
    /// Window is closed
    Closed,
}

/// Window configuration
#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// Application identifier
    pub app_id: String,
    /// Application type
    pub app_type: AppType,
    /// Application URL (for Gaia)
    pub url: String,
    /// Initial width
    pub width: u32,
    /// Initial height
    pub height: u32,
}

/// Represents a window in the Gecko window manager
pub struct Window {
    window_id: u32,
    surface_id: u32,
    config: WindowConfig,
    state: Arc<RwLock<WindowState>>,
    size: Arc<RwLock<(u32, u32)>>,
}

impl Window {
    /// Create new window
    pub fn new(window_id: u32, surface_id: u32, config: WindowConfig) -> Self {
        Window {
            window_id,
            surface_id,
            config: config.clone(),
            state: Arc::new(RwLock::new(WindowState::Created)),
            size: Arc::new(RwLock::new((config.width, config.height))),
        }
    }

    /// Get window ID
    pub fn window_id(&self) -> u32 {
        self.window_id
    }

    /// Get associated Wayland surface ID
    pub fn surface_id(&self) -> u32 {
        self.surface_id
    }

    /// Get application ID
    pub fn app_id(&self) -> &str {
        &self.config.app_id
    }

    /// Get application type
    pub fn app_type(&self) -> AppType {
        self.config.app_type
    }

    /// Get application URL
    pub fn url(&self) -> &str {
        &self.config.url
    }

    /// Get current size
    pub fn size(&self) -> (u32, u32) {
        *self.size.read()
    }

    /// Update size
    pub fn set_size(&self, width: u32, height: u32) {
        debug!(
            "Window {} resized to {}x{}",
            self.window_id, width, height
        );
        let mut size = self.size.write();
        size.0 = width;
        size.1 = height;
    }

    /// Get current state
    pub fn state(&self) -> WindowState {
        *self.state.read()
    }

    /// Update state
    pub fn set_state(&self, state: WindowState) {
        debug!(
            "Window {} state changed to {:?}",
            self.window_id, state
        );
        *self.state.write() = state;
    }

    /// Check if window is visible
    pub fn is_visible(&self) -> bool {
        let state = self.state();
        matches!(state, WindowState::Focused | WindowState::Visible)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_window() -> Window {
        Window::new(
            1,
            1,
            WindowConfig {
                app_id: "test.app".to_string(),
                app_type: AppType::Gaia,
                url: "file:///apps/test/index.html".to_string(),
                width: 800,
                height: 600,
            },
        )
    }

    #[test]
    fn test_window_creation() {
        let window = test_window();
        assert_eq!(window.window_id(), 1);
        assert_eq!(window.surface_id(), 1);
        assert_eq!(window.app_id(), "test.app");
        assert_eq!(window.app_type(), AppType::Gaia);
    }

    #[test]
    fn test_window_size() {
        let window = test_window();
        assert_eq!(window.size(), (800, 600));

        window.set_size(1024, 768);
        assert_eq!(window.size(), (1024, 768));
    }

    #[test]
    fn test_window_state() {
        let window = test_window();
        assert_eq!(window.state(), WindowState::Created);

        window.set_state(WindowState::Focused);
        assert!(window.is_visible());

        window.set_state(WindowState::Hidden);
        assert!(!window.is_visible());
    }
}
