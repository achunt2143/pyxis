//! Server-Side Window Decorations
//!
//! Provides window frame rendering with title bar, control buttons, and borders

use crate::window::{WindowGeometry, WindowState};
use std::collections::HashMap;

/// Window decoration configuration
#[derive(Debug, Clone)]
pub struct DecorationConfig {
    pub title_bar_height: u32,
    pub border_width: u32,
    pub button_size: u32,
    pub shadow_blur: u32,
    pub corner_radius: u32,
    pub enabled: bool,
}

impl Default for DecorationConfig {
    fn default() -> Self {
        DecorationConfig {
            title_bar_height: 35,
            border_width: 4,
            button_size: 24,
            shadow_blur: 8,
            corner_radius: 8,
            enabled: true,
        }
    }
}

/// Window control button types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlButton {
    Close,
    Maximize,
    Minimize,
}

/// Button state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Normal,
    Hovered,
    Active,
    Disabled,
}

/// Window decoration frame
pub struct WindowDecoration {
    pub window_id: u32,
    pub config: DecorationConfig,
    pub title: String,
    pub app_id: String,
    pub is_focused: bool,
    pub button_states: HashMap<ControlButton, ButtonState>,
    pub theme: Theme,
}

/// Color theme
#[derive(Debug, Clone)]
pub struct Theme {
    pub background: (u8, u8, u8, u8),      // RGBA
    pub text_color: (u8, u8, u8, u8),
    pub border_color: (u8, u8, u8, u8),
    pub focus_color: (u8, u8, u8, u8),
    pub button_bg: (u8, u8, u8, u8),
    pub button_hover: (u8, u8, u8, u8),
    pub button_active: (u8, u8, u8, u8),
    pub shadow_color: (u8, u8, u8, u8),
}

impl Default for Theme {
    fn default() -> Self {
        // Light theme
        Theme {
            background: (240, 240, 240, 255),
            text_color: (30, 30, 30, 255),
            border_color: (200, 200, 200, 255),
            focus_color: (0, 150, 200, 255),
            button_bg: (220, 220, 220, 255),
            button_hover: (200, 200, 200, 255),
            button_active: (180, 180, 180, 255),
            shadow_color: (0, 0, 0, 50),
        }
    }
}

impl Theme {
    pub fn dark() -> Self {
        Theme {
            background: (50, 50, 50, 255),
            text_color: (230, 230, 230, 255),
            border_color: (100, 100, 100, 255),
            focus_color: (0, 200, 255, 255),
            button_bg: (80, 80, 80, 255),
            button_hover: (100, 100, 100, 255),
            button_active: (120, 120, 120, 255),
            shadow_color: (0, 0, 0, 100),
        }
    }
}

impl WindowDecoration {
    pub fn new(window_id: u32, title: String, app_id: String) -> Self {
        let mut button_states = HashMap::new();
        button_states.insert(ControlButton::Close, ButtonState::Normal);
        button_states.insert(ControlButton::Maximize, ButtonState::Normal);
        button_states.insert(ControlButton::Minimize, ButtonState::Normal);

        WindowDecoration {
            window_id,
            config: DecorationConfig::default(),
            title,
            app_id,
            is_focused: false,
            button_states,
            theme: Theme::default(),
        }
    }

    /// Calculate decoration regions
    pub fn get_regions(&self, window_geom: &WindowGeometry) -> DecorationRegions {
        DecorationRegions {
            title_bar: Rect {
                x: window_geom.x,
                y: window_geom.y,
                width: window_geom.width as i32,
                height: self.config.title_bar_height as i32,
            },
            top_border: Rect {
                x: window_geom.x,
                y: window_geom.y,
                width: window_geom.width as i32,
                height: self.config.border_width as i32,
            },
            left_border: Rect {
                x: window_geom.x,
                y: window_geom.y + self.config.border_width as i32,
                width: self.config.border_width as i32,
                height: window_geom.height as i32 - 2 * self.config.border_width as i32,
            },
            right_border: Rect {
                x: window_geom.x + window_geom.width as i32 - self.config.border_width as i32,
                y: window_geom.y + self.config.border_width as i32,
                width: self.config.border_width as i32,
                height: window_geom.height as i32 - 2 * self.config.border_width as i32,
            },
            bottom_border: Rect {
                x: window_geom.x,
                y: window_geom.y + window_geom.height as i32 - self.config.border_width as i32,
                width: window_geom.width as i32,
                height: self.config.border_width as i32,
            },
            close_button: self.get_button_rect(ControlButton::Close, window_geom),
            maximize_button: self.get_button_rect(ControlButton::Maximize, window_geom),
            minimize_button: self.get_button_rect(ControlButton::Minimize, window_geom),
        }
    }

    /// Get button rectangle
    fn get_button_rect(&self, button: ControlButton, window_geom: &WindowGeometry) -> Rect {
        let button_size = self.config.button_size as i32;
        let padding = 8i32;
        
        let base_x = window_geom.x + window_geom.width as i32 - button_size - padding;
        let y = window_geom.y + (self.config.title_bar_height as i32 - button_size) / 2;
        
        let offset = match button {
            ControlButton::Close => 0,
            ControlButton::Maximize => button_size + padding,
            ControlButton::Minimize => 2 * (button_size + padding),
        };
        
        Rect {
            x: base_x - offset,
            y,
            width: button_size,
            height: button_size,
        }
    }

    /// Check if point is over a button
    pub fn get_button_at(&self, x: i32, y: i32, window_geom: &WindowGeometry) -> Option<ControlButton> {
        let regions = self.get_regions(window_geom);
        
        if regions.close_button.contains(x, y) {
            return Some(ControlButton::Close);
        }
        if regions.maximize_button.contains(x, y) {
            return Some(ControlButton::Maximize);
        }
        if regions.minimize_button.contains(x, y) {
            return Some(ControlButton::Minimize);
        }
        
        None
    }

    /// Check if point is on title bar (drag handle)
    pub fn is_on_title_bar(&self, x: i32, y: i32, window_geom: &WindowGeometry) -> bool {
        let regions = self.get_regions(window_geom);
        regions.title_bar.contains(x, y)
    }

    /// Check if point is on a resize handle
    pub fn get_resize_handle(&self, x: i32, y: i32, window_geom: &WindowGeometry) -> Option<ResizeHandle> {
        let regions = self.get_regions(window_geom);
        let border = self.config.border_width as i32;
        
        // Top-left
        if regions.top_border.contains(x, y) && regions.left_border.contains(x, y) {
            return Some(ResizeHandle::TopLeft);
        }
        // Top-right
        if regions.top_border.contains(x, y) && regions.right_border.contains(x, y) {
            return Some(ResizeHandle::TopRight);
        }
        // Bottom-left
        if regions.bottom_border.contains(x, y) && regions.left_border.contains(x, y) {
            return Some(ResizeHandle::BottomLeft);
        }
        // Bottom-right
        if regions.bottom_border.contains(x, y) && regions.right_border.contains(x, y) {
            return Some(ResizeHandle::BottomRight);
        }
        // Top
        if regions.top_border.contains(x, y) {
            return Some(ResizeHandle::Top);
        }
        // Bottom
        if regions.bottom_border.contains(x, y) {
            return Some(ResizeHandle::Bottom);
        }
        // Left
        if regions.left_border.contains(x, y) {
            return Some(ResizeHandle::Left);
        }
        // Right
        if regions.right_border.contains(x, y) {
            return Some(ResizeHandle::Right);
        }
        
        None
    }

    /// Update button state on hover
    pub fn set_button_hover(&mut self, button: ControlButton, hovered: bool) {
        let new_state = if hovered {
            ButtonState::Hovered
        } else {
            ButtonState::Normal
        };
        self.button_states.insert(button, new_state);
    }

    /// Handle button click
    pub fn set_button_active(&mut self, button: ControlButton, active: bool) {
        let new_state = if active {
            ButtonState::Active
        } else {
            ButtonState::Normal
        };
        self.button_states.insert(button, new_state);
    }

    /// Get button action label
    pub fn get_button_label(&self, button: ControlButton) -> &'static str {
        match button {
            ControlButton::Close => "×",
            ControlButton::Maximize => "□",
            ControlButton::Minimize => "−",
        }
    }

    /// Set theme
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        tracing::debug!("Theme updated for window {}", self.window_id);
    }

    /// Enable/disable decorations
    pub fn set_enabled(&mut self, enabled: bool) {
        self.config.enabled = enabled;
        tracing::info!("Decorations {} for window {}", 
            if enabled { "enabled" } else { "disabled" },
            self.window_id);
    }
}

/// Decoration regions
pub struct DecorationRegions {
    pub title_bar: Rect,
    pub top_border: Rect,
    pub left_border: Rect,
    pub right_border: Rect,
    pub bottom_border: Rect,
    pub close_button: Rect,
    pub maximize_button: Rect,
    pub minimize_button: Rect,
}

/// Rectangle for hit testing
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// Resize handle positions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeHandle {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl ResizeHandle {
    pub fn cursor_name(&self) -> &'static str {
        match self {
            ResizeHandle::Top => "ns-resize",
            ResizeHandle::Bottom => "ns-resize",
            ResizeHandle::Left => "ew-resize",
            ResizeHandle::Right => "ew-resize",
            ResizeHandle::TopLeft => "nwse-resize",
            ResizeHandle::TopRight => "nesw-resize",
            ResizeHandle::BottomLeft => "nesw-resize",
            ResizeHandle::BottomRight => "nwse-resize",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decoration_creation() {
        let decoration = WindowDecoration::new(1, "Test".to_string(), "test.app".to_string());
        assert_eq!(decoration.window_id, 1);
        assert_eq!(decoration.title, "Test");
    }

    #[test]
    fn test_theme_creation() {
        let light = Theme::default();
        let dark = Theme::dark();
        assert_ne!(light.background, dark.background);
    }

    #[test]
    fn test_button_hit_test() {
        let decoration = WindowDecoration::new(1, "Test".to_string(), "test.app".to_string());
        let geom = WindowGeometry {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        
        let button = decoration.get_button_at(750, 10, &geom);
        assert!(button.is_some());
    }

    #[test]
    fn test_resize_handle_detection() {
        let decoration = WindowDecoration::new(1, "Test".to_string(), "test.app".to_string());
        let geom = WindowGeometry {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        
        let handle = decoration.get_resize_handle(2, 2, &geom);
        assert_eq!(handle, Some(ResizeHandle::TopLeft));
    }

    #[test]
    fn test_title_bar_detection() {
        let decoration = WindowDecoration::new(1, "Test".to_string(), "test.app".to_string());
        let geom = WindowGeometry {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        
        assert!(decoration.is_on_title_bar(400, 10, &geom));
        assert!(!decoration.is_on_title_bar(400, 100, &geom));
    }
}
