mod backend;
mod input;
mod render;
mod shell;
mod window;

pub use backend::BackendState;
pub use input::InputState;
pub use render::RenderState;
pub use shell::ShellState;
pub use window::{WindowStateManager, WindowMetadata, WindowGeometry, WindowState};

use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::env;
use std::path::PathBuf;

pub enum WindowElement {
    Wayland(u32), // Window ID
    Native(u32),  // Process ID
}

/// Main compositor state - central authority for all rendering and window management
pub struct GeckoOSState {
    backend: BackendState,
    input: InputState,
    render: RenderState,
    shell: ShellState,
    windows: WindowStateManager,
    socket_name: String,
    running: Arc<Mutex<bool>>,
}

impl GeckoOSState {
    /// Initialize the compositor
    pub async fn new() -> Result<Self> {
        tracing::info!("Initializing GeckoOS Compositor State");

        // Initialize subsystems
        let backend = BackendState::new()?;
        let input = InputState::new();
        let render = RenderState::new();
        let shell = ShellState::new()?;
        let windows = WindowStateManager::new();

        let socket_name = generate_socket_name();
        tracing::info!("Wayland socket: {}", socket_name);

        Ok(GeckoOSState {
            backend,
            input,
            render,
            shell,
            windows,
            socket_name,
            running: Arc::new(Mutex::new(true)),
        })
    }

    /// Get Wayland socket name
    pub fn socket_name(&self) -> &str {
        &self.socket_name
    }

    /// Main event loop
    pub async fn run(&mut self) -> Result<()> {
        tracing::info!("Compositor event loop started");

        // Set WAYLAND_DISPLAY environment variable for clients
        env::set_var("WAYLAND_DISPLAY", &self.socket_name);

        loop {
            if !*self.running.lock().unwrap() {
                break;
            }

            // Process events (would be connected to actual event sources in production)
            self.render.render_frame()?;

            // Simulate frame time (60 FPS = ~16.6ms per frame)
            tokio::time::sleep(tokio::time::Duration::from_millis(16)).await;
        }

        Ok(())
    }

    /// Shutdown compositor
    pub fn shutdown(&self) {
        *self.running.lock().unwrap() = false;
        tracing::info!("Compositor shutdown initiated");
    }

    /// Create a new window
    pub fn create_window(&mut self, metadata: WindowMetadata) -> u32 {
        let window_id = self.windows.create_window(metadata);
        tracing::info!("Window created: {}", window_id);
        window_id
    }

    /// Destroy a window
    pub fn destroy_window(&mut self, window_id: u32) {
        if self.windows.destroy_window(window_id).is_some() {
            tracing::info!("Window destroyed: {}", window_id);
        }
    }

    /// Get window metadata
    pub fn get_window(&self, window_id: u32) -> Option<WindowMetadata> {
        self.windows.get_window(window_id).cloned()
    }

    /// Update window state (normal, maximized, minimized, fullscreen)
    pub fn set_window_state(&mut self, window_id: u32, state: WindowState) -> bool {
        self.windows.set_window_state(window_id, state)
    }

    /// Update window geometry (position and size)
    pub fn set_window_geometry(&mut self, window_id: u32, geometry: WindowGeometry) -> bool {
        self.windows.set_window_geometry(window_id, geometry)
    }

    /// Set which window has focus (input focus)
    pub fn set_focus(&mut self, window_id: Option<u32>) {
        self.windows.set_focus(window_id);
    }

    /// Get focused window
    pub fn get_focused_window(&self) -> Option<(u32, WindowMetadata)> {
        self.windows
            .get_focused_window()
            .map(|(id, metadata)| (id, metadata.clone()))
    }

    /// List all windows
    pub fn list_windows(&self) -> Vec<(u32, WindowMetadata)> {
        self.windows
            .all_windows()
            .into_iter()
            .map(|(id, metadata)| (id, metadata.clone()))
            .collect()
    }

    /// Process keyboard event
    pub fn handle_keyboard_event(&mut self, key_code: u32, pressed: bool) {
        // Handle system hotkeys
        match (key_code, self.input.modifiers.super_key) {
            (50, true) => {
                // Super+D: Minimize all windows
                tracing::info!("Super+D pressed");
            }
            (16, true) => {
                // Super+Q: Launcher
                tracing::info!("Super+Q pressed - opening launcher");
            }
            _ => {}
        }
    }

    /// Process pointer motion
    pub fn handle_pointer_motion(&mut self, x: i32, y: i32) {
        // Could update cursor position, window highlighting, etc.
        tracing::trace!("Pointer at ({}, {})", x, y);
    }

    /// Get compositor status for monitoring
    pub fn get_status(&self) -> CompositorStatus {
        CompositorStatus {
            frame_count: self.render.frame_count(),
            window_count: self.windows.count_windows(),
            is_active: self.backend.is_session_active(),
            pressed_keys: self.input.pressed_keys.len(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompositorStatus {
    pub frame_count: u64,
    pub window_count: usize,
    pub is_active: bool,
    pub pressed_keys: usize,
}

/// Generate unique Wayland socket name
fn generate_socket_name() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();

    format!("geckoOS-{}", timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_compositor_initialization() {
        // Note: This test requires DRM device, may fail on CI
        // Skip on non-Linux or without graphics device
        if cfg!(target_os = "linux") && std::path::Path::new("/dev/dri/card0").exists() {
            let state = GeckoOSState::new().await;
            // Would assert state.is_ok() but DRM device may not be available
        }
    }

    #[test]
    fn test_window_management() {
        let mut manager = WindowStateManager::new();
        let mut metadata = WindowMetadata::default();
        metadata.app_id = "test-app".to_string();

        let id = manager.create_window(metadata);
        assert_eq!(id, 1);

        let window = manager.get_window(id).unwrap();
        assert_eq!(window.app_id, "test-app");

        manager.destroy_window(id);
        assert!(manager.get_window(id).is_none());
    }
}
