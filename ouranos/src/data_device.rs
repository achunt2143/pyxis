//! Data Device Protocol Support
//!
//! Implements clipboard management and drag-and-drop functionality

use std::collections::HashMap;
use std::sync::Arc;
use anyhow::Result;

/// MIME type constants
pub mod mime_types {
    pub const TEXT_PLAIN: &str = "text/plain";
    pub const TEXT_UTF8: &str = "text/plain;charset=utf-8";
    pub const TEXT_HTML: &str = "text/html";
    pub const IMAGE_PNG: &str = "image/png";
    pub const IMAGE_JPEG: &str = "image/jpeg";
    pub const FILE_URI: &str = "text/uri-list";
}

/// Data offer containing MIME types
#[derive(Debug, Clone)]
pub struct DataOffer {
    pub mime_types: Vec<String>,
    pub data: Arc<Vec<u8>>,
}

impl DataOffer {
    pub fn new(mime_types: Vec<String>, data: Vec<u8>) -> Self {
        DataOffer {
            mime_types,
            data: Arc::new(data),
        }
    }

    pub fn get_data(&self, mime_type: &str) -> Option<&[u8]> {
        if self.mime_types.contains(&mime_type.to_string()) {
            Some(&self.data)
        } else {
            None
        }
    }
}

/// Selection types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectionType {
    Clipboard,
    Primary,
    Secondary,
}

/// Selection data manager
pub struct SelectionManager {
    selections: HashMap<SelectionType, Option<DataOffer>>,
}

impl SelectionManager {
    pub fn new() -> Self {
        SelectionManager {
            selections: HashMap::new(),
        }
    }

    /// Set selection data
    pub fn set_selection(&mut self, selection_type: SelectionType, offer: DataOffer) {
        self.selections.insert(selection_type, Some(offer));
        tracing::debug!("Selection set: {:?}", selection_type);
    }

    /// Get selection data
    pub fn get_selection(&self, selection_type: SelectionType) -> Option<&DataOffer> {
        self.selections.get(&selection_type).and_then(|opt| opt.as_ref())
    }

    /// Clear selection
    pub fn clear_selection(&mut self, selection_type: SelectionType) {
        self.selections.insert(selection_type, None);
        tracing::debug!("Selection cleared: {:?}", selection_type);
    }
}

/// Clipboard manager
pub struct ClipboardManager {
    clipboard_data: HashMap<String, Vec<u8>>,
    selection_manager: SelectionManager,
}

impl ClipboardManager {
    pub fn new() -> Self {
        ClipboardManager {
            clipboard_data: HashMap::new(),
            selection_manager: SelectionManager::new(),
        }
    }

    /// Copy text to clipboard
    pub fn copy_text(&mut self, text: &str) -> Result<()> {
        let offer = DataOffer::new(
            vec![mime_types::TEXT_PLAIN.to_string()],
            text.as_bytes().to_vec(),
        );
        self.selection_manager.set_selection(SelectionType::Clipboard, offer);
        tracing::debug!("Text copied: {} bytes", text.len());
        Ok(())
    }

    /// Paste text from clipboard
    pub fn paste_text(&self) -> Option<String> {
        if let Some(offer) = self.selection_manager.get_selection(SelectionType::Clipboard) {
            if let Some(data) = offer.get_data(mime_types::TEXT_PLAIN) {
                if let Ok(text) = String::from_utf8(data.to_vec()) {
                    tracing::debug!("Text pasted: {} bytes", text.len());
                    return Some(text);
                }
            }
        }
        None
    }

    /// Copy binary data with MIME type
    pub fn copy_data(&mut self, mime_type: &str, data: Vec<u8>) -> Result<()> {
        let offer = DataOffer::new(vec![mime_type.to_string()], data);
        self.selection_manager.set_selection(SelectionType::Clipboard, offer);
        tracing::debug!("Data copied: {} MIME type: {}", offer.data.len(), mime_type);
        Ok(())
    }

    /// Get clipboard data as specific MIME type
    pub fn get_data(&self, mime_type: &str) -> Option<Vec<u8>> {
        if let Some(offer) = self.selection_manager.get_selection(SelectionType::Clipboard) {
            offer.get_data(mime_type).map(|data| data.to_vec())
        } else {
            None
        }
    }

    /// Check if clipboard has data
    pub fn has_data(&self) -> bool {
        self.selection_manager.get_selection(SelectionType::Clipboard).is_some()
    }

    /// Clear clipboard
    pub fn clear(&mut self) {
        self.selection_manager.clear_selection(SelectionType::Clipboard);
    }
}

/// Drag and drop operation
#[derive(Debug, Clone)]
pub struct DragOperation {
    pub drag_id: u32,
    pub source_window: u32,
    pub target_window: Option<u32>,
    pub offer: DataOffer,
    pub x: i32,
    pub y: i32,
    pub actions: u32, // Bitmask: copy=1, move=2, ask=4
    pub selected_action: u32,
}

/// Drag and drop manager
pub struct DragDropManager {
    active_drag: Option<DragOperation>,
    next_drag_id: u32,
    drop_targets: Vec<u32>, // Window IDs that accept drops
}

