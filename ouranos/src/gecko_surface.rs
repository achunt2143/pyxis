// ouranos/src/gecko_surface.rs - Compositor-side Gecko Handling
//! Special handling for Gecko client surfaces in OURANOS compositor
//!
//! Gecko has special requirements:
//! - Privileged access to compositor resources
//! - Proper buffer lifecycle management
//! - XDG Shell toplevel configuration
//! - Direct rendering path optimization

use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use anyhow::{Result, anyhow};
use tracing::{debug, info, warn};

/// Gecko-specific surface wrapper
#[derive(Debug, Clone)]
pub struct GeckoSurface {
    surface_id: u32,
    is_gecko_client: bool,
    privileged: bool,
    buffer_id: u32,
    ready_for_rendering: bool,
}

impl GeckoSurface {
    /// Create a new Gecko surface
    pub fn new(surface_id: u32) -> Self {
        debug!("Creating Gecko surface: id={}", surface_id);
        GeckoSurface {
            surface_id,
            is_gecko_client: true,
            privileged: true,
            buffer_id: 0,
            ready_for_rendering: false,
        }
    }

    /// Check if this is a Gecko client surface
    pub fn is_gecko_client(&self) -> bool {
        self.is_gecko_client
    }

    /// Grant privileged access for rendering
    pub fn set_privileged(&mut self, privileged: bool) {
        debug!("Setting Gecko surface {} privileged: {}", self.surface_id, privileged);
        self.privileged = privileged;
    }

    /// Set buffer for rendering
    pub fn set_buffer(&mut self, buffer_id: u32) -> Result<()> {
        debug!("Setting buffer for Gecko surface {}: buffer={}", self.surface_id, buffer_id);
        self.buffer_id = buffer_id;
        Ok(())
    }

    /// Mark surface as ready for rendering
    pub fn mark_ready(&mut self) {
        debug!("Gecko surface {} ready for rendering", self.surface_id);
        self.ready_for_rendering = true;
    }

    /// Check if surface is ready
    pub fn is_ready(&self) -> bool {
        self.ready_for_rendering
    }
}

/// Gecko surface manager for compositor
pub struct GeckoSurfaceManager {
    surfaces: Arc<Mutex<HashMap<u32, GeckoSurface>>>,
}

