//! Window animation: the foundation, and open/close/popup on top of it.
//! Design and motion spec: `docs/internal/evo-animation-plan.md`.
//!
//! # For the phase 2 lanes
//!
//! **Animation is a render-time transform.** Hit-testing, focus, the work
//! area, `ListWindows` and BindSurface all use a window's *final* geometry
//! from the first frame; nothing here changes where a window is, only how it
//! is drawn. Never read an animated pose back into placement.
//!
//! ## The pieces
//!
//! - [`curve`]: `CubicBezier` easing, an analytic `Spring`, and
//!   `Animation<T>` over `f64` or `Vec2` with an injected clock. `retarget`
//!   turns an animation toward a new target from its current value and (for a
//!   spring) its velocity: nothing is queued, nothing jumps.
//! - [`settings`]: `settings()` gives the mode (`full`/`reduced`/`off`) and
//!   the duration scale. Build every animation through
//!   `AnimationSettings::duration(base)`, which returns `None` for "don't
//!   animate" and applies the reduced cap and the scale.
//! - [`spec`]: the motion tokens (durations, curves, springs). Bloom's
//!   `motion.ts` carries the same names and numbers.
//! - [`window`]: `TransformAnim` (scale, offset and alpha, each an
//!   `Animation`, about an `Origin`), `WindowAnim` (one running animation
//!   tagged with an [`AnimKind`]), and the counters. Constructors:
//!   `TransformAnim::{open, close, popup_in}`. Write yours the same way:
//!   take `&AnimationSettings` and `now`, return `Option`.
//! - [`slot`]: on `WindowElement`, `start_animation(WindowAnim)`,
//!   `with_animation(|a| a.anim.retarget(now, target))`, `animating()`,
//!   `cancel_animation()`, `animation_transform(now, frame)`. A window has at
//!   most one running animation; starting another drops (and so completes)
//!   the old one, so retarget it instead when you want continuity.
//! - [`transform`]: `WindowTransform` and `wrap`, which scales a window's
//!   render elements (SSD included) about an origin and offsets them.
//!   `render::output_elements` applies it to every window with a running
//!   animation; you do not call it for live windows.
//! - [`snapshot`]: `Backend::snapshot_window(window, scale)` renders a window
//!   to pixels *now*. Take it before the buffer goes (or before a geometry
//!   change); it needs the renderer, so it is a `Backend` method, which the
//!   udev backend implements. `render_to_mem` is the shared offscreen path
//!   (screencopy uses it too).
//! - [`ghosts`]: `ghosts::add(Ghost { source, anim, anchor, frame,
//!   render_origin })` draws a window that has left the Space, at the z-slot
//!   above `anchor` (the window next below it when it left). `Live(window)`
//!   draws a live surface (minimize); `Snapshot` draws pixels (close). A ghost
//!   takes no input and is dropped when its animation ends.
//! - [`lumen`]: the Lumen program, which draws a snapshot (`lumen::Source`)
//!   with a rounded clip, blur, accent tint and glow, alpha and squash/stretch
//!   (`Params`), as a `LumenElement`; plus `Effect` frame counters, the
//!   switch (`EVO_LUMEN`, `SetLumen`) and `GetLumenState`. Its header shows the
//!   API. [`droplet`] is the minimize/restore pose drawn through it
//!   ([`warp`] the payload); [`genie`] is what is left of the retired funnel.
//!
//! ## Rules
//!
//! 1. Count by owning a `WindowAnim`: it is *started* when made and
//!    *completed* when dropped, however it ended. The harness asserts
//!    `started == completed` and `active == 0`, so never hold one forever.
//! 2. Don't animate non-bindable windows (the `vela-default` about:blank
//!    pre-window), the shell, or the overlay. See `EvoState::animation_eligible`.
//! 3. Reduced mode is opacity only: build the `TransformAnim` with fixed
//!    scale/offset (`TransformAnim::open` shows how).
//! 4. Alpha-only changes are repainted: smithay's damage tracker compares each
//!    element's alpha frame to frame (verified in its source, and by the
//!    `alpha_changes_are_damage` test in `render.rs`), so the wrapper reports
//!    nothing extra.
//! 5. Time is `anim::now()`, a monotonic clock. Tests inject a `Duration`.
//!
//! ## IPC
//!
//! `org.geckoOS.evo` `SetAnimationSettings {mode?, scale?}` and
//! `GetAnimationState {}` (see `enhancements/echo_ipc.rs`). The latter reports
//! the counters per kind, which is what the on-device suite asserts.

pub mod bloom;
pub mod clear;
pub mod curve;
pub mod depth;
pub mod droplet;
pub mod evaporate;
pub mod genie;
pub mod geometry;
pub mod glint;
pub mod ghosts;
pub mod lumen;
pub mod minimize;
pub mod recede;
pub mod settings;
pub mod shader;
pub mod slot;
pub mod snapshot;
pub mod spec;
pub mod transform;
pub mod veil;
pub mod warp;
pub mod window;

use std::{sync::OnceLock, time::{Duration, Instant}};

pub use curve::{Animation, CubicBezier, Spring, Timing, Vec2};
pub use settings::{settings, AnimationSettings, Mode};
pub use snapshot::Snapshot;
pub use transform::WindowTransform;
pub use window::{stats, AnimKind, AnimStats, Origin, TransformAnim, TransformTarget, WindowAnim};

/// The animation clock: time since the first call, monotonic.
pub fn now() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// Animations in flight: windows animating plus ghosts being drawn. Read by
/// `GetAnimationState` from the Echo thread, so it is the shared counter, not a
/// walk over windows.
pub fn active_count() -> u32 {
    stats().active()
}
