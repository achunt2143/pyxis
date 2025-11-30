//! Advanced Input Handling
//!
//! Extends basic input handling with gesture recognition, IME support,
//! multi-touch tracking, and shortcut bindings

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Gesture types for touch input
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GestureType {
    Swipe,
    Pinch,
    LongPress,
    Rotate,
    DoubleTap,
}

/// Direction of swipe gesture
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Gesture event
#[derive(Debug, Clone)]
pub struct GestureEvent {
    pub gesture_type: GestureType,
    pub position: (f64, f64),
    pub delta: Option<(f64, f64)>,
    pub scale: Option<f64>,
    pub rotation: Option<f64>,
    pub direction: Option<SwipeDirection>,
    pub timestamp: Instant,
}

/// Multi-touch tracker for gesture recognition
pub struct TouchTracker {
    active_touches: HashMap<u32, TouchPoint>,
    gesture_history: VecDeque<GestureEvent>,
    last_tap_time: Option<Instant>,
    long_press_threshold: Duration,
    swipe_min_distance: f64,
    pinch_threshold: f64,
}

#[derive(Debug, Clone)]
struct TouchPoint {
    id: u32,
    x: f64,
    y: f64,
    pressure: f64,
    start_time: Instant,
}

impl TouchTracker {
    pub fn new() -> Self {
        TouchTracker {
            active_touches: HashMap::new(),
            gesture_history: VecDeque::with_capacity(100),
            last_tap_time: None,
            long_press_threshold: Duration::from_millis(500),
            swipe_min_distance: 50.0,
            pinch_threshold: 0.1,
        }
    }

    /// Track touch down event
    pub fn touch_down(&mut self, touch_id: u32, x: f64, y: f64, pressure: f64) {
        let touch = TouchPoint {
            id: touch_id,
            x,
            y,
            pressure,
            start_time: Instant::now(),
        };
        self.active_touches.insert(touch_id, touch);
        tracing::debug!("Touch down: id={}, x={:.2}, y={:.2}, pressure={:.2}", touch_id, x, y, pressure);
    }

    /// Track touch motion event
    pub fn touch_motion(&mut self, touch_id: u32, x: f64, y: f64, pressure: f64) {
        if let Some(touch) = self.active_touches.get_mut(&touch_id) {
            touch.x = x;
            touch.y = y;
            touch.pressure = pressure;
        }
    }

    /// Track touch up event
    pub fn touch_up(&mut self, touch_id: u32) -> Option<GestureEvent> {
        if let Some(touch) = self.active_touches.remove(&touch_id) {
            let duration = touch.start_time.elapsed();
            
            // Check for long press
            if duration > self.long_press_threshold {
                let event = GestureEvent {
                    gesture_type: GestureType::LongPress,
                    position: (touch.x, touch.y),
                    delta: None,
                    scale: None,
                    rotation: None,
                    direction: None,
                    timestamp: Instant::now(),
                };
                self.gesture_history.push_back(event.clone());
                return Some(event);
            }
            
            // Check for double tap
            if let Some(last_time) = self.last_tap_time {
                if last_time.elapsed() < Duration::from_millis(300) {
                    let event = GestureEvent {
                        gesture_type: GestureType::DoubleTap,
                        position: (touch.x, touch.y),
                        delta: None,
                        scale: None,
                        rotation: None,
                        direction: None,
                        timestamp: Instant::now(),
                    };
                    self.gesture_history.push_back(event.clone());
                    self.last_tap_time = None;
                    return Some(event);
                }
            }
            self.last_tap_time = Some(Instant::now());
            
            tracing::debug!("Touch up: id={}, duration={:?}", touch_id, duration);
        }
        None
    }

    /// Detect swipe gesture from multiple touches
    pub fn detect_swipe(&self) -> Option<GestureEvent> {
        if self.active_touches.len() < 1 {
            return None;
        }

        // Get first touch point
        if let Some(touch) = self.active_touches.values().next() {
            // Simple swipe detection based on movement
            let dx = 0.0; // Would track from start position
            let dy = 0.0;
            
            let distance = (dx * dx + dy * dy).sqrt();
            if distance > self.swipe_min_distance {
                let direction = if dx.abs() > dy.abs() {
                    if dx > 0.0 { SwipeDirection::Right } else { SwipeDirection::Left }
                } else {
                    if dy > 0.0 { SwipeDirection::Down } else { SwipeDirection::Up }
                };
                
                let event = GestureEvent {
                    gesture_type: GestureType::Swipe,
                    position: (touch.x, touch.y),
                    delta: Some((dx, dy)),
                    scale: None,
                    rotation: None,
                    direction: Some(direction),
                    timestamp: Instant::now(),
                };
                return Some(event);
            }
        }
        None
    }

    /// Detect pinch gesture (scale)
    pub fn detect_pinch(&self) -> Option<f64> {
        if self.active_touches.len() < 2 {
            return None;
        }

        let touches: Vec<_> = self.active_touches.values().collect();
        if touches.len() >= 2 {
            let t1 = touches[0];
            let t2 = touches[1];
            
            let dx = t2.x - t1.x;
            let dy = t2.y - t1.y;
            let distance = (dx * dx + dy * dy).sqrt();
            
            // Normalized scale (1.0 = no scale, >1.0 = zoom in, <1.0 = zoom out)
            if distance > 0.0 {
                return Some(distance / 100.0); // Normalized to ~100px baseline
            }
        }
        None
    }

    /// Get active touch count
    pub fn touch_count(&self) -> usize {
        self.active_touches.len()
    }

    /// Clear all touches
    pub fn clear(&mut self) {
        self.active_touches.clear();
    }
}

/// Input Method Engine integration
pub struct ImeState {
    pub enabled: bool,
    pub preedit_text: String,
    pub cursor_position: usize,
    pub surrounding_text: String,
    pub surrounding_cursor: usize,
}

impl Default for ImeState {
    fn default() -> Self {
        ImeState {
            enabled: false,
            preedit_text: String::new(),
            cursor_position: 0,
            surrounding_text: String::new(),
            surrounding_cursor: 0,
        }
    }
}

impl ImeState {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn update_preedit(&mut self, text: String, cursor: usize) {
        self.preedit_text = text;
        self.cursor_position = cursor;
        tracing::trace!("Preedit updated: '{}' cursor={}", self.preedit_text, cursor);
    }

    pub fn commit(&mut self, text: String) {
        self.surrounding_text.push_str(&text);
        tracing::debug!("IME commit: '{}', surrounding='{}'", text, self.surrounding_text);
    }

    pub fn set_surrounding_text(&mut self, text: String, cursor: usize) {
        self.surrounding_text = text;
        self.surrounding_cursor = cursor;
    }
}

/// Keyboard layout manager
pub struct KeyboardLayout {
    pub current_layout: String,
    pub available_layouts: Vec<String>,
    pub variant: Option<String>,
}

impl KeyboardLayout {
    pub fn new() -> Self {
        KeyboardLayout {
            current_layout: "us".to_string(),
            available_layouts: vec!["us".to_string(), "fr".to_string(), "de".to_string()],
            variant: None,
        }
    }

    pub fn switch_layout(&mut self, layout: &str) -> bool {
        if self.available_layouts.contains(&layout.to_string()) {
            self.current_layout = layout.to_string();
            tracing::info!("Keyboard layout switched to: {}", layout);
            true
        } else {
            false
        }
    }

    pub fn next_layout(&mut self) {
        let current_idx = self.available_layouts.iter()
            .position(|l| l == &self.current_layout)
            .unwrap_or(0);
        let next_idx = (current_idx + 1) % self.available_layouts.len();
        self.current_layout = self.available_layouts[next_idx].clone();
        tracing::info!("Switched to layout: {}", self.current_layout);
    }
}

/// Text input protocol support
pub struct TextInputState {
    pub enabled: bool,
    pub surrounding_text: String,
    pub cursor_position: usize,
    pub anchor_position: Option<usize>,
    pub content_type: ContentType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Text,
    Number,
    Email,
    Password,
    Date,
    Time,
}

impl TextInputState {
    pub fn new() -> Self {
        TextInputState {
            enabled: false,
            surrounding_text: String::new(),
            cursor_position: 0,
            anchor_position: None,
            content_type: ContentType::Text,
        }
    }

    pub fn set_surrounding_text(&mut self, text: String, cursor: usize) {
        self.surrounding_text = text;
        self.cursor_position = cursor;
        tracing::trace!("Text input surrounding updated: cursor={}", cursor);
    }

    pub fn set_selection(&mut self, cursor: usize, anchor: usize) {
        self.cursor_position = cursor;
        self.anchor_position = Some(anchor);
    }
}

/// Shortcut binding system
pub struct ShortcutBinder {
    bindings: HashMap<(u32, u32), String>, // (key_code, modifier_mask) -> action
}

impl ShortcutBinder {
    pub fn new() -> Self {
        ShortcutBinder {
            bindings: HashMap::new(),
        }
    }

    /// Bind a key combination to an action
    pub fn bind(&mut self, key_code: u32, modifiers: u32, action: String) {
        self.bindings.insert((key_code, modifiers), action);
        tracing::debug!("Shortcut bound: key={}, mods={:08x} -> {}", key_code, modifiers, action);
    }

    /// Lookup action for key combination
    pub fn lookup(&self, key_code: u32, modifiers: u32) -> Option<&str> {
        self.bindings.get(&(key_code, modifiers)).map(|s| s.as_str())
    }

    /// Register common system shortcuts
    pub fn register_defaults(&mut self) {
        // Alt+Tab: Window switcher
        self.bind(23, 0x04, "window_switcher".to_string());
        
        // Super+D: Show desktop
        self.bind(50, 0x08, "show_desktop".to_string());
        
        // Super+L: Lock screen
        self.bind(38, 0x08, "lock_screen".to_string());
        
        // Ctrl+Alt+T: Terminal
        self.bind(28, 0x06, "open_terminal".to_string());
        
        // Super+Space: App launcher
        self.bind(65, 0x08, "app_launcher".to_string());
        
        tracing::info!("Default shortcuts registered");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_touch_tracker_creation() {
        let tracker = TouchTracker::new();
        assert_eq!(tracker.touch_count(), 0);
    }

    #[test]
    fn test_touch_tracking() {
        let mut tracker = TouchTracker::new();
        tracker.touch_down(1, 100.0, 200.0, 0.8);
        assert_eq!(tracker.touch_count(), 1);
        
        tracker.touch_motion(1, 105.0, 205.0, 0.8);
        assert_eq!(tracker.touch_count(), 1);
        
        tracker.touch_up(1);
        assert_eq!(tracker.touch_count(), 0);
    }

    #[test]
    fn test_keyboard_layout() {
        let mut layout = KeyboardLayout::new();
        assert_eq!(layout.current_layout, "us");
        
        layout.switch_layout("fr");
        assert_eq!(layout.current_layout, "fr");
        
        layout.next_layout();
        assert_eq!(layout.current_layout, "de");
    }

    #[test]
    fn test_shortcut_binder() {
        let mut binder = ShortcutBinder::new();
        binder.bind(23, 0x04, "test_action".to_string());
        
        assert_eq!(binder.lookup(23, 0x04), Some("test_action"));
        assert_eq!(binder.lookup(24, 0x04), None);
    }

    #[test]
    fn test_ime_state() {
        let mut ime = ImeState::new();
        ime.update_preedit("hello".to_string(), 3);
        assert_eq!(ime.preedit_text, "hello");
        assert_eq!(ime.cursor_position, 3);
    }
}
