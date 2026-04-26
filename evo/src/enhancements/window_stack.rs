//! Window stacking and z-order management

use smithay::desktop::{Space, WindowSurface};
use crate::shell::WindowElement;

/// Window stacking operations
pub trait WindowStack {
    /// Raise window to top of stack
    fn raise_window(&mut self, window: &WindowElement);
    
    /// Lower window to bottom of stack
    fn lower_window(&mut self, window: &WindowElement);
    
    /// Raise window above another
    fn raise_above(&mut self, window: &WindowElement, above: &WindowElement);
    
    /// Lower window below another
    fn lower_below(&mut self, window: &WindowElement, below: &WindowElement);
    
    /// Get z-order index of window (0 = bottom)
    fn window_z_index(&self, window: &WindowElement) -> Option<usize>;
}

impl WindowStack for Space<WindowElement> {
    fn raise_window(&mut self, window: &WindowElement) {
        // Smithay's space.raise_element already handles this
        self.raise_element(window, true);
    }
    
    fn lower_window(&mut self, window: &WindowElement) {
        // Get all windows
        let windows: Vec<_> = self.elements().cloned().collect();
        
        // Find our window
        if windows.iter().any(|w| w == window) {
            // Unmap and remap at bottom
            if let Some(loc) = self.element_location(window) {
                self.unmap_elem(window);
                
                // Re-add at bottom (activate = false keeps it at bottom)
                self.map_element(window.clone(), loc, false);
            }
        }
    }
    
    fn raise_above(&mut self, window: &WindowElement, above: &WindowElement) {
        // First raise the reference window
        self.raise_element(above, false);
        // Then raise our window (will be on top of above)
        self.raise_element(window, false);
    }
    
    fn lower_below(&mut self, window: &WindowElement, below: &WindowElement) {
        // Lower reference window first
        if let Some(loc) = self.element_location(below) {
            self.unmap_elem(below);
            self.map_element(below.clone(), loc, false);
        }
        // Then lower our window
        if let Some(loc) = self.element_location(window) {
            self.unmap_elem(window);
            self.map_element(window.clone(), loc, false);
        }
    }
    
    fn window_z_index(&self, window: &WindowElement) -> Option<usize> {
        self.elements()
            .position(|w| w == window)
    }
}
