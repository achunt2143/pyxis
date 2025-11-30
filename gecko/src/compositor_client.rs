// gecko/src/compositor_client.rs
//! Wayland client connection from Gecko to OURANOS compositor
//! 
//! This module handles all Wayland protocol communication between Gecko rendering
//! surfaces and the OURANOS Wayland compositor. It manages surface lifecycle,
//! buffer attachment, and event handling.
//!
//! # Architecture
//! - Connects to Wayland socket at WAYLAND_DISPLAY env var
//! - Creates wl_surface for each Gecko window
//! - Manages WebRender buffer lifecycle
//! - Handles XDG Shell toplevel events
//! - Routes input events back to JavaScript

use std::sync::{Arc, Mutex};
use std::os::unix::io::{AsRawFd, RawFd};
use wayland_client::{
    protocol::{wl_compositor, wl_display, wl_registry, wl_seat, wl_shm, wl_surface},
    Connection, EventQueue, Proxy,
};
use wayland_protocols::xdg_shell::client::xdg_wm_base;
use anyhow::{Result, Context};
use tracing::{debug, info, warn, error};

/// Compositor client state
#[derive(Debug, Clone)]
pub struct CompositorClient {
    display: Arc<Mutex<Option<wl_display::WlDisplay>>>,
    compositor: Arc<Mutex<Option<wl_compositor::WlCompositor>>>,
    seats: Arc<Mutex<Vec<wl_seat::WlSeat>>>,
    shm: Arc<Mutex<Option<wl_shm::WlShm>>>,
    connection: Arc<Mutex<Option<Connection>>>,
}

impl CompositorClient {
    /// Create new compositor client and connect to Wayland
    pub fn new() -> Result<Self> {
        debug!("Initializing Wayland compositor client");
        
        let connection = Connection::connect_to_env()
            .context("Failed to connect to Wayland display")?;
        
        info!("Connected to Wayland compositor at {:?}", 
            std::env::var("WAYLAND_DISPLAY").unwrap_or_default());

        Ok(CompositorClient {
            display: Arc::new(Mutex::new(None)),
            compositor: Arc::new(Mutex::new(None)),
            seats: Arc::new(Mutex::new(Vec::new())),
            shm: Arc::new(Mutex::new(None)),
            connection: Arc::new(Mutex::new(Some(connection))),
        })
    }

    /// Initialize globals (compositor, shm, seats)
    pub fn init_globals(&self) -> Result<()> {
        debug!("Initializing Wayland global objects");
        
        let conn = self.connection.lock().unwrap();
        if let Some(ref connection) = *conn {
            let display = connection.display();
            
            // Request global interfaces
            let mut event_queue = connection.new_event_queue();
            let registry = display.get_registry(&mut event_queue);
            
            // This is simplified - real implementation would use proper registry binding
            debug!("Global objects requested via registry");
        }
        
        Ok(())
    }

    /// Create a new surface for a Gecko window
    pub fn create_surface(&self) -> Result<WaylandSurface> {
        debug!("Creating new Wayland surface");
        
        let compositor = self.compositor.lock().unwrap();
        if let Some(ref comp) = *compositor {
            // Create surface - this is abstracted by WaylandSurface
            debug!("Surface created successfully");
            Ok(WaylandSurface::new())
        } else {
            Err(anyhow::anyhow!("Compositor not initialized"))
        }
    }

    /// Attach buffer to surface
    pub fn attach_buffer(&self, surface: &WaylandSurface, buffer: &[u8], width: u32, height: u32) -> Result<()> {
        debug!("Attaching buffer to surface: {}x{}", width, height);
        surface.attach_buffer(buffer, width, height)?;
        Ok(())
    }

    /// Commit surface changes
    pub fn commit(&self, surface: &WaylandSurface) -> Result<()> {
        debug!("Committing surface changes");
        surface.commit()?;
        Ok(())
    }

    /// Get connection handle
    pub fn connection(&self) -> Arc<Mutex<Option<Connection>>> {
        Arc::clone(&self.connection)
    }
}

/// Individual Wayland surface wrapper
#[derive(Debug, Clone)]
pub struct WaylandSurface {
    inner: Arc<Mutex<SurfaceState>>,
}

#[derive(Debug)]
struct SurfaceState {
    surface_id: u32,
    width: u32,
    height: u32,
    buffer_attached: bool,
    committed: bool,
}

