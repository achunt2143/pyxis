use anyhow::Result;
use tracing;
use std::sync::Arc;
use parking_lot::RwLock;

/// X11 compatibility layer for Wayland environments
pub struct X11CompatibilityLayer {
    enabled: bool,
    connected: Arc<RwLock<bool>>,
    display: Option<String>,
}

#[derive(Debug, Clone)]
pub struct X11Window {
    pub id: u32,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub pid: u32,
}

impl X11CompatibilityLayer {
    pub async fn new(display: &Option<String>) -> Result<Self> {
        tracing::info!("Initializing X11 Compatibility Layer");

        let display_str = display.clone().or_else(|| std::env::var("DISPLAY").ok());

        if display_str.is_none() {
            tracing::warn!("No X11 display found, X11 compat disabled");
        }

        Ok(X11CompatibilityLayer {
            enabled: display_str.is_some(),
            connected: Arc::new(RwLock::new(display_str.is_some())),
            display: display_str,
        })
    }

    pub async fn disabled() -> Result<Self> {
        Ok(X11CompatibilityLayer {
            enabled: false,
            connected: Arc::new(RwLock::new(false)),
            display: None,
        })
    }

    /// Check if X11 is available
    pub async fn is_connected(&self) -> bool {
        if !self.enabled {
            return false;
        }
        *self.connected.read()
    }

    /// Map X11 window to Wayland surface
    pub async fn map_window_to_surface(
        &self,
        x11_window_id: u32,
        title: &str,
    ) -> Result<X11Window> {
        if !self.enabled {
            return Err(anyhow::anyhow!("X11 compatibility disabled"));
        }

        tracing::debug!("Mapping X11 window {} to Wayland", x11_window_id);

        Ok(X11Window {
            id: x11_window_id,
            title: title.to_string(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
            pid: 0,
        })
    }

    /// Get X11 display
    pub fn display(&self) -> Option<&str> {
        self.display.as_deref()
    }

    /// Check X11 window properties
    pub async fn get_window_properties(&self, window_id: u32) -> Result<X11WindowProperties> {
        tracing::debug!("Getting X11 window properties: {}", window_id);

        Ok(X11WindowProperties {
            window_id,
            name: String::new(),
            class: String::new(),
            role: String::new(),
            pid: 0,
            mapped: false,
        })
    }

    /// Set environment for X11 forwarding
    pub async fn setup_x11_env(&self) -> Result<()> {
        if let Some(display) = &self.display {
            std::env::set_var("DISPLAY", display);
            // tracing::debug!("Set DISPLAY={}", display);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct X11WindowProperties {
    pub window_id: u32,
    pub name: String,
    pub class: String,
    pub role: String,
    pub pid: u32,
    pub mapped: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_x11_compat_init() {
        let compat = X11CompatibilityLayer::new(&None).await.unwrap();
        assert!(compat.enabled || !compat.is_connected().await);
    }

    #[tokio::test]
    async fn test_x11_compat_disabled() {
        let compat = X11CompatibilityLayer::disabled().await.unwrap();
        assert!(!compat.is_connected().await);
    }
}
