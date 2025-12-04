// gecko/src/surface_manager.rs
//! Manages bidirectional mapping between windows and surfaces

use anyhow::{Result, anyhow};
use dashmap::DashMap;
use std::collections::HashMap;
use tracing::debug;

/// Manages window-to-surface mappings
pub struct SurfaceManager {
    window_to_surface: DashMap<u32, u32>,
    surface_to_window: DashMap<u32, u32>,
    window_metadata: DashMap<u32, WindowMetadata>,
}

#[derive(Debug, Clone)]
struct WindowMetadata {
    window_id: u32,
    surface_id: u32,
    width: u32,
    height: u32,
}

impl SurfaceManager {
    /// Create new surface manager
    pub fn new() -> Self {
        SurfaceManager {
            window_to_surface: DashMap::new(),
            surface_to_window: DashMap::new(),
            window_metadata: DashMap::new(),
        }
    }

    /// Map window to surface
    pub fn map_window_to_surface(
        &self,
        window_id: u32,
        surface_id: u32,
        width: u32,
        height: u32,
    ) -> Result<()> {
        debug!(
            "Mapping window {} to surface {} ({}x{})",
            window_id, surface_id, width, height
        );

        self.window_to_surface.insert(window_id, surface_id);
        self.surface_to_window.insert(surface_id, window_id);
        self.window_metadata.insert(
            window_id,
            WindowMetadata {
                window_id,
                surface_id,
                width,
                height,
            },
        );

        Ok(())
    }

    /// Unmap window from surface
    pub fn unmap_window(&self, window_id: u32) -> Result<()> {
        debug!("Unmapping window {}", window_id);

        if let Some((_, surface_id)) = self.window_to_surface.remove(&window_id) {
            self.surface_to_window.remove(&surface_id);
            self.window_metadata.remove(&window_id);
            Ok(())
        } else {
            Err(anyhow!("Window {} not mapped", window_id))
        }
    }

    /// Get surface ID for window
    pub fn get_surface_for_window(&self, window_id: u32) -> Result<u32> {
        self.window_to_surface
            .get(&window_id)
            .map(|r| *r)
            .ok_or_else(|| anyhow!("Window {} not mapped to surface", window_id))
    }

    /// Get window ID for surface
    pub fn get_window_for_surface(&self, surface_id: u32) -> Result<u32> {
        self.surface_to_window
            .get(&surface_id)
            .map(|r| *r)
            .ok_or_else(|| anyhow!("Surface {} not mapped to window", surface_id))
    }

    /// Update window size
    pub fn update_window_size(&self, window_id: u32, width: u32, height: u32) -> Result<()> {
        self.window_metadata
            .alter(&window_id, |_, mut meta| {
                meta.width = width;
                meta.height = height;
                meta
            });

        Ok(())
    }

    /// Get window size
    pub fn get_window_size(&self, window_id: u32) -> Result<(u32, u32)> {
        self.window_metadata
            .get(&window_id)
            .map(|meta| (meta.width, meta.height))
            .ok_or_else(|| anyhow!("Window {} metadata not found", window_id))
    }

    /// List all windows
    pub fn list_all_windows(&self) -> Vec<u32> {
        self.window_to_surface
            .iter()
            .map(|entry| *entry.key())
            .collect()
    }

    /// Count mapped windows
    pub fn window_count(&self) -> usize {
        self.window_to_surface.len()
    }
}

impl Default for SurfaceManager {
    fn default() -> Self {
        Self::new()
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
    fn test_window_size_tracking() {
        let manager = SurfaceManager::new();
        manager.map_window_to_surface(1, 1, 800, 600).unwrap();

        assert_eq!(manager.get_window_size(1).unwrap(), (800, 600));

        manager.update_window_size(1, 1024, 768).unwrap();
        assert_eq!(manager.get_window_size(1).unwrap(), (1024, 768));
    }

    #[test]
    fn test_multiple_windows() {
        let manager = SurfaceManager::new();
        manager.map_window_to_surface(1, 1, 800, 600).unwrap();
        manager.map_window_to_surface(2, 2, 1024, 768).unwrap();

        assert_eq!(manager.window_count(), 2);
        assert_eq!(manager.list_all_windows().len(), 2);
    }
}
