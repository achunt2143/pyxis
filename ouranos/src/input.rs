use smithay::input::keyboard::{KeyboardHandle, KeyboardTarget};
use smithay::input::pointer::{PointerHandle, PointerTarget, CursorImageStatus};
use smithay::input::touch::TouchHandle;
use smithay::backend::input::{Event, InputEvent, KeyboardKeyEvent, PointerMotionEvent, PointerButtonEvent, PointerAxisEvent};
use xkbcommon::xkb;
use std::collections::HashSet;

pub struct InputState {
    pub keyboard: Option<KeyboardHandle>,
    pub pointer: Option<PointerHandle>,
    pub touch: Option<TouchHandle>,
    pub modifiers: ModifierState,
    pub pressed_keys: HashSet<u32>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ModifierState {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
}

impl InputState {
    pub fn new() -> Self {
        InputState {
            keyboard: None,
            pointer: None,
            touch: None,
            modifiers: ModifierState::default(),
            pressed_keys: HashSet::new(),
        }
    }

    /// Process keyboard event
    pub fn process_keyboard_event(&mut self, event: KeyboardKeyEvent) {
        let key_code = event.key();
        
        if event.state() == smithay::backend::input::KeyState::Pressed {
            self.pressed_keys.insert(key_code);
            tracing::trace!("Key pressed: {}", key_code);
        } else {
            self.pressed_keys.remove(&key_code);
            tracing::trace!("Key released: {}", key_code);
        }

        // Update modifier state based on key
        self.update_modifiers(key_code, event.state() == smithay::backend::input::KeyState::Pressed);
    }

    /// Process pointer motion
    pub fn process_pointer_motion(&mut self, event: PointerMotionEvent) {
        let dx = event.delta_x();
        let dy = event.delta_y();
        tracing::trace!("Pointer motion: dx={}, dy={}", dx, dy);
    }

    /// Process pointer button
    pub fn process_pointer_button(&mut self, event: PointerButtonEvent) {
        let button = event.button();
        let state = event.state();
        tracing::trace!("Pointer button {}: {:?}", button, state);
    }

    /// Process pointer axis (scroll wheel)
    pub fn process_pointer_axis(&mut self, event: PointerAxisEvent) {
        tracing::trace!("Pointer axis event");
    }

    /// Update modifier state based on key code
    fn update_modifiers(&mut self, key_code: u32, pressed: bool) {
        // Map Linux key codes to modifiers
        // These are evdev key codes
        match key_code {
            42 | 54 => self.modifiers.shift = pressed,      // Shift (left/right)
            29 | 97 => self.modifiers.ctrl = pressed,       // Ctrl (left/right)
            56 | 108 => self.modifiers.alt = pressed,       // Alt (left/right)
            125 | 126 => self.modifiers.super_key = pressed, // Super (left/right)
            58 => self.modifiers.caps_lock = pressed,       // Caps Lock
            69 => self.modifiers.num_lock = pressed,        // Num Lock
            _ => {}
        }
    }

    /// Check if key is currently pressed
    pub fn is_key_pressed(&self, key_code: u32) -> bool {
        self.pressed_keys.contains(&key_code)
    }

    /// Get modifier combination as bitmask
    pub fn modifier_mask(&self) -> u32 {
        let mut mask = 0u32;
        if self.modifiers.shift { mask |= 0x01; }
        if self.modifiers.ctrl { mask |= 0x02; }
        if self.modifiers.alt { mask |= 0x04; }
        if self.modifiers.super_key { mask |= 0x08; }
        if self.modifiers.caps_lock { mask |= 0x10; }
        if self.modifiers.num_lock { mask |= 0x20; }
        mask
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_state_creation() {
        let input = InputState::new();
        assert_eq!(input.pressed_keys.len(), 0);
        assert!(!input.modifiers.shift);
    }

    #[test]
    fn test_modifier_mask() {
        let mut input = InputState::new();
        input.modifiers.shift = true;
        input.modifiers.ctrl = true;
        assert_eq!(input.modifier_mask(), 0x03);
    }
}