impl GeckoSurfaceManager {
    /// Create new Gecko surface manager
    pub fn new() -> Self {
        debug!("Initializing Gecko surface manager");
        GeckoSurfaceManager {
            surfaces: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a new Gecko surface
    pub fn register_surface(&self, surface_id: u32) -> Result<()> {
        debug!("Registering Gecko surface: {}", surface_id);

        let mut surfaces = self.surfaces.lock().unwrap();
        if surfaces.contains_key(&surface_id) {
            return Err(anyhow!("Surface already registered: {}", surface_id));
        }

        let surface = GeckoSurface::new(surface_id);
        surfaces.insert(surface_id, surface);

        info!("Gecko surface registered: {}", surface_id);
        Ok(())
    }

    /// Get surface by ID
    pub fn get_surface(&self, surface_id: u32) -> Option<GeckoSurface> {
        let surfaces = self.surfaces.lock().unwrap();
        surfaces.get(&surface_id).cloned()
    }

    /// Update surface buffer
    pub fn update_buffer(&self, surface_id: u32, buffer_id: u32) -> Result<()> {
        debug!("Updating buffer for surface {}: buffer={}", surface_id, buffer_id);

        let mut surfaces = self.surfaces.lock().unwrap();
        let surface = surfaces.get_mut(&surface_id)
            .ok_or_else(|| anyhow!("Surface not found: {}", surface_id))?;

        surface.set_buffer(buffer_id)?;
        Ok(())
    }

    /// Mark surface as ready for rendering
    pub fn mark_ready(&self, surface_id: u32) -> Result<()> {
        debug!("Marking surface {} ready", surface_id);

        let mut surfaces = self.surfaces.lock().unwrap();
        let surface = surfaces.get_mut(&surface_id)
            .ok_or_else(|| anyhow!("Surface not found: {}", surface_id))?;

        surface.mark_ready();
        Ok(())
    }

    /// Unregister surface
    pub fn unregister_surface(&self, surface_id: u32) -> Result<()> {
        debug!("Unregistering Gecko surface: {}", surface_id);

        let mut surfaces = self.surfaces.lock().unwrap();
        if surfaces.remove(&surface_id).is_some() {
            info!("Gecko surface unregistered: {}", surface_id);
            Ok(())
        } else {
            Err(anyhow!("Surface not found: {}", surface_id))
        }
    }

    /// Get count of active Gecko surfaces
    pub fn count_surfaces(&self) -> usize {
        let surfaces = self.surfaces.lock().unwrap();
        surfaces.len()
    }

    /// List all Gecko surfaces
    pub fn list_surfaces(&self) -> Vec<u32> {
        let surfaces = self.surfaces.lock().unwrap();
        surfaces.keys().copied().collect()
    }
}

impl Default for GeckoSurfaceManager {
    fn default() -> Self {
        Self::new()
    }
}

/// XDG Toplevel configuration for Gecko windows
pub struct GeckoToplevelHandler;

impl GeckoToplevelHandler {
    /// Configure Gecko window as toplevel
    pub fn configure_toplevel(surface_id: u32, width: u32, height: u32) -> Result<()> {
        debug!("Configuring Gecko toplevel: surface_id={}, {}x{}", surface_id, width, height);

        // INTEGRATE: Set up XDG Shell toplevel state
        // This tells Gecko the configured size for layout

        Ok(())
    }

    /// Send close event to Gecko
    pub fn send_close(surface_id: u32) -> Result<()> {
        debug!("Sending close event to Gecko surface: {}", surface_id);

        // INTEGRATE: Signal Gecko to close window
        // Gecko should emit to ECHO bus for app termination

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gecko_surface_creation() {
        let surface = GeckoSurface::new(1);
        assert_eq!(surface.surface_id, 1);
        assert!(surface.is_gecko_client());
        assert!(surface.privileged);
        assert!(!surface.is_ready());
    }

    #[test]
    fn test_gecko_surface_manager_creation() {
        let manager = GeckoSurfaceManager::new();
        assert_eq!(manager.count_surfaces(), 0);
    }

    #[test]
    fn test_register_surface() {
        let manager = GeckoSurfaceManager::new();
        let result = manager.register_surface(1);
        assert!(result.is_ok());
        assert_eq!(manager.count_surfaces(), 1);
    }

    #[test]
    fn test_duplicate_registration() {
        let manager = GeckoSurfaceManager::new();
        manager.register_surface(1).unwrap();
        let result = manager.register_surface(1);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_surface() {
        let manager = GeckoSurfaceManager::new();
        manager.register_surface(1).unwrap();
        let surface = manager.get_surface(1);
        assert!(surface.is_some());
    }

    #[test]
    fn test_update_buffer() {
        let manager = GeckoSurfaceManager::new();
        manager.register_surface(1).unwrap();
        let result = manager.update_buffer(1, 100);
        assert!(result.is_ok());
    }

    #[test]
    fn test_mark_ready() {
        let manager = GeckoSurfaceManager::new();
        manager.register_surface(1).unwrap();
        manager.mark_ready(1).unwrap();
        
        let surface = manager.get_surface(1).unwrap();
        assert!(surface.is_ready());
    }

    #[test]
    fn test_unregister_surface() {
        let manager = GeckoSurfaceManager::new();
        manager.register_surface(1).unwrap();
        let result = manager.unregister_surface(1);
        assert!(result.is_ok());
        assert_eq!(manager.count_surfaces(), 0);
    }

    #[test]
    fn test_list_surfaces() {
        let manager = GeckoSurfaceManager::new();
        manager.register_surface(1).unwrap();
        manager.register_surface(2).unwrap();
        
        let list = manager.list_surfaces();
        assert_eq!(list.len(), 2);
    }
}

// INTEGRATE: Add to ouranos/src/lib.rs:
// pub mod gecko_surface;
// pub use gecko_surface::{GeckoSurfaceManager, GeckoSurface};
//
// During compositor initialization:
// let gecko_surfaces = GeckoSurfaceManager::new();
// In surface creation handler:
// if is_gecko_client {
//     gecko_surfaces.register_surface(surface_id)?;
// }
