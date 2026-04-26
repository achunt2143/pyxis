//! Optional enhancement modules for GeckoOS Compositor
//!
//! These modules provide additional functionality beyond the core compositor.
//! Each can be enabled/disabled via Cargo features.

/// Damage tracking for optimized rendering
///
/// Tracks which regions of surfaces have changed and need redrawing.
/// This optimization reduces unnecessary full-screen redraws.
#[cfg(feature = "damage-tracking")]
pub mod damage_tracker;

/// Server-side window decorations with theme support
///
/// Provides window title bars, control buttons, borders, shadows,
/// and other visual decorations with customizable themes.
#[cfg(feature = "decorations")]
pub mod decorations;

/// Hot-plug management for displays and input devices
///
/// Handles dynamic connection/disconnection of monitors, keyboards,
/// mice, and other input devices without restarting the compositor.
#[cfg(feature = "hotplug")]
pub mod hotplug;

/// Echo IPC service for system communication
///
/// Enables communication between the compositor and other system services
/// via the Echo IPC bus for managing windows, application lifecycle, etc.
#[cfg(feature = "echo-ipc")]
pub mod echo_ipc;

/// Advanced input management (keyboard repeat, gestures, tablets)
///
/// Provides sophisticated input handling including:
/// - Keyboard repeat rates and delays
/// - Pointer acceleration and scroll factors
/// - Multi-touch gesture recognition
/// - Tablet tool support
#[cfg(feature = "advanced-input")]
pub mod input_advanced;

/// Data device support for clipboard and drag-drop
///
/// Implements Wayland data device protocol for:
/// - Clipboard copy/paste operations
/// - Drag and drop between windows
/// - Selection and primary selection support
#[cfg(feature = "data-device")]
pub mod data_device;

/// Window layout enhancements
///
/// Adds new window management features
/// - stack management
/// - more to come
#[cfg(feature = "window-enhance")]
pub mod window_stack;

/// Screencopy Support
///
/// Adds screencopy support
/// so apps can screenshot/record the screen
#[cfg(feature = "screencopy")]
pub mod screencopy;


// Re-export feature-gated modules for convenience
#[cfg(feature = "damage-tracking")]
pub use damage_tracker::*;

#[cfg(feature = "decorations")]
pub use decorations::*;

#[cfg(feature = "hotplug")]
pub use hotplug::*;

#[cfg(feature = "echo-ipc")]
pub use echo_ipc::*;

#[cfg(feature = "advanced-input")]
pub use input_advanced::*;

#[cfg(feature = "data-device")]
pub use data_device::*;

#[cfg(feature = "window-enhance")]
pub use window_stack::*;

#[cfg(feature = "screencopy")]
pub use data_device::*;
