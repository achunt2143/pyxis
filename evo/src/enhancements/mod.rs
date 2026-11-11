//! Optional enhancement modules for GeckoOS Compositor
//!
//! These modules provide additional functionality beyond the core compositor.
//! Each can be enabled/disabled via Cargo features.

/// Server-side window decorations with theme support
///
/// Provides window title bars, control buttons, borders, shadows,
/// and other visual decorations with customizable themes.
#[cfg(feature = "decorations")]
pub mod decorations;

/// Echo IPC service for system communication
///
/// Enables communication between the compositor and other system services
/// via the Echo IPC bus for managing windows, application lifecycle, etc.
#[cfg(feature = "echo-ipc")]
pub mod echo_ipc;

/// Screencopy Support
///
/// Adds screencopy support
/// so apps can screenshot/record the screen
#[cfg(feature = "screencopy")]
pub mod screencopy;


// Re-export feature-gated modules for convenience
#[cfg(feature = "decorations")]
pub use decorations::*;

#[cfg(feature = "echo-ipc")]
pub use echo_ipc::*;