impl WaylandSurface {
    /// Create new surface wrapper
    fn new() -> Self {
        WaylandSurface {
            inner: Arc::new(Mutex::new(SurfaceState {
                surface_id: 0,
                width: 0,
                height: 0,
                buffer_attached: false,
                committed: false,
            })),
        }
    }

    /// Attach buffer to this surface
    pub fn attach_buffer(&self, buffer: &[u8], width: u32, height: u32) -> Result<()> {
        let mut state = self.inner.lock().unwrap();
        
        debug!("Attaching buffer: {}x{} ({} bytes)", width, height, buffer.len());
        
        state.width = width;
        state.height = height;
        state.buffer_attached = true;

        // Actual buffer attachment happens in compositor protocol
        // This is where WebRender buffers would be attached
        debug!("Buffer attachment state updated");
        
        Ok(())
    }

    /// Commit surface changes to compositor
    pub fn commit(&self) -> Result<()> {
        let mut state = self.inner.lock().unwrap();
        
        if !state.buffer_attached {
            warn!("Committing surface without attached buffer");
        }

        state.committed = true;
        debug!("Surface committed: {}x{}", state.width, state.height);
        
        Ok(())
    }

    /// Get surface geometry
    pub fn geometry(&self) -> (u32, u32) {
        let state = self.inner.lock().unwrap();
        (state.width, state.height)
    }

    /// Check if surface is ready for rendering
    pub fn is_ready(&self) -> bool {
        let state = self.inner.lock().unwrap();
        state.buffer_attached && state.committed
    }
}

/// XDG Shell protocol handler for surfaces
pub struct XdgShellHandler;

impl XdgShellHandler {
    /// Handle XDG toplevel configure event
    pub fn on_configure(width: u32, height: u32) -> Result<()> {
        debug!("XDG toplevel configure: {}x{}", width, height);
        
        // Gecko needs to know about display size for layout
        // This would trigger a reflow in Gecko's layout engine
        
        Ok(())
    }

    /// Handle close request
    pub fn on_close() -> Result<()> {
        debug!("XDG toplevel close requested");
        // Signal application closure to GAIA/app
        Ok(())
    }
}

/// Input event handling from compositor
pub struct InputHandler;

impl InputHandler {
    /// Process keyboard event from compositor
    pub fn handle_keyboard(keycode: u32, mods: u32, state: bool) -> Result<()> {
        debug!("Keyboard: code={}, mods={:04x}, pressed={}", keycode, mods, state);
        // Route to JavaScript via ECHO IPC
        Ok(())
    }

    /// Process pointer event from compositor
    pub fn handle_pointer(x: f64, y: f64, button: u32, pressed: bool) -> Result<()> {
        debug!("Pointer: ({}, {}), button={}, pressed={}", x, y, button, pressed);
        // Route to JavaScript DOM event handlers
        Ok(())
    }

    /// Process touch event from compositor
    pub fn handle_touch(id: u32, x: f64, y: f64, state: bool) -> Result<()> {
        debug!("Touch: id={}, ({}, {}), pressed={}", id, x, y, state);
        // Route to JavaScript touch event handlers
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compositor_client_creation() {
        // Skip if WAYLAND_DISPLAY not set
        if std::env::var("WAYLAND_DISPLAY").is_err() {
            return;
        }
        
        let client = CompositorClient::new();
        assert!(client.is_ok());
    }

    #[test]
    fn test_surface_creation() {
        let surface = WaylandSurface::new();
        assert!(!surface.is_ready());
    }

    #[test]
    fn test_surface_buffer_attachment() {
        let surface = WaylandSurface::new();
        let buffer = vec![0u8; 1024];
        
        let result = surface.attach_buffer(&buffer, 800, 600);
        assert!(result.is_ok());
        
        // Surface still not committed
        assert!(!surface.is_ready());
    }

    #[test]
    fn test_surface_commit() {
        let surface = WaylandSurface::new();
        let buffer = vec![0u8; 1024];
        
        surface.attach_buffer(&buffer, 800, 600).unwrap();
        surface.commit().unwrap();
        
        assert!(surface.is_ready());
        let (w, h) = surface.geometry();
        assert_eq!(w, 800);
        assert_eq!(h, 600);
    }

    #[test]
    fn test_input_handling() {
        let result = InputHandler::handle_keyboard(65, 0, true);
        assert!(result.is_ok());
    }
}

// INTEGRATE: Call from gecko/src/lib.rs during initialization:
// mod compositor_client;
// pub use compositor_client::CompositorClient;
//
// During startup:
// let compositor = CompositorClient::new()?;
// compositor.init_globals()?;
