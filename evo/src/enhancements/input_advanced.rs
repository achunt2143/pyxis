//! Advanced Input Management - FIXED VERSION
//!
//! Comprehensive input handling for keyboard, pointer, touch, and tablets

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use anyhow::Result;

// ============================================================================
// ACTION ENUMS - Define what each input action returns
// ============================================================================

/// Keyboard actions from input events
#[derive(Debug, Clone, Copy)]
pub enum KeyboardAction {
    KeyPressed(u32),
    KeyReleased(u32),
    KeyRepeat(u32),
}

/// Pointer (mouse) actions
#[derive(Debug, Clone)]
pub enum PointerAction {
    Motion { x: f64, y: f64 },
    ButtonPressed(u32),
    ButtonReleased(u32),
    Scroll { horizontal: f64, vertical: f64 },
}

/// Touch screen actions
#[derive(Debug, Clone)]
pub enum TouchAction {
    Down { id: i32, x: f64, y: f64 },
    Motion { id: i32, x: f64, y: f64 },
    Up { id: i32 },
    None,
}

/// Tablet tool actions
#[derive(Debug, Clone)]
pub enum TabletAction {
    ToolProximity { in_proximity: bool },
    MotionEvent { pressure: f64, distance: f64 },
    TiltEvent { x: f64, y: f64 },
}

// ============================================================================
// KEYBOARD REPEAT
// ============================================================================

/// Keyboard repeat configuration
#[derive(Debug, Clone)]
pub struct KeyboardRepeat {
    pub delay: Duration,
    pub rate: u32,
    pub last_key: Option<u32>,
    pub last_press: Option<Instant>,
}

impl KeyboardRepeat {
    pub fn new(delay_ms: u32, rate: u32) -> Self {
        KeyboardRepeat {
            delay: Duration::from_millis(delay_ms as u64),
            rate,
            last_key: None,
            last_press: None,
        }
    }

    pub fn key_pressed(&mut self, key: u32) {
        self.last_key = Some(key);
        self.last_press = Some(Instant::now());
    }

    pub fn key_released(&mut self, key: u32) {
        if self.last_key == Some(key) {
            self.last_key = None;
            self.last_press = None;
        }
    }

    pub fn should_repeat(&self) -> Option<u32> {
        if let (Some(key), Some(press_time)) = (self.last_key, self.last_press) {
            if press_time.elapsed() > self.delay {
                return Some(key);
            }
        }
        None
    }
}

// ============================================================================
// TOUCH POINT - Now with start position tracking!
// ============================================================================

/// Touch point tracking
#[derive(Debug, Clone, Copy)]
pub struct TouchPoint {
    pub id: i32,
    pub x: f64,
    pub y: f64,
    pub start_x: f64,
    pub start_y: f64,
    pub start_time: Instant,
    pub last_update: Instant,
}

impl TouchPoint {
    pub fn new(id: i32, x: f64, y: f64) -> Self {
        let now = Instant::now();
        TouchPoint {
            id,
            x,
            y,
            start_x: x,
            start_y: y,
            start_time: now,
            last_update: now,
        }
    }

    pub fn update_position(&mut self, x: f64, y: f64) {
        self.x = x;
        self.y = y;
        self.last_update = Instant::now();
    }

    pub fn duration(&self) -> Duration {
        self.start_time.elapsed()
    }

    pub fn distance_from_start(&self, start_x: f64, start_y: f64) -> f64 {
        let dx = self.x - start_x;
        let dy = self.y - start_y;
        (dx * dx + dy * dy).sqrt()
    }

    /// Get delta from start position (for swipe detection)
    pub fn delta_from_start(&self) -> (f64, f64) {
        (self.x - self.start_x, self.y - self.start_y)
    }
}

// ============================================================================
// GESTURE RECOGNIZER
// ============================================================================

/// Gesture recognizer
#[derive(Debug, Clone)]
pub struct GestureRecognizer {
    pub swipe_threshold: f64,
    pub pinch_threshold: f64,
    pub tap_max_duration: Duration,
    pub tap_max_distance: f64,
}

impl GestureRecognizer {
    pub fn new() -> Self {
        GestureRecognizer {
            swipe_threshold: 100.0,
            pinch_threshold: 50.0,
            tap_max_duration: Duration::from_millis(200),
            tap_max_distance: 20.0,
        }
    }

