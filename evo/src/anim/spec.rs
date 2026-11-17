//! The motion spec: the tokens of `docs/internal/evo-animation-plan.md`, with
//! the names and values Bloom's `src/motion.ts` carries. Change both together.
//!
//! Durations are at scale 1; `AnimationSettings::duration` applies mode and
//! scale. Tokens used by phase 2 lanes are declared now so the numbers have
//! one home.

use std::time::Duration;

use super::curve::{CubicBezier, Spring, ACCELERATE, EXPO_OUT, LINEAR, STANDARD};

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// open: scale 0.92 -> 1 about the launch origin (else the centre, 12 px low),
// alpha 0 -> 1 done by 60% of the duration.
pub const OPEN: Duration = ms(260);
pub const OPEN_CURVE: CubicBezier = EXPO_OUT;
pub const OPEN_SCALE_FROM: f64 = 0.92;
pub const OPEN_RISE_PX: f64 = 12.0;
pub const OPEN_ALPHA_FRACTION: f64 = 0.6;

// Bloom, Lumen's open (`anim::bloom`): scale 0.90 -> 1 on a ζ 0.86 spring,
// rising 16 px (expo out), alpha done by 35%, blur 10 px -> 0 (cubic out, the
// focus pull), corners 42 -> the frame's own, an accent tint 0.45 -> 0 over
// the first half, and a radial glow of the accent behind the window that peaks
// mid-way. The `OPEN_*` tokens above are the plain open, drawn with Lumen off.
pub const BLOOM: Duration = ms(420);
pub const BLOOM_ZETA: f64 = 0.86;
pub const BLOOM_SCALE_FROM: f64 = 0.90;
pub const BLOOM_RISE_PX: f64 = 16.0;
pub const BLOOM_ALPHA_FRACTION: f64 = 0.35;
pub const BLOOM_BLUR_PX: f64 = 10.0;
pub const BLOOM_RADIUS_FROM: f64 = 42.0;
pub const BLOOM_TINT: f64 = 0.45;
pub const BLOOM_TINT_FRACTION: f64 = 0.5;
pub const BLOOM_GLOW_ALPHA: f64 = 0.33;
/// The glow's radius, as a fraction of the window's long side.
pub const BLOOM_GLOW_REACH: f64 = 0.75;

pub fn bloom_spring(duration: Duration) -> Spring {
    Spring::settling_in(duration, BLOOM_ZETA)
}

// close: a snapshot scales 1 -> 0.94 and fades out.
pub const CLOSE: Duration = ms(180);
pub const CLOSE_CURVE: CubicBezier = ACCELERATE;
pub const CLOSE_SCALE_TO: f64 = 0.94;

// popups: alpha, plus scale 0.97 from the anchor edge.
pub const POPUP_IN: Duration = ms(120);
pub const POPUP_IN_CURVE: CubicBezier = EXPO_OUT;
pub const POPUP_OUT: Duration = ms(80);
pub const POPUP_OUT_CURVE: CubicBezier = LINEAR;
pub const POPUP_SCALE_FROM: f64 = 0.97;

// minimize / restore: a spring to and from the dock icon.
pub const MINIMIZE: Duration = ms(320);
pub const RESTORE: Duration = ms(300);
pub const MINIMIZE_ZETA: f64 = 0.9;
pub const SHOW_DESKTOP_STAGGER: Duration = ms(45);

// Lumen's Recede (show desktop) and Emerge (its way back): scale 1 -> 0.88
// about the centre, 14 px down, blur 0 -> 10 px, alpha on a power-1.6 curve.
// Staggered by SHOW_DESKTOP_STAGGER, front-most first out, back-most first in.
pub const RECEDE: Duration = ms(460);
pub const EMERGE: Duration = ms(440);
pub const RECEDE_SCALE_TO: f64 = 0.88;
pub const RECEDE_DRIFT_PX: f64 = 14.0;
pub const RECEDE_BLUR_PX: f64 = 10.0;

// Lumen's Veil (the screen locks) and Clear (it unlocks); the lock page's own
// half of both is Bloom's (docs/internal/pyxis-glass-and-lock-plan.md).
//
// Veil: each app window's picture blurs 0 -> 14 px, loses its colour and fades
// out over 600 ms, beneath the lock page fading in. Clear: the desktop and the
// windows come into focus about the field the password was typed in, over
// 900 ms: blur 14 -> 0 px and scale 1.04 -> 1, both on
// `f = 1 - (1 - min(1, t / 0.85))^3` (so they are done at 85% of the run, and
// the last 15% is only the lock page's ring leaving).
pub const VEIL: Duration = ms(600);
pub const VEIL_BLUR_PX: f64 = 14.0;
/// How far toward grey the picture goes by the end (1 would be a flat grey).
pub const VEIL_DESATURATE: f64 = 0.85;
pub const CLEAR: Duration = ms(900);
pub const CLEAR_BLUR_PX: f64 = 14.0;
pub const CLEAR_SCALE_FROM: f64 = 1.04;
/// The share of Clear's run the focus pull takes.
pub const CLEAR_PULL_FRACTION: f64 = 0.85;

// geometry: maximize, fullscreen, fit to the work area.
pub const GEOMETRY: Duration = ms(280);
pub const GEOMETRY_ZETA: f64 = 1.0;
/// Fraction of a geometry transition over which snapshot crossfades to live.
pub const GEOMETRY_CROSSFADE: f64 = 0.4;

// focus / lift.
pub const FOCUS: Duration = ms(150);
pub const LIFT: Duration = ms(120);
pub const STANDARD_CURVE: CubicBezier = STANDARD;

/// Rest threshold for springs on normalised (0..1) values and on pixels.
pub const REST_NORMALISED: f64 = 0.002;
pub const REST_PIXELS: f64 = 0.25;

pub fn minimize_spring(duration: Duration) -> Spring {
    Spring::settling_in(duration, MINIMIZE_ZETA)
}

pub fn geometry_spring(duration: Duration) -> Spring {
    Spring::settling_in(duration, GEOMETRY_ZETA)
}
