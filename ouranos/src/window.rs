use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WindowState {
    Normal,
    Maximized,
    Minimized,
    Fullscreen,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Default for WindowGeometry {
    fn default() -> Self {
        WindowGeometry {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowMetadata {
    pub title: String,
    pub app_id: String,
    pub state: WindowState,
    pub geometry: WindowGeometry,
    pub parent_id: Option<u32>,
    pub is_focused: bool,
    pub is_decorated: bool,
    pub workspace: u32,
}

impl Default for WindowMetadata {
    fn default() -> Self {
        WindowMetadata {
            title: "Untitled".to_string(),
            app_id: "unknown".to_string(),
            state: WindowState::Normal,
            geometry: WindowGeometry::default(),
            parent_id: None,
            is_focused: false,
            is_decorated: true,
            workspace: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowEvent {
    pub window_id: u32,
    pub event_type: WindowEventType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WindowEventType {
    Created { metadata: WindowMetadata },
    Destroyed,
    FocusGained,
    FocusLost,
    StateChanged { new_state: WindowState },
    GeometryChanged { new_geometry: WindowGeometry },
    TitleChanged { new_title: String },
}

pub struct WindowStateManager {
    windows: HashMap<u32, WindowMetadata>,
    next_id: u32,
}

impl WindowStateManager {
    pub fn new() -> Self {
        WindowStateManager {
            windows: HashMap::new(),
            next_id: 1,
        }
    }

    /// Create and register a new window
    pub fn create_window(&mut self, mut metadata: WindowMetadata) -> u32 {
        let window_id = self.next_id;
        self.next_id += 1;

        tracing::debug!("Creating window {} ({})", window_id, metadata.app_id);
        self.windows.insert(window_id, metadata);

        window_id
    }

    /// Get window metadata by ID
    pub fn get_window(&self, window_id: u32) -> Option<&WindowMetadata> {
        self.windows.get(&window_id)
    }

    /// Get mutable window metadata
    pub fn get_window_mut(&mut self, window_id: u32) -> Option<&mut WindowMetadata> {
        self.windows.get_mut(&window_id)
    }

    /// Destroy a window
    pub fn destroy_window(&mut self, window_id: u32) -> Option<WindowMetadata> {
        tracing::debug!("Destroying window {}", window_id);
        self.windows.remove(&window_id)
    }

    /// List all windows in a workspace
    pub fn list_windows(&self, workspace: u32) -> Vec<(u32, &WindowMetadata)> {
        self.windows
            .iter()
            .filter(|(_, metadata)| metadata.workspace == workspace)
            .map(|(id, metadata)| (*id, metadata))
            .collect()
    }

    /// Get all windows
    pub fn all_windows(&self) -> Vec<(u32, &WindowMetadata)> {
        self.windows.iter().map(|(id, metadata)| (*id, metadata)).collect()
    }

    /// Set window state
    pub fn set_window_state(&mut self, window_id: u32, new_state: WindowState) -> bool {
        if let Some(window) = self.get_window_mut(window_id) {
            window.state = new_state;
            tracing::debug!("Window {} state changed to {:?}", window_id, new_state);
            true
        } else {
            false
        }
    }

    /// Set window geometry
    pub fn set_window_geometry(
        &mut self,
        window_id: u32,
        geometry: WindowGeometry,
    ) -> bool {
        if let Some(window) = self.get_window_mut(window_id) {
            window.geometry = geometry;
            tracing::debug!("Window {} geometry changed", window_id);
            true
        } else {
            false
        }
    }

    /// Set focus
    pub fn set_focus(&mut self, window_id: Option<u32>) {
        // Clear all focus
        for (_, metadata) in &mut self.windows {
            metadata.is_focused = false;
        }

        // Set new focus
        if let Some(id) = window_id {
            if let Some(metadata) = self.get_window_mut(id) {
                metadata.is_focused = true;
                tracing::debug!("Window {} focused", id);
            }
        }
    }

    /// Get focused window
    pub fn get_focused_window(&self) -> Option<(u32, &WindowMetadata)> {
        self.windows
            .iter()
            .find(|(_, metadata)| metadata.is_focused)
            .map(|(id, metadata)| (*id, metadata))
    }

    /// Count windows
    pub fn count_windows(&self) -> usize {
        self.windows.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_creation() {
        let mut manager = WindowStateManager::new();
        let id = manager.create_window(WindowMetadata::default());
        assert_eq!(id, 1);
        assert!(manager.get_window(id).is_some());
    }

    #[test]
    fn test_window_destruction() {
        let mut manager = WindowStateManager::new();
        let id = manager.create_window(WindowMetadata::default());
        assert!(manager.destroy_window(id).is_some());
        assert!(manager.get_window(id).is_none());
    }

    #[test]
    fn test_focus_management() {
        let mut manager = WindowStateManager::new();
        let id1 = manager.create_window(WindowMetadata::default());
        let id2 = manager.create_window(WindowMetadata::default());

        manager.set_focus(Some(id1));
        assert!(manager.get_window(id1).unwrap().is_focused);
        assert!(!manager.get_window(id2).unwrap().is_focused);

        manager.set_focus(Some(id2));
        assert!(!manager.get_window(id1).unwrap().is_focused);
        assert!(manager.get_window(id2).unwrap().is_focused);
    }
}