impl DragDropManager {
    pub fn new() -> Self {
        DragDropManager {
            active_drag: None,
            next_drag_id: 1,
            drop_targets: Vec::new(),
        }
    }

    /// Start drag operation
    pub fn start_drag(
        &mut self,
        source_window: u32,
        mime_types: Vec<String>,
        data: Vec<u8>,
        actions: u32,
    ) -> u32 {
        let drag_id = self.next_drag_id;
        self.next_drag_id += 1;

        let offer = DataOffer::new(mime_types, data);
        let drag = DragOperation {
            drag_id,
            source_window,
            target_window: None,
            offer,
            x: 0,
            y: 0,
            actions,
            selected_action: 0,
        };

        self.active_drag = Some(drag);
        tracing::debug!("Drag started: id={}", drag_id);
        drag_id
    }

    /// Update drag position
    pub fn update_drag_position(&mut self, x: i32, y: i32) -> Option<u32> {
        if let Some(ref mut drag) = self.active_drag {
            drag.x = x;
            drag.y = y;
            
            // Find drop target at position (simplified)
            for &target in &self.drop_targets {
                return Some(target);
            }
        }
        None
    }

    /// Drop operation
    pub fn drop(&mut self, x: i32, y: i32, target_window: u32) -> Result<()> {
        if let Some(drag) = self.active_drag.take() {
            tracing::info!("Drop at ({}, {}) to window {}", x, y, target_window);
            return Ok(());
        }
        anyhow::bail!("No active drag operation")
    }

    /// Cancel drag
    pub fn cancel_drag(&mut self) -> Result<()> {
        if let Some(drag) = self.active_drag.take() {
            tracing::debug!("Drag cancelled: id={}", drag.drag_id);
            Ok(())
        } else {
            anyhow::bail!("No active drag operation")
        }
    }

    /// Register drop target window
    pub fn register_drop_target(&mut self, window_id: u32) {
        if !self.drop_targets.contains(&window_id) {
            self.drop_targets.push(window_id);
            tracing::debug!("Drop target registered: {}", window_id);
        }
    }

    /// Unregister drop target window
    pub fn unregister_drop_target(&mut self, window_id: u32) {
        self.drop_targets.retain(|&id| id != window_id);
        tracing::debug!("Drop target unregistered: {}", window_id);
    }

    /// Check if window is drop target
    pub fn is_drop_target(&self, window_id: u32) -> bool {
        self.drop_targets.contains(&window_id)
    }

    /// Get active drag
    pub fn get_active_drag(&self) -> Option<&DragOperation> {
        self.active_drag.as_ref()
    }

    /// Get drag data
    pub fn get_drag_data(&self, mime_type: &str) -> Option<Vec<u8>> {
        if let Some(drag) = &self.active_drag {
            drag.offer.get_data(mime_type).map(|data| data.to_vec())
        } else {
            None
        }
    }
}

/// Data device state holder
pub struct DataDeviceState {
    pub clipboard: ClipboardManager,
    pub drag_drop: DragDropManager,
}

impl DataDeviceState {
    pub fn new() -> Self {
        DataDeviceState {
            clipboard: ClipboardManager::new(),
            drag_drop: DragDropManager::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_offer() {
        let offer = DataOffer::new(
            vec![mime_types::TEXT_PLAIN.to_string()],
            b"Hello, World!".to_vec(),
        );
        assert_eq!(offer.mime_types.len(), 1);
        assert!(offer.get_data(mime_types::TEXT_PLAIN).is_some());
    }

    #[test]
    fn test_clipboard_copy_paste() {
        let mut clipboard = ClipboardManager::new();
        clipboard.copy_text("Test text").unwrap();
        
        let pasted = clipboard.paste_text();
        assert_eq!(pasted, Some("Test text".to_string()));
    }

    #[test]
    fn test_clipboard_has_data() {
        let mut clipboard = ClipboardManager::new();
        assert!(!clipboard.has_data());
        
        clipboard.copy_text("Some data").unwrap();
        assert!(clipboard.has_data());
        
        clipboard.clear();
        assert!(!clipboard.has_data());
    }

    #[test]
    fn test_drag_operation() {
        let mut drag_drop = DragDropManager::new();
        let drag_id = drag_drop.start_drag(
            1,
            vec![mime_types::TEXT_PLAIN.to_string()],
            b"dragged data".to_vec(),
            1, // Copy action
        );
        
        assert_eq!(drag_id, 1);
        assert!(drag_drop.get_active_drag().is_some());
    }

    #[test]
    fn test_drop_targets() {
        let mut drag_drop = DragDropManager::new();
        drag_drop.register_drop_target(1);
        drag_drop.register_drop_target(2);
        
        assert!(drag_drop.is_drop_target(1));
        assert!(drag_drop.is_drop_target(2));
        assert!(!drag_drop.is_drop_target(3));
        
        drag_drop.unregister_drop_target(1);
        assert!(!drag_drop.is_drop_target(1));
    }
}
