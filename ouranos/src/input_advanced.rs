//! Advanced Input Management
//!
//! Comprehensive input handling for keyboard, pointer, touch, and tablets

use smithay::input::keyboard::{KeyboardHandle, KeysymHandle, ModifiersState};
use smithay::input::pointer::{PointerHandle, AxisFrame, ButtonEvent, MotionEvent};
use smithay::input::touch::TouchHandle;
use smithay::backend::input::{
    Event, InputEvent, KeyboardKeyEvent, PointerMotionEvent,
    PointerButtonEvent, PointerAxisEvent, TouchEvent,
};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use anyhow::Result;

/// Advanced input manager
pub struct AdvancedInputManager {
    // Keyboard state
    pub pressed_keys: HashSet<u32>,
    pub modifiers: ModifiersState,
    pub keyboard_repeat: KeyboardRepeat,
    
    // Pointer state
    pub pointer_position: (f64, f64),
    pub pointer_buttons: HashSet<u32>,
    pub pointer_accumulated_scroll: (f64, f64),
    
    // Touch state
    pub active_touches: HashMap<i32, TouchPoint>,
    pub touch_gestures: GestureRecognizer,
    
    // Tablet state
    pub tablet_tool: Option<TabletTool>,
    
    // Configuration
    pub pointer_acceleration: f64,
    pub scroll_factor: f64,
    pub long_press_threshold: Duration,
}

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

/// Touch point tracking
#[derive(Debug, Clone, Copy)]
pub struct TouchPoint {
    pub id: i32,
    pub x: f64,
    pub y: f64,
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
}

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
        
        // Simple swipe detection (would be more sophisticated in production)
        if touch.duration() > Duration::from_millis(100) {
            let dx = touch.x; // Would calculate from start position
            let dy = touch.y;
            
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

impl AdvancedInputManager {
    pub fn new() -> Self {
        AdvancedInputManager {
            pressed_keys: HashSet::new(),
            modifiers: ModifiersState::default(),
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
    pub fn process_keyboard<B: smithay::backend::input::InputBackend>(&mut self, event: &B::KeyboardKeyEvent) -> Result<KeyboardAction> {
        let key_code = event.key_code();
        let pressed = event.state() == smithay::backend::input::KeyState::Pressed;
        
        if pressed {
            self.pressed_keys.insert(key_code.raw());
            self.keyboard_repeat.key_pressed(key_code.raw());
            tracing::trace!("Key pressed: {}", key_code.raw());
            Ok(KeyboardAction::KeyPressed(key_code.raw()))
        } else {
            self.pressed_keys.remove(&key_code.raw());
            self.keyboard_repeat.key_released(key_code.raw());
            tracing::trace!("Key released: {}", key_code.raw());
            Ok(KeyboardAction::KeyReleased(key_code.raw()))
        }
    }
    
    /// Process pointer motion
    pub fn process_pointer_motion<B: smithay::backend::input::InputBackend>(&mut self, event: &B::PointerMotionEvent) -> Result<PointerAction> {
        let dx = event.delta_x() * self.pointer_acceleration;
        let dy = event.delta_y() * self.pointer_acceleration;
        
        self.pointer_position.0 += dx;
        self.pointer_position.1 += dy;
        
        // Clamp to screen bounds (would use actual screen size)
        self.pointer_position.0 = self.pointer_position.0.max(0.0).min(1920.0);
        self.pointer_position.1 = self.pointer_position.1.max(0.0).min(1080.0);
        
        tracing::trace!("Pointer motion: ({:.1}, {:.1})", self.pointer_position.0, self.pointer_position.1);
        
        Ok(PointerAction::Motion {
            x: self.pointer_position.0,
            y: self.pointer_position.1,
        })
    }
    
    /// Process pointer button
    pub fn process_pointer_button<B: smithay::backend::input::InputBackend>(&mut self, event: &B::PointerButtonEvent) -> Result<PointerAction> {
        let button = event.button_code();
        let pressed = event.state() == smithay::backend::input::ButtonState::Pressed;
        
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
    
    /// Process pointer axis (scroll)
    pub fn process_pointer_axis<B: smithay::backend::input::InputBackend>(&mut self, event: &B::PointerAxisEvent) -> Result<PointerAction> {
        let horizontal = event.amount(smithay::backend::input::Axis::Horizontal)
            .unwrap_or(0.0) * self.scroll_factor;
        let vertical = event.amount(smithay::backend::input::Axis::Vertical)
            .unwrap_or(0.0) * self.scroll_factor;
        
        self.pointer_accumulated_scroll.0 += horizontal;
        self.pointer_accumulated_scroll.1 += vertical;
        
        tracing::trace!("Pointer scroll: h={:.1}, v={:.1}", horizontal, vertical);
        
        Ok(PointerAction::Scroll { horizontal, vertical })
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
        if let Some(touch) = self.active_touches.remove(&id) {
            tracing::trace!("Touch up: id={}", id);
            
            // Check for tap gesture
            if self.touch_gestures.recognize_tap(&touch, touch.x, touch.y) {
                return Ok(TouchAction::Tap { id, x: touch.x, y: touch.y });
            }
            
            Ok(TouchAction::Up { id })
        } else {
            Ok(TouchAction::None)
        }
    }
    
    /// Check for keyboard shortcuts
    pub fn check_shortcut(&self, key: u32) -> Option<Shortcut> {
        let ctrl = self.modifiers.ctrl;
        let shift = self.modifiers.shift;
        let alt = self.modifiers.alt;
        let logo = self.modifiers.logo;
        
        // Common shortcuts (using evdev key codes)
        match (ctrl, shift, alt, logo, key) {
            (true, false, false, false, 24) => Some(Shortcut::Quit),        // Ctrl+Q
            (true, false, false, false, 46) => Some(Shortcut::Copy),        // Ctrl+C
            (true, false, false, false, 47) => Some(Shortcut::Paste),       // Ctrl+V
            (true, false, false, false, 45) => Some(Shortcut::Cut),         // Ctrl+X
            (true, false, false, false, 31) => Some(Shortcut::Save),        // Ctrl+S
            (false, false, false, true, 36) => Some(Shortcut::Terminal),    // Super+Enter
            (false, false, false, true, 32) => Some(Shortcut::AppLauncher), // Super+D
            _ => None,
        }
    }
    
    /// Get pointer position
    pub fn pointer_position(&self) -> (f64, f64) {
        self.pointer_position
    }
    
    /// Check if key is pressed
    pub fn is_key_pressed(&self, key: u32) -> bool {
        self.pressed_keys.contains(&key)
    }
    
    /// Check if pointer button is pressed
    pub fn is_button_pressed(&self, button: u32) -> bool {
        self.pointer_buttons.contains(&button)
    }
    
    /// Get active touch count
    pub fn active_touch_count(&self) -> usize {
        self.active_touches.len()
    }
}

/// Keyboard action result
#[derive(Debug, Clone, Copy)]
pub enum KeyboardAction {
    KeyPressed(u32),
    KeyReleased(u32),
    Repeat(u32),
}

/// Pointer action result
#[derive(Debug, Clone, Copy)]
pub enum PointerAction {
    Motion { x: f64, y: f64 },
    ButtonPressed(u32),
    ButtonReleased(u32),
    Scroll { horizontal: f64, vertical: f64 },
}

/// Touch action result
#[derive(Debug, Clone, Copy)]
pub enum TouchAction {
    Down { id: i32, x: f64, y: f64 },
    Motion { id: i32, x: f64, y: f64 },
    Up { id: i32 },
    Tap { id: i32, x: f64, y: f64 },
    LongPress { id: i32, x: f64, y: f64 },
    None,
}

/// Keyboard shortcuts
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    Quit,
    Copy,
    Paste,
    Cut,
    Save,
    Terminal,
    AppLauncher,
    SwitchWorkspace(u8),
    CloseWindow,
}