    pub fn recognize_tap(&self, touch: &TouchPoint, start_x: f64, start_y: f64) -> bool {
        touch.duration() < self.tap_max_duration
            && touch.distance_from_start(start_x, start_y) < self.tap_max_distance
    }

    pub fn recognize_swipe(&self, touches: &HashMap<i32, TouchPoint>) -> Option<SwipeGesture> {
        if touches.len() != 1 {
            return None;
        }

        let touch = touches.values().next()?;

        let (dx, dy) = touch.delta_from_start();

        if dx.abs() > self.swipe_threshold {
            return Some(if dx > 0.0 {
                SwipeGesture::Right
            } else {
                SwipeGesture::Left
            });
        }

        if dy.abs() > self.swipe_threshold {
            return Some(if dy > 0.0 {
                SwipeGesture::Down
            } else {
                SwipeGesture::Up
            });
        }

        None
    }
}

/// Swipe gesture direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeGesture {
    Up,
    Down,
    Left,
    Right,
}

// ============================================================================
// TABLET TOOL
// ============================================================================

/// Tablet tool information
#[derive(Debug, Clone)]
pub struct TabletTool {
    pub tool_type: TabletToolType,
    pub serial: u32,
    pub pressure: f64,
    pub distance: f64,
    pub tilt_x: f64,
    pub tilt_y: f64,
    pub rotation: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabletToolType {
    Pen,
    Eraser,
    Brush,
    Pencil,
    Airbrush,
    Mouse,
    Lens,
}

// ============================================================================
// ADVANCED INPUT MANAGER
// ============================================================================

/// Advanced input manager
pub struct AdvancedInputManager {
    pub pressed_keys: HashSet<u32>,
    pub keyboard_repeat: KeyboardRepeat,
    pub pointer_position: (f64, f64),
    pub pointer_buttons: HashSet<u32>,
    pub pointer_accumulated_scroll: (f64, f64),
    pub active_touches: HashMap<i32, TouchPoint>,
    pub touch_gestures: GestureRecognizer,
    pub tablet_tool: Option<TabletTool>,
    pub pointer_acceleration: f64,
    pub scroll_factor: f64,
    pub long_press_threshold: Duration,
}

impl AdvancedInputManager {
    pub fn new() -> Self {
        AdvancedInputManager {
            pressed_keys: HashSet::new(),
            keyboard_repeat: KeyboardRepeat::new(500, 30),
            pointer_position: (0.0, 0.0),
            pointer_buttons: HashSet::new(),
            pointer_accumulated_scroll: (0.0, 0.0),
            active_touches: HashMap::new(),
            touch_gestures: GestureRecognizer::new(),
            tablet_tool: None,
            pointer_acceleration: 1.0,
            scroll_factor: 1.0,
            long_press_threshold: Duration::from_millis(500),
        }
    }

    /// Process keyboard event
    pub fn process_keyboard(&mut self, key: u32, pressed: bool) -> Result<KeyboardAction> {
        if pressed {
            self.pressed_keys.insert(key);
            self.keyboard_repeat.key_pressed(key);
            tracing::trace!("Key pressed: {}", key);
            Ok(KeyboardAction::KeyPressed(key))
        } else {
            self.pressed_keys.remove(&key);
            self.keyboard_repeat.key_released(key);
            tracing::trace!("Key released: {}", key);
            Ok(KeyboardAction::KeyReleased(key))
        }
    }

    /// Process pointer motion
    pub fn process_pointer_motion(&mut self, dx: f64, dy: f64) -> Result<PointerAction> {
        let new_dx = dx * self.pointer_acceleration;
        let new_dy = dy * self.pointer_acceleration;
        self.pointer_position.0 += new_dx;
        self.pointer_position.1 += new_dy;

        self.pointer_position.0 = self.pointer_position.0.max(0.0).min(1920.0);
        self.pointer_position.1 = self.pointer_position.1.max(0.0).min(1080.0);

        tracing::trace!(
            "Pointer motion: ({:.1}, {:.1})",
            self.pointer_position.0,
            self.pointer_position.1
        );

        Ok(PointerAction::Motion {
            x: self.pointer_position.0,
            y: self.pointer_position.1,
        })
    }

    /// Process pointer button
    pub fn process_pointer_button(&mut self, button: u32, pressed: bool) -> Result<PointerAction> {
        if pressed {
            self.pointer_buttons.insert(button);
            tracing::trace!("Pointer button pressed: {}", button);
            Ok(PointerAction::ButtonPressed(button))
        } else {
            self.pointer_buttons.remove(&button);
            tracing::trace!("Pointer button released: {}", button);
            Ok(PointerAction::ButtonReleased(button))
        }
    }

    /// Process pointer scroll
    pub fn process_pointer_scroll(
        &mut self,
        horizontal: f64,
        vertical: f64,
    ) -> Result<PointerAction> {
        let h_scroll = horizontal * self.scroll_factor;
        let v_scroll = vertical * self.scroll_factor;

        self.pointer_accumulated_scroll.0 += h_scroll;
        self.pointer_accumulated_scroll.1 += v_scroll;

        tracing::trace!("Pointer scroll: h={:.1}, v={:.1}", h_scroll, v_scroll);

        Ok(PointerAction::Scroll {
            horizontal: h_scroll,
            vertical: v_scroll,
        })
    }

    /// Process touch down
    pub fn process_touch_down(&mut self, id: i32, x: f64, y: f64) -> Result<TouchAction> {
        let touch = TouchPoint::new(id, x, y);
        self.active_touches.insert(id, touch);
        tracing::trace!("Touch down: id={}, ({:.1}, {:.1})", id, x, y);
        Ok(TouchAction::Down { id, x, y })
    }

    /// Process touch motion
    pub fn process_touch_motion(&mut self, id: i32, x: f64, y: f64) -> Result<TouchAction> {
        if let Some(touch) = self.active_touches.get_mut(&id) {
            touch.update_position(x, y);
            tracing::trace!("Touch motion: id={}, ({:.1}, {:.1})", id, x, y);
            Ok(TouchAction::Motion { id, x, y })
        } else {
            Ok(TouchAction::None)
        }
    }

    /// Process touch up
    pub fn process_touch_up(&mut self, id: i32) -> Result<TouchAction> {
        if let Some(_touch) = self.active_touches.remove(&id) {
            tracing::trace!("Touch up: id={}", id);
            Ok(TouchAction::Up { id })
        } else {
            Ok(TouchAction::None)
        }
    }

    /// Get current keyboard repeat key if any
    pub fn get_keyboard_repeat(&self) -> Option<u32> {
        self.keyboard_repeat.should_repeat()
    }

    /// Check if a key is currently pressed
    pub fn is_key_pressed(&self, key: u32) -> bool {
        self.pressed_keys.contains(&key)
    }

    /// Check if a pointer button is pressed
    pub fn is_button_pressed(&self, button: u32) -> bool {
        self.pointer_buttons.contains(&button)
    }

    /// Get current pointer position
    pub fn pointer_position(&self) -> (f64, f64) {
        self.pointer_position
    }

    /// Get active touch points
    pub fn active_touches(&self) -> &HashMap<i32, TouchPoint> {
        &self.active_touches
    }

    /// Recognize gestures in active touches
    pub fn recognize_gesture(&self) -> Option<SwipeGesture> {
        self.touch_gestures.recognize_swipe(&self.active_touches)
    }
}

impl Default for AdvancedInputManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keyboard_press_release() {
        let mut manager = AdvancedInputManager::new();
        
        let action = manager.process_keyboard(42, true).unwrap();
        assert!(matches!(action, KeyboardAction::KeyPressed(42)));
        assert!(manager.is_key_pressed(42));
        
        let action = manager.process_keyboard(42, false).unwrap();
        assert!(matches!(action, KeyboardAction::KeyReleased(42)));
        assert!(!manager.is_key_pressed(42));
    }

    #[test]
    fn test_touch_start_position() {
        let mut manager = AdvancedInputManager::new();
        
        manager.process_touch_down(1, 100.0, 100.0).unwrap();
        manager.process_touch_motion(1, 200.0, 200.0).unwrap();
        
        let touch = manager.active_touches.get(&1).unwrap();
        let (dx, dy) = touch.delta_from_start();
        
        assert_eq!(dx, 100.0);
        assert_eq!(dy, 100.0);
        assert_eq!(touch.start_x, 100.0);
        assert_eq!(touch.start_y, 100.0);
    }

    #[test]
    fn test_swipe_detection() {
        let manager = AdvancedInputManager::new();
        let gesture = &manager.touch_gestures;
        
        let mut touch = TouchPoint::new(1, 0.0, 0.0);
        touch.x = 150.0;
        
        let mut touches = HashMap::new();
        touches.insert(1, touch);
        
        let swipe = gesture.recognize_swipe(&touches);
        assert_eq!(swipe, Some(SwipeGesture::Right));
    }
}