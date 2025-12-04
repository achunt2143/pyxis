// input_events_router - Complete Window Manager Implementation

use std::collections::{HashMap, VecDeque};
use parking_lot::RwLock;
use std::sync::Arc;
use tracing::{debug, warn, trace};
use serde::{Deserialize, Serialize};

/// Input event types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InputEvent {
    Pointer {
        x: i32,
        y: i32,
        button: MouseButton,
        state: ButtonState,
    },
    Keyboard {
        key_code: u32,
        state: KeyState,
        modifiers: KeyModifiers,
    },
    Touch {
        touch_id: u32,
        x: i32,
        y: i32,
        state: TouchState,
        pressure: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum KeyState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KeyModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TouchState {
    Start,
    Move,
    End,
    Cancel,
}

/// Window geometry and state tracking
#[derive(Debug, Clone)]
pub struct WindowGeometry {
    pub window_id: u32,
    pub app_id: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_visible: bool,
    pub z_order: i32,
    pub is_focused: bool,
    pub is_grabbed: bool,
}

impl WindowGeometry {
    /// Check if point is inside this window
    pub fn contains_point(&self, px: i32, py: i32) -> bool {
        if !self.is_visible {
            return false;
        }
        px >= self.x && px < (self.x + self.width as i32) &&
        py >= self.y && py < (self.y + self.height as i32)
    }

    /// Get window bounds
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        (self.x, self.y, self.x + self.width as i32, self.y + self.height as i32)
    }
}

/// Touch tracking for multi-touch support
#[derive(Debug, Clone)]
struct TouchTrack {
    touch_id: u32,
    window_id: u32,
    start_x: i32,
    start_y: i32,
    current_x: i32,
    current_y: i32,
}

/// Event queue for replay and debugging
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub timestamp: u64,
    pub event: InputEvent,
    pub target_window_id: u32,
    pub was_consumed: bool,
}

/// Full-featured input event router with window management
pub struct InputEventRouter {
    /// Window geometry and state (window_id -> WindowGeometry)
    windows: Arc<RwLock<HashMap<u32, WindowGeometry>>>,

    /// Window to app mapping (window_id -> app_id)
    window_to_app: Arc<RwLock<HashMap<u32, String>>>,

    /// Focus stack (most recent first)
    focus_stack: Arc<RwLock<Vec<u32>>>,

    /// Pointer grab state (which window has grabbed pointer)
    pointer_grab: Arc<RwLock<Option<u32>>>,

    /// Keyboard grab state (which window has grabbed keyboard)
    keyboard_grab: Arc<RwLock<Option<u32>>>,

    /// Active touch tracking (touch_id -> TouchTrack)
    active_touches: Arc<RwLock<HashMap<u32, TouchTrack>>>,

    /// Last pointer position
    last_pointer_x: Arc<RwLock<i32>>,
    last_pointer_y: Arc<RwLock<i32>>,

    /// Event history for debugging/replay
    event_history: Arc<RwLock<VecDeque<EventRecord>>>,
    max_history_size: usize,

    /// Display dimensions for boundary checking
    display_width: u32,
    display_height: u32,

    /// Modal window (if set, only this window receives events)
    modal_window: Arc<RwLock<Option<u32>>>,

    /// Pointer over window (for hover effects)
    pointer_over: Arc<RwLock<Option<u32>>>,
}

impl InputEventRouter {
    /// Create new input router with display dimensions
    pub fn new(display_width: u32, display_height: u32) -> Self {
        InputEventRouter {
            windows: Arc::new(RwLock::new(HashMap::new())),
            window_to_app: Arc::new(RwLock::new(HashMap::new())),
            focus_stack: Arc::new(RwLock::new(Vec::new())),
            pointer_grab: Arc::new(RwLock::new(None)),
            keyboard_grab: Arc::new(RwLock::new(None)),
            active_touches: Arc::new(RwLock::new(HashMap::new())),
            last_pointer_x: Arc::new(RwLock::new(0)),
            last_pointer_y: Arc::new(RwLock::new(0)),
            event_history: Arc::new(RwLock::new(VecDeque::new())),
            max_history_size: 1000,
            display_width,
            display_height,
            modal_window: Arc::new(RwLock::new(None)),
            pointer_over: Arc::new(RwLock::new(None)),
        }
    }

    /// Register a window in the manager
    pub fn register_window(&self, geometry: WindowGeometry) {
        let mut windows = self.windows.write();
        let mut window_to_app = self.window_to_app.write();

        windows.insert(geometry.window_id, geometry.clone());
        window_to_app.insert(geometry.window_id, geometry.app_id.clone());

        debug!("Registered window: id={}, app={}, pos=({},{}), size={}x{}",
            geometry.window_id, geometry.app_id, geometry.x, geometry.y,
            geometry.width, geometry.height);
    }

    /// Unregister a window
    pub fn unregister_window(&self, window_id: u32) -> bool {
        let mut windows = self.windows.write();
        let mut window_to_app = self.window_to_app.write();
        let mut focus_stack = self.focus_stack.write();
        let mut pointer_grab = self.pointer_grab.write();
        let mut keyboard_grab = self.keyboard_grab.write();

        // Remove from all tracking
        window_to_app.remove(&window_id);
        focus_stack.retain(|&wid| wid != window_id);

        if *pointer_grab == Some(window_id) {
            *pointer_grab = None;
        }
        if *keyboard_grab == Some(window_id) {
            *keyboard_grab = None;
        }

        let removed = windows.remove(&window_id).is_some();
        if removed {
            debug!("Unregistered window: {}", window_id);
        }
        removed
    }

    /// Update window geometry
    pub fn update_window_geometry(&self, window_id: u32, x: i32, y: i32, width: u32, height: u32) -> bool {
        let mut windows = self.windows.write();

        if let Some(window) = windows.get_mut(&window_id) {
            window.x = x;
            window.y = y;
            window.width = width;
            window.height = height;
            debug!("Updated window {}: pos=({},{}), size={}x{}", window_id, x, y, width, height);
            true
        } else {
            warn!("Window {} not found", window_id);
            false
        }
    }

    /// Set window visibility
    pub fn set_window_visible(&self, window_id: u32, visible: bool) -> bool {
        let mut windows = self.windows.write();

        if let Some(window) = windows.get_mut(&window_id) {
            window.is_visible = visible;
            debug!("Window {} visibility: {}", window_id, visible);
            true
        } else {
            false
        }
    }

    /// Raise window to front (bring to focus)
    pub fn raise_window(&self, window_id: u32) -> bool {
        let mut windows = self.windows.write();
        let mut focus_stack = self.focus_stack.write();

        if !windows.contains_key(&window_id) {
            return false;
        }

        // Remove from focus stack if present
        focus_stack.retain(|&wid| wid != window_id);

        // Add to front
        focus_stack.insert(0, window_id);

        // Update z-order
        for (i, &wid) in focus_stack.iter().enumerate() {
            if let Some(window) = windows.get_mut(&wid) {
                window.z_order = (focus_stack.len() - i) as i32;
                window.is_focused = i == 0;
            }
        }

        debug!("Raised window to front: {}", window_id);
        true
    }

    /// Lower window to back
    pub fn lower_window(&self, window_id: u32) -> bool {
        let mut windows = self.windows.write();
        let mut focus_stack = self.focus_stack.write();

        if !windows.contains_key(&window_id) {
            return false;
        }

        // Remove from focus stack
        focus_stack.retain(|&wid| wid != window_id);

        // Add to back
        focus_stack.push(window_id);

        // Update z-order and focus
        for (i, &wid) in focus_stack.iter().enumerate() {
            if let Some(window) = windows.get_mut(&wid) {
                window.z_order = (focus_stack.len() - i) as i32;
                window.is_focused = i == 0;
            }
        }

        debug!("Lowered window: {}", window_id);
        true
    }

    /// Get focused window
    pub fn get_focused_window(&self) -> Option<u32> {
        self.focus_stack.read().first().copied()
    }

    /// Set pointer grab (window captures all pointer events)
    pub fn set_pointer_grab(&self, window_id: Option<u32>) -> bool {
        if let Some(wid) = window_id {
            if !self.windows.read().contains_key(&wid) {
                return false;
            }
        }

        *self.pointer_grab.write() = window_id;
        debug!("Pointer grab: {:?}", window_id);
        true
    }

    /// Set keyboard grab (window captures all keyboard events)
    pub fn set_keyboard_grab(&self, window_id: Option<u32>) -> bool {
        if let Some(wid) = window_id {
            if !self.windows.read().contains_key(&wid) {
                return false;
            }
        }

        *self.keyboard_grab.write() = window_id;
        debug!("Keyboard grab: {:?}", window_id);
        true
    }

    /// Set modal window (only this window receives events)
    pub fn set_modal_window(&self, window_id: Option<u32>) -> bool {
        if let Some(wid) = window_id {
            if !self.windows.read().contains_key(&wid) {
                return false;
            }
        }

        *self.modal_window.write() = window_id;
        debug!("Modal window: {:?}", window_id);
        true
    }

    /// Route event to target window(s)
    ///
    /// Returns list of window IDs that should receive the event
    pub fn route_event(&self, event: &InputEvent) -> Vec<u32> {
        // Check modal first - if set, only modal window gets events
        if let Some(modal_id) = *self.modal_window.read() {
            debug!("Modal window active: {}", modal_id);
            return vec![modal_id];
        }

        debug!("Routing event: {:?}", event);

        match event {
            InputEvent::Pointer { x, y, button, state } => {
                self.route_pointer_event(*x, *y, *button, *state)
            }
            InputEvent::Keyboard { key_code, state, modifiers } => {
                self.route_keyboard_event(*key_code, *state, *modifiers)
            }
            InputEvent::Touch { touch_id, x, y, state, pressure } => {
                self.route_touch_event(*touch_id, *x, *y, *state, *pressure)
            }
        }
    }

    /// Route pointer event with hit testing
    fn route_pointer_event(&self, x: i32, y: i32, button: MouseButton, state: ButtonState) -> Vec<u32> {
        let mut last_x = self.last_pointer_x.write();
        let mut last_y = self.last_pointer_y.write();
        *last_x = x;
        *last_y = y;

        debug!("Pointer event at ({}, {}), button: {:?}, state: {:?}", x, y, button, state);

        // Check for pointer grab first
        if let Some(grab_id) = *self.pointer_grab.read() {
            debug!("Pointer grabbed by window: {}", grab_id);
            return vec![grab_id];
        }

        // Hit test from top window down
        let windows = self.windows.read();
        let binding = windows.clone();
        let mut window_list: Vec<_> = binding.values().collect();

        // Sort by z-order (highest first)
        window_list.sort_by(|a, b| b.z_order.cmp(&a.z_order));

        for window in window_list {
            if window.contains_point(x, y) {
                debug!("Pointer hit window: {} at ({},{})", window.window_id, window.x, window.y);

                // Update pointer over
                if state == ButtonState::Pressed {
                    *self.pointer_over.write() = Some(window.window_id);
                    // Auto-raise on click
                    drop(windows);
                    self.raise_window(window.window_id);
                }

                return vec![window.window_id];
            }
        }

        // No hit - clear pointer over
        *self.pointer_over.write() = None;
        debug!("Pointer event outside all windows");
        vec![]
    }

    /// Route keyboard event to focused window
    fn route_keyboard_event(&self, key_code: u32, state: KeyState, modifiers: KeyModifiers) -> Vec<u32> {
        debug!("Keyboard event: key={}, state: {:?}, modifiers: {:?}", key_code, state, modifiers);

        // Check for keyboard grab first
        if let Some(grab_id) = *self.keyboard_grab.read() {
            debug!("Keyboard grabbed by window: {}", grab_id);
            return vec![grab_id];
        }

        // Route to focused window
        if let Some(focused_id) = self.get_focused_window() {
            debug!("Keyboard event routed to focused window: {}", focused_id);
            vec![focused_id]
        } else {
            debug!("No focused window for keyboard event");
            vec![]
        }
    }

    /// Route touch event with multi-touch support
    fn route_touch_event(&self, touch_id: u32, x: i32, y: i32, state: TouchState, pressure: f32) -> Vec<u32> {
        debug!("Touch event {} at ({}, {}), state: {:?}, pressure: {}", touch_id, x, y, state, pressure);

        let mut active_touches = self.active_touches.write();

        match state {
            TouchState::Start => {
                // Hit test to find target window
                let windows = self.windows.read();
                let mut window_list: Vec<_> = windows.values().collect();

                // Sort by z-order (highest first)
                window_list.sort_by(|a, b| b.z_order.cmp(&a.z_order));

                for window in window_list {
                    if window.contains_point(x, y) {
                        debug!("Touch {} hit window: {}", touch_id, window.window_id);

                        active_touches.insert(touch_id, TouchTrack {
                            touch_id,
                            window_id: window.window_id,
                            start_x: x,
                            start_y: y,
                            current_x: x,
                            current_y: y,
                        });

                        return vec![window.window_id];
                    }
                }

                debug!("Touch {} outside all windows", touch_id);
                vec![]
            }
            TouchState::Move => {
                // Route to the window that started this touch
                if let Some(track) = active_touches.get_mut(&touch_id) {
                    track.current_x = x;
                    track.current_y = y;
                    debug!("Touch {} move in window: {}", touch_id, track.window_id);
                    vec![track.window_id]
                } else {
                    debug!("Touch {} move without start", touch_id);
                    vec![]
                }
            }
            TouchState::End => {
                // Route to same window that received start
                if let Some(track) = active_touches.remove(&touch_id) {
                    debug!("Touch {} end in window: {}", touch_id, track.window_id);
                    vec![track.window_id]
                } else {
                    vec![]
                }
            }
            TouchState::Cancel => {
                // Clean up touch tracking
                if active_touches.remove(&touch_id).is_some() {
                    debug!("Touch {} cancelled", touch_id);
                }
                vec![]
            }
        }
    }

    /// Record event in history for debugging
    pub fn record_event(&self, event: &InputEvent, target_window_id: u32, was_consumed: bool) {
        let record = EventRecord {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            event: event.clone(),
            target_window_id,
            was_consumed,
        };

        let mut history = self.event_history.write();
        history.push_back(record);

        // Keep history bounded
        while history.len() > self.max_history_size {
            history.pop_front();
        }
    }

    /// Get event history
    pub fn get_event_history(&self, limit: usize) -> Vec<EventRecord> {
        self.event_history.read()
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    /// Clear event history
    pub fn clear_event_history(&self) {
        self.event_history.write().clear();
    }

    /// Get all windows (sorted by z-order)
    pub fn get_windows(&self) -> Vec<WindowGeometry> {
        let windows = self.windows.read();
        let mut window_list: Vec<_> = windows.values().cloned().collect();

        // Sort by z-order (highest first)
        window_list.sort_by(|a, b| b.z_order.cmp(&a.z_order));
        window_list
    }

    /// Get window by ID
    pub fn get_window(&self, window_id: u32) -> Option<WindowGeometry> {
        self.windows.read().get(&window_id).cloned()
    }

    /// Get window at position
    pub fn window_at_position(&self, x: i32, y: i32) -> Option<u32> {
        let windows = self.windows.read();
        let mut window_list: Vec<_> = windows.values().collect();

        // Sort by z-order (highest first)
        window_list.sort_by(|a, b| b.z_order.cmp(&a.z_order));

        for window in window_list {
            if window.contains_point(x, y) {
                return Some(window.window_id);
            }
        }

        None
    }

    /// Get window under pointer
    pub fn pointer_over_window(&self) -> Option<u32> {
        *self.pointer_over.read()
    }

    /// Get active touch count
    pub fn active_touch_count(&self) -> usize {
        self.active_touches.read().len()
    }

    /// Get focus stack
    pub fn get_focus_stack(&self) -> Vec<u32> {
        self.focus_stack.read().clone()
    }

    /// Dump window manager state (debugging)
    pub fn dump_state(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!("=== Window Manager State ===\n"));
        output.push_str(&format!("Display: {}x{}\n", self.display_width, self.display_height));
        output.push_str(&format!("Modal window: {:?}\n", *self.modal_window.read()));
        output.push_str(&format!("Pointer grab: {:?}\n", *self.pointer_grab.read()));
        output.push_str(&format!("Keyboard grab: {:?}\n", *self.keyboard_grab.read()));
        output.push_str(&format!("Pointer over: {:?}\n", *self.pointer_over.read()));
        output.push_str(&format!("Active touches: {}\n", self.active_touch_count()));
        output.push_str(&format!("Focus stack: {:?}\n", self.get_focus_stack()));

        output.push_str("\n=== Windows (by z-order) ===\n");
        for window in self.get_windows() {
            output.push_str(&format!(
                "  ID: {}, App: {}, Pos: ({},{}), Size: {}x{}, Visible: {}, Focused: {}, Z: {}\n",
                window.window_id, window.app_id, window.x, window.y,
                window.width, window.height, window.is_visible, window.is_focused, window.z_order
            ));
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_registration() {
        let router = InputEventRouter::new(1280, 720);

        let geo = WindowGeometry {
            window_id: 1,
            app_id: "app1".to_string(),
            x: 100,
            y: 100,
            width: 400,
            height: 300,
            is_visible: true,
            z_order: 1,
            is_focused: true,
            is_grabbed: false,
        };

        router.register_window(geo);
        assert_eq!(router.get_windows().len(), 1);
        assert!(router.unregister_window(1));
        assert_eq!(router.get_windows().len(), 0);
    }

    #[test]
    fn test_hit_testing() {
        let router = InputEventRouter::new(1280, 720);

        let geo = WindowGeometry {
            window_id: 1,
            app_id: "app1".to_string(),
            x: 100,
            y: 100,
            width: 400,
            height: 300,
            is_visible: true,
            z_order: 1,
            is_focused: true,
            is_grabbed: false,
        };

        router.register_window(geo);

        // Hit inside window
        assert_eq!(router.window_at_position(200, 200), Some(1));

        // Hit outside window
        assert_eq!(router.window_at_position(50, 50), None);

        // Hit at edge
        assert_eq!(router.window_at_position(100, 100), Some(1));
    }

    #[test]
    fn test_focus_stack() {
        let router = InputEventRouter::new(1280, 720);

        for i in 1..=3 {
            let geo = WindowGeometry {
                window_id: i,
                app_id: format!("app{}", i),
                x: i as i32 * 50,
                y: i as i32 * 50,
                width: 300,
                height: 200,
                is_visible: true,
                z_order: i as i32,
                is_focused: i == 1,
                is_grabbed: false,
            };
            router.register_window(geo);
        }

        // Initially window 1 should be focused
        assert_eq!(router.get_focused_window(), Some(1));

        // Raise window 3
        router.raise_window(3);
        assert_eq!(router.get_focused_window(), Some(3));

        // Lower window 3
        router.lower_window(3);
        assert_eq!(router.get_focused_window(), Some(1));
    }

    #[test]
    fn test_pointer_routing() {
        let router = InputEventRouter::new(1280, 720);

        let geo = WindowGeometry {
            window_id: 1,
            app_id: "app1".to_string(),
            x: 100,
            y: 100,
            width: 400,
            height: 300,
            is_visible: true,
            z_order: 1,
            is_focused: true,
            is_grabbed: false,
        };

        router.register_window(geo);

        let event = InputEvent::Pointer {
            x: 200,
            y: 200,
            button: MouseButton::Left,
            state: ButtonState::Pressed,
        };

        let targets = router.route_event(&event);
        assert_eq!(targets, vec![1]);
    }

    #[test]
    fn test_keyboard_routing() {
        let router = InputEventRouter::new(1280, 720);

        let geo = WindowGeometry {
            window_id: 1,
            app_id: "app1".to_string(),
            x: 100,
            y: 100,
            width: 400,
            height: 300,
            is_visible: true,
            z_order: 1,
            is_focused: true,
            is_grabbed: false,
        };

        router.register_window(geo);

        let event = InputEvent::Keyboard {
            key_code: 65, // 'A'
            state: KeyState::Pressed,
            modifiers: KeyModifiers { shift: false, ctrl: false, alt: false, meta: false },
        };

        let targets = router.route_event(&event);
        assert_eq!(targets, vec![1]);
    }

    #[test]
    fn test_modal_window() {
        let router = InputEventRouter::new(1280, 720);

        for i in 1..=2 {
            let geo = WindowGeometry {
                window_id: i,
                app_id: format!("app{}", i),
                x: 100 + i as i32 * 50,
                y: 100,
                width: 300,
                height: 200,
                is_visible: true,
                z_order: i as i32,
                is_focused: false,
                is_grabbed: false,
            };
            router.register_window(geo);
        }

        // Set window 2 as modal
        router.set_modal_window(Some(2));

        let event = InputEvent::Pointer {
            x: 200,
            y: 200,
            button: MouseButton::Left,
            state: ButtonState::Pressed,
        };

        // Event should go to modal window only
        let targets = router.route_event(&event);
        assert_eq!(targets, vec![2]);
    }

    #[test]
    fn test_multi_touch() {
        let router = InputEventRouter::new(1280, 720);

        let geo = WindowGeometry {
            window_id: 1,
            app_id: "app1".to_string(),
            x: 100,
            y: 100,
            width: 400,
            height: 300,
            is_visible: true,
            z_order: 1,
            is_focused: true,
            is_grabbed: false,
        };

        router.register_window(geo);

        // Start two touches
        let touch1_start = InputEvent::Touch {
            touch_id: 1,
            x: 150,
            y: 150,
            state: TouchState::Start,
            pressure: 1.0,
        };

        let touch2_start = InputEvent::Touch {
            touch_id: 2,
            x: 250,
            y: 250,
            state: TouchState::Start,
            pressure: 0.8,
        };

        assert_eq!(router.route_event(&touch1_start), vec![1]);
        assert_eq!(router.route_event(&touch2_start), vec![1]);
        assert_eq!(router.active_touch_count(), 2);
    }
}