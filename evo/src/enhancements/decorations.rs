//! Server-Side Window Decorations
//!
//! Provides window frame rendering with title bar, control buttons, and borders


use std::collections::HashMap;
use serde::{Deserialize, Serialize};

// ========== GEOMETRY TYPE ==========
/// Window geometry for decoration calculations
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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

// Conversion helper if you need to work with Smithay's Rectangle
impl From<WindowGeometry> for smithay::utils::Rectangle<i32, smithay::utils::Logical> {
    fn from(geom: WindowGeometry) -> Self {
        smithay::utils::Rectangle::from_loc_and_size(
            (geom.x, geom.y),
            (geom.width as i32, geom.height as i32),
        )
    }
}

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
            // Full height, corners included. These used to stop
            // `border_width` short of each end, so no point was ever in both
            // a side border and the top or bottom one, and no corner handle
            // could fire (the "#11 corner bug").
            left_border: Rect {
                x: window_geom.x,
                y: window_geom.y,
                width: self.config.border_width as i32,
                height: window_geom.height as i32,
            },
            right_border: Rect {
                x: window_geom.x + window_geom.width as i32 - self.config.border_width as i32,
                y: window_geom.y,
                width: self.config.border_width as i32,
                height: window_geom.height as i32,
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
        let border = self.config.border_width as i32;
        resize_handle_at(x, y, window_geom, border, border + self.config.corner_radius as i32)
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

/// Which resize handle `(x, y)` is on, for a frame whose outer rect is
/// `geom`. The grab band runs `border` deep along every edge. A corner
/// reaches `corner` along both of its edges, so it is a comfortable target
/// rather than a `border`-square sliver. (Pass `corner == border` for plain
/// band intersections.) Returns `None` inside the band or outside `geom`.
///
/// Evo's SSD frame calls this with a band that lies wholly outside the
/// visible window (shell/element.rs), so that grabbing an edge never steals
/// the client's own edge pixels, such as a scrollbar.
pub fn resize_handle_at(x: i32, y: i32, geom: &WindowGeometry, border: i32, corner: i32) -> Option<ResizeHandle> {
    let (left, top) = (geom.x, geom.y);
    let (right, bottom) = (left + geom.width as i32, top + geom.height as i32);
    if x < left || x >= right || y < top || y >= bottom {
        return None;
    }

    let near = |edge_distance: i32, depth: i32| edge_distance < depth;
    let (to_left, to_right, to_top, to_bottom) = (x - left, right - 1 - x, y - top, bottom - 1 - y);
    let in_band = near(to_left, border) || near(to_right, border) || near(to_top, border) || near(to_bottom, border);
    if !in_band {
        return None;
    }

    // Corners first, so a point near a corner is diagonal even when it's
    // only in one edge's band.
    let (cl, cr, ct, cb) = (
        near(to_left, corner),
        near(to_right, corner),
        near(to_top, corner),
        near(to_bottom, corner),
    );
    Some(match (ct, cb, cl, cr) {
        (true, _, true, _) => ResizeHandle::TopLeft,
        (true, _, _, true) => ResizeHandle::TopRight,
        (_, true, true, _) => ResizeHandle::BottomLeft,
        (_, true, _, true) => ResizeHandle::BottomRight,
        _ if near(to_top, border) => ResizeHandle::Top,
        _ if near(to_bottom, border) => ResizeHandle::Bottom,
        _ if near(to_left, border) => ResizeHandle::Left,
        _ => ResizeHandle::Right,
    })
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
    fn every_resize_handle_is_reachable() {
        // Band 4 deep, corners reaching 12 along each edge, on 800x600.
        let geom = WindowGeometry { x: 0, y: 0, width: 800, height: 600 };
        let at = |x, y| resize_handle_at(x, y, &geom, 4, 12);

        assert_eq!(at(1, 1), Some(ResizeHandle::TopLeft));
        assert_eq!(at(798, 2), Some(ResizeHandle::TopRight));
        assert_eq!(at(0, 599), Some(ResizeHandle::BottomLeft));
        assert_eq!(at(799, 599), Some(ResizeHandle::BottomRight));
        assert_eq!(at(400, 0), Some(ResizeHandle::Top));
        assert_eq!(at(400, 597), Some(ResizeHandle::Bottom));
        assert_eq!(at(3, 300), Some(ResizeHandle::Left));
        assert_eq!(at(796, 300), Some(ResizeHandle::Right));
    }

    #[test]
    fn corners_reach_along_the_edges() {
        let geom = WindowGeometry { x: 0, y: 0, width: 800, height: 600 };
        // In the top band only, but within 12 of the left edge: diagonal.
        assert_eq!(resize_handle_at(10, 1, &geom, 4, 12), Some(ResizeHandle::TopLeft));
        // Just past the corner's reach: back to a plain edge.
        assert_eq!(resize_handle_at(12, 1, &geom, 4, 12), Some(ResizeHandle::Top));
        // With corner == border, only the band intersection is a corner.
        assert_eq!(resize_handle_at(10, 1, &geom, 4, 4), Some(ResizeHandle::Top));
    }

    #[test]
    fn interior_and_outside_are_not_handles() {
        let geom = WindowGeometry { x: 100, y: 50, width: 800, height: 600 };
        assert_eq!(resize_handle_at(500, 300, &geom, 4, 12), None);
        assert_eq!(resize_handle_at(99, 300, &geom, 4, 12), None);
        assert_eq!(resize_handle_at(900, 300, &geom, 4, 12), None);
        assert_eq!(resize_handle_at(500, 650, &geom, 4, 12), None);
        // Offset geometry: the band follows the frame, not the origin.
        assert_eq!(resize_handle_at(100, 50, &geom, 4, 12), Some(ResizeHandle::TopLeft));
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
