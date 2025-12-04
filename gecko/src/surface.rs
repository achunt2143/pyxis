// gecko/src/surface.rs
//! Wayland surface abstraction

use anyhow::Result;
use tracing::debug;

/// Surface configuration for Wayland
#[derive(Debug, Clone)]
pub struct SurfaceConfig {
    /// Surface width
    pub width: u32,
    /// Surface height
    pub height: u32,
    /// Requested frame rate
    pub fps: u32,
}

impl Default for SurfaceConfig {
    fn default() -> Self {
        SurfaceConfig {
            width: 1024,
            height: 768,
            fps: 60,
        }
    }
}

/// Represents a Wayland surface
#[derive(Debug, Clone)]
pub struct Surface {
    surface_id: u32,
    config: SurfaceConfig,
    attached: bool,
    committed: bool,
}

impl Surface {
    /// Create new surface
    pub fn new(surface_id: u32, config: SurfaceConfig) -> Self {
        Surface {
            surface_id,
            config,
            attached: false,
            committed: false,
        }
    }

    /// Get surface ID
    pub fn surface_id(&self) -> u32 {
        self.surface_id
    }

    /// Get configuration
    pub fn config(&self) -> &SurfaceConfig {
        &self.config
    }

    /// Get dimensions
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Mark buffer as attached (ready for content)
    pub fn mark_attached(&mut self) {
        debug!("Surface {} buffer attached", self.surface_id);
        self.attached = true;
    }

    /// Check if buffer is attached
    pub fn is_attached(&self) -> bool {
        self.attached
    }

    /// Mark changes as committed
    pub fn mark_committed(&mut self) {
        debug!("Surface {} committed", self.surface_id);
        self.committed = true;
    }

    /// Check if committed
    pub fn is_committed(&self) -> bool {
        self.committed
    }

    /// Check if surface is ready for rendering
    pub fn is_ready(&self) -> bool {
        self.attached && self.committed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_surface_creation() {
        let config = SurfaceConfig {
            width: 800,
            height: 600,
            fps: 60,
        };
        let surface = Surface::new(1, config);
        assert_eq!(surface.surface_id(), 1);
        assert_eq!(surface.size(), (800, 600));
    }

    #[test]
    fn test_surface_lifecycle() {
        let config = SurfaceConfig::default();
        let mut surface = Surface::new(1, config);

        assert!(!surface.is_ready());

        surface.mark_attached();
        assert!(surface.is_attached());
        assert!(!surface.is_ready());

        surface.mark_committed();
        assert!(surface.is_committed());
        assert!(surface.is_ready());
    }
}
