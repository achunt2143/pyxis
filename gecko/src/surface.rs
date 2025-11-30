use anyhow::Result;
use std::collections::HashMap;
use parking_lot::RwLock;
use tracing;
use crate::WindowInfo;

/// Manages mapping between Gecko windows and Wayland surfaces
pub struct SurfaceManager {
    window_to_surface: RwLock<HashMap<u32, u32>>,
    surface_to_window: RwLock<HashMap<u32, u32>>,
    window_metadata: RwLock<HashMap<u32, WindowMetadata>>,
}

#[derive(Clone, Debug)]
struct WindowMetadata {
    window_id: u32,
    surface_id: u32,
    app_id: String,
    url: String,
    width: u32,
    height: u32,
    is_visible: bool,
}

impl SurfaceManager {
    pub fn new() -> Self {
        SurfaceManager {
            window_to_surface: RwLock::new(HashMap::new()),
            surface_to_window: RwLock::new(HashMap::new()),
            window_metadata: RwLock::new(HashMap::new()),
        }
    }

    /// Map Gecko window to Wayland surface
    pub fn map_window_to_surface(
        &self,
        window_id: u32,
        surface_id: u32,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let mut w2s = self.window_to_surface.write();
        let mut s2w = self.surface_to_window.write();
        let mut metadata = self.window_metadata.write();

        w2s.insert(window_id, surface_id);
        s2w.insert(surface_id, window_id);

        // Store metadata for later retrieval
        metadata.insert(
            window_id,
            WindowMetadata {
                window_id,
                surface_id,
                app_id: String::new(),
                url: String::new(),
                width,
                height,
                is_visible: true,
            },
        );

        tracing::debug!(
            "Mapped window {} to surface {} ({}x{})",
            window_id,
            surface_id,
            width,
            height
        );
        Ok(())
    }

    /// Unmap Gecko window from surface
    pub fn unmap_window(&self, window_id: u32) -> Result<()> {
        let mut w2s = self.window_to_surface.write();
        let mut s2w = self.surface_to_window.write();
        let mut metadata = self.window_metadata.write();

        if let Some(surface_id) = w2s.remove(&window_id) {
            s2w.remove(&surface_id);
            metadata.remove(&window_id);

            tracing::debug!("Unmapped window {}", window_id);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Window {} not mapped", window_id))
        }
    }

    /// Get surface ID for window
    pub fn get_surface_for_window(&self, window_id: u32) -> Result<u32> {
        self.window_to_surface
            .read()
            .get(&window_id)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Window {} not mapped to surface", window_id))
    }

    /// Get window ID for surface
    pub fn get_window_for_surface(&self, surface_id: u32) -> Result<u32> {
        self.surface_to_window
            .read()
            .get(&surface_id)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Surface {} not mapped to window", surface_id))
    }

    /// Update window metadata
    pub fn update_window_metadata(
        &self,
        window_id: u32,
        app_id: &str,
        url: &str,
        is_visible: bool,
    ) -> Result<()> {
        let mut metadata = self.window_metadata.write();

        if let Some(meta) = metadata.get_mut(&window_id) {
            meta.app_id = app_id.to_string();
            meta.url = url.to_string();
            meta.is_visible = is_visible;

            tracing::trace!(
                "Updated metadata for window {}: app_id={}, url={}",
                window_id,
                app_id,
                url
            );
            Ok(())
        } else {
            Err(anyhow::anyhow!("Window {} not tracked", window_id))
        }
    }

    /// Get window info
    pub fn get_window_info(&self, window_id: u32) -> Option<WindowInfo> {
        self.window_metadata.read().get(&window_id).map(|meta| WindowInfo {
            window_id: meta.window_id,
            surface_id: meta.surface_id,
            app_id: meta.app_id.clone(),
            url: meta.url.clone(),
            width: meta.width,
            height: meta.height,
            is_visible: meta.is_visible,
        })
    }

    /// List all windows
    pub fn list_all_windows(&self) -> Vec<WindowInfo> {
        self.window_metadata
            .read()
            .values()
            .map(|meta| WindowInfo {
                window_id: meta.window_id,
                surface_id: meta.surface_id,
                app_id: meta.app_id.clone(),
                url: meta.url.clone(),
                width: meta.width,
                height: meta.height,
                is_visible: meta.is_visible,
            })
            .collect()
    }

    /// Count mapped windows
    pub fn window_count(&self) -> usize {
        self.window_to_surface.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_surface_mapping() {
        let manager = SurfaceManager::new();
        manager.map_window_to_surface(1, 1, 800, 600).unwrap();

        assert_eq!(manager.get_surface_for_window(1).unwrap(), 1);
        assert_eq!(manager.get_window_for_surface(1).unwrap(), 1);
    }

    #[test]
    fn test_window_unmapping() {
        let manager = SurfaceManager::new();
        manager.map_window_to_surface(1, 1, 800, 600).unwrap();
        manager.unmap_window(1).unwrap();

        assert!(manager.get_surface_for_window(1).is_err());
    }

    #[test]
    fn test_window_metadata() {
        let manager = SurfaceManager::new();
        manager.map_window_to_surface(1, 1, 800, 600).unwrap();
        manager
            .update_window_metadata(1, "test-app", "https://example.com", true)
            .unwrap();

        let info = manager.get_window_info(1).unwrap();
        assert_eq!(info.app_id, "test-app");
        assert_eq!(info.url, "https://example.com");
    }
}
