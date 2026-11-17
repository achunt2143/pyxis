//! Lumen's Recede (show desktop) and its way back, Emerge: each window goes
//! soft and sinks away, like a lens pulling focus past it. Spec:
//! `docs/internal/evo-animation-plan.md`, Lumen, "Show desktop".
//!
//! - **Recede**, ~460 ms: scale 1 → 0.88 about the frame's centre, drift
//!   14 px down, blur 0 → 10 px, alpha 1 → 0 on a power-1.6 curve (it holds
//!   while it softens, then goes).
//! - **Emerge**, ~440 ms: the same, run back, on mirrored curves.
//!
//! The `TransformAnim` here is the whole motion (scale, drift, alpha); the
//! blur is read off its scale (`progress`), so the plain fallback (the live
//! window under the same transform, no blur) runs on the very same clock.
//! The picture is a snapshot taken when the window leaves or comes back,
//! drawn through Lumen's program ([`lumen::Source`], counted as
//! `Effect::Recede`). Reduced is an opacity fade (≤ 150 ms, no blur, no
//! picture); off is nothing. The stagger and the state ("desktop shown")
//! are `shell/recede.rs`.

use std::time::Duration;

use smithay::utils::{Logical, Rectangle};

use super::{
    curve::{Animation, CubicBezier, Timing, Vec2},
    lumen::{self, Effect, Source},
    settings::AnimationSettings,
    shader::GlesAccess,
    snapshot::Snapshot,
    spec,
    transform::WindowTransform,
    warp::{Drawn, Placement},
    window::{Origin, TransformAnim},
};
use crate::shell::identity::Radii;

/// Movement (scale and drift) on the way out: a gentle in-out.
pub const RECEDE_CURVE: CubicBezier = CubicBezier::new(0.4, 0.0, 0.2, 1.0);
/// Alpha on the way out: a cubic-bezier fit of `t^1.6` (within 0.023).
pub const RECEDE_ALPHA_CURVE: CubicBezier = CubicBezier::new(0.35, 0.05, 0.75, 0.55);
/// The same two, mirrored, for the way back.
pub const EMERGE_CURVE: CubicBezier = mirror(RECEDE_CURVE);
pub const EMERGE_ALPHA_CURVE: CubicBezier = mirror(RECEDE_ALPHA_CURVE);

/// A curve run backwards in time and value: `y(x) = 1 - c(1 - x)`.
const fn mirror(c: CubicBezier) -> CubicBezier {
    CubicBezier::new(1.0 - c.x2, 1.0 - c.y2, 1.0 - c.x1, 1.0 - c.y1)
}

/// How far through a Recede a pose is (0 the window, 1 gone), read off its
/// scale: what the blur follows.
pub fn progress(scale: f64) -> f64 {
    ((1.0 - scale) / (1.0 - spec::RECEDE_SCALE_TO)).clamp(0.0, 1.0)
}

/// The receded pose's values: scale, drift, alpha.
fn receded() -> (Vec2, Vec2, f64) {
    (Vec2::splat(spec::RECEDE_SCALE_TO), Vec2::new(0.0, spec::RECEDE_DRIFT_PX), 0.0)
}

/// The Recede, starting at `start` (show desktop staggers it), or `None` when
/// settings say off. Reduced: a fade in place.
pub fn recede(settings: &AnimationSettings, start: Duration) -> Option<TransformAnim> {
    let duration = settings.duration(spec::RECEDE)?;
    let (scale_to, drift_to, _) = receded();
    if !settings.motion() {
        return Some(fade(1.0, 0.0, start, duration));
    }
    let timing = Timing::curve(duration, RECEDE_CURVE);
    Some(TransformAnim {
        origin: Origin::Frame(0.5, 0.5),
        scale: Animation::new(Vec2::splat(1.0), scale_to, start, timing),
        offset: Animation::new(Vec2::splat(0.0), drift_to, start, timing),
        alpha: Animation::new(1.0, 0.0, start, Timing::curve(duration, RECEDE_ALPHA_CURVE)),
    })
}

/// Where an Emerge starts from: the receded pose, or wherever a Recede that
/// is still running has got to (Super+D pressed twice quickly), so nothing
/// jumps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmergeFrom {
    pub scale: Vec2,
    pub offset: Vec2,
    pub alpha: f64,
}

impl EmergeFrom {
    pub fn receded() -> Self {
        let (scale, offset, alpha) = receded();
        Self { scale, offset, alpha }
    }

    /// A running Recede's values at `now`. Before its start it has not left
    /// yet: the identity, and the Emerge has nothing to do but be short.
    pub fn sample(anim: &TransformAnim, now: Duration) -> Self {
        Self { scale: anim.scale.value(now), offset: anim.offset.value(now), alpha: anim.alpha.value(now) }
    }

    /// How much of the way out this is (1 fully receded), by the larger of
    /// the scale and alpha distances.
    fn amount(&self) -> f64 {
        progress(self.scale.y).max(1.0 - self.alpha).clamp(0.0, 1.0)
    }
}

/// The Emerge from `from`, starting at `start`. A partial recede comes back in
/// the matching part of the time.
pub fn emerge(settings: &AnimationSettings, start: Duration, from: EmergeFrom) -> Option<TransformAnim> {
    let full = settings.duration(spec::EMERGE)?;
    let duration = full.mul_f64(from.amount().max(0.2));
    if !settings.motion() {
        return Some(fade(from.alpha, 1.0, start, duration));
    }
    let timing = Timing::curve(duration, EMERGE_CURVE);
    Some(TransformAnim {
        origin: Origin::Frame(0.5, 0.5),
        scale: Animation::new(from.scale, Vec2::splat(1.0), start, timing),
        offset: Animation::new(from.offset, Vec2::splat(0.0), start, timing),
        alpha: Animation::new(from.alpha, 1.0, start, Timing::curve(duration, EMERGE_ALPHA_CURVE)),
    })
}

fn fade(from: f64, to: f64, start: Duration, duration: Duration) -> TransformAnim {
    TransformAnim {
        origin: Origin::Frame(0.5, 0.5),
        scale: Animation::fixed(Vec2::splat(1.0)),
        offset: Animation::fixed(Vec2::splat(0.0)),
        alpha: Animation::new(from, to, start, Timing::curve(duration, super::curve::LINEAR)),
    }
}

/// The picture a Recede or Emerge draws.
pub struct RecedeFx {
    source: Source,
    /// The frame's corners when it left, logical (`picture_radii`).
    corners: Radii,
}

impl std::fmt::Debug for RecedeFx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecedeFx").field("source", &self.source).finish()
    }
}

impl RecedeFx {
    pub fn new(snapshot: Snapshot, corners: Radii) -> Self {
        Self { source: Source::new(snapshot), corners }
    }

    /// One frame for the pose `t`, or `None` to draw the plain fallback (the
    /// live window under `t`): Lumen off or unable to draw.
    pub fn draw<R: GlesAccess>(&self, renderer: &mut R, place: &Placement, t: &WindowTransform) -> Option<Drawn> {
        if !lumen::enabled() {
            return None;
        }
        let p = progress(t.scale.y);
        let s = place.scale;
        let f = place.frame;
        let frame_px = [
            (f.loc.x - place.output_origin.x) as f64 * s,
            (f.loc.y - place.output_origin.y) as f64 * s,
            f.size.w as f64 * s,
            f.size.h as f64 * s,
        ];
        let drawn = posed(f, t);
        let snap = self.source.snapshot();
        let top_left = (place.render_origin + snap.origin).to_f64();
        let mut params = lumen::Params::picture(&self.source, top_left);
        params.center = [
            (drawn.loc.x + drawn.size.x / 2.0 - place.output_origin.x as f64) * s,
            (drawn.loc.y + drawn.size.y / 2.0 - place.output_origin.y as f64) * s,
        ];
        params.size = [drawn.size.x * s, drawn.size.y * s];
        // Only the frame (a client's shadow margin stays out), with the
        // frame's own corners.
        params.uv = self.source.uv_of(top_left, frame_px);
        params.radii = self.corners.scale(s * t.scale.y);
        params.blur = spec::RECEDE_BLUR_PX * s * p;
        params.alpha = t.alpha;
        let element = self.source.element(renderer, &params, Effect::Recede)?;
        lumen::record_progress(Effect::Recede, p);
        Some(Drawn { element, progress: p })
    }
}

/// Where `t` draws `frame`: logical, fractional (top-left, size).
fn posed(frame: Rectangle<i32, Logical>, t: &WindowTransform) -> super::geometry::FRect {
    let x = t.origin.x + (frame.loc.x as f64 - t.origin.x) * t.scale.x + t.offset.x;
    let y = t.origin.y + (frame.loc.y as f64 - t.origin.y) * t.scale.y + t.offset.y;
    super::geometry::FRect {
        loc: Vec2::new(x, y),
        size: Vec2::new(frame.size.w as f64 * t.scale.x, frame.size.h as f64 * t.scale.y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::settings::Mode;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn full() -> AnimationSettings {
        AnimationSettings::default()
    }

    fn frame() -> Rectangle<i32, Logical> {
        Rectangle::new((200, 100).into(), (800, 600).into())
    }

    #[test]
    fn recede_shrinks_to_point_eight_eight_drifts_fourteen_down_and_fades() {
        let a = recede(&full(), ms(0)).unwrap();
        let start = a.sample(ms(0), frame());
        assert!(start.is_identity());
        let end = a.sample(spec::RECEDE, frame());
        assert!((end.scale.y - 0.88).abs() < 1e-9);
        assert!((end.offset.y - 14.0).abs() < 1e-9);
        assert_eq!(end.alpha, 0.0);
        assert!(a.is_finished(spec::RECEDE));
        // About its centre: the centre only drifts down.
        let mid = a.sample(spec::RECEDE / 2, frame());
        let r = posed(frame(), &mid);
        assert!((r.loc.x + r.size.x / 2.0 - 600.0).abs() < 1e-6);
        assert!(r.loc.y + r.size.y / 2.0 > 400.0);
    }

    #[test]
    fn its_alpha_follows_power_one_point_six() {
        let a = recede(&full(), ms(0)).unwrap();
        for k in 0..=10 {
            let x = k as f64 / 10.0;
            let alpha = a.alpha.value(spec::RECEDE.mul_f64(x));
            assert!((alpha - (1.0 - x.powf(1.6))).abs() < 0.03, "{x}: {alpha}");
        }
    }

    #[test]
    fn the_blur_follows_the_scale() {
        assert_eq!(progress(1.0), 0.0);
        assert!((progress(0.94) - 0.5).abs() < 1e-9);
        assert_eq!(progress(0.88), 1.0);
        assert_eq!(progress(0.5), 1.0, "clamped");
    }

    #[test]
    fn a_staggered_recede_holds_the_window_until_its_turn() {
        let a = recede(&full(), ms(90)).unwrap();
        assert!(a.sample(ms(40), frame()).is_identity());
        assert!(!a.is_finished(ms(40)));
        assert!(a.is_finished(ms(90) + spec::RECEDE));
    }

    #[test]
    fn emerge_runs_it_back_from_where_it_is() {
        let e = emerge(&full(), ms(0), EmergeFrom::receded()).unwrap();
        let s = e.sample(ms(0), frame());
        assert!((s.scale.y - 0.88).abs() < 1e-9 && s.alpha == 0.0);
        assert!(e.sample(spec::EMERGE, frame()).is_identity());
        // Half way out, the way back starts there and is quicker.
        let r = recede(&full(), ms(0)).unwrap();
        let from = EmergeFrom::sample(&r, spec::RECEDE / 2);
        let e = emerge(&full(), ms(0), from).unwrap();
        let s = e.sample(ms(0), frame());
        assert!((s.scale.y - from.scale.y).abs() < 1e-9 && (s.alpha as f64 - from.alpha).abs() < 1e-6);
        assert!(e.is_finished(spec::EMERGE.mul_f64(0.9)));
    }

    #[test]
    fn mirrored_curves_run_back() {
        for k in 0..=10 {
            let x = k as f64 / 10.0;
            let back = EMERGE_ALPHA_CURVE.ease(x);
            assert!((back - (1.0 - RECEDE_ALPHA_CURVE.ease(1.0 - x))).abs() < 1e-3, "{x}");
        }
    }

    #[test]
    fn reduced_fades_in_place_and_off_does_nothing() {
        let reduced = AnimationSettings { mode: Mode::Reduced, scale: 1.0 };
        let a = recede(&reduced, ms(0)).unwrap();
        let mid = a.sample(ms(75), frame());
        assert_eq!((mid.scale, mid.offset), (Vec2::splat(1.0), Vec2::splat(0.0)));
        assert!(mid.alpha > 0.0 && mid.alpha < 1.0);
        assert!(a.is_finished(ms(150)), "capped at 150 ms");
        let e = emerge(&reduced, ms(0), EmergeFrom::receded()).unwrap();
        assert_eq!(e.sample(ms(0), frame()).scale, Vec2::splat(1.0));
        let off = AnimationSettings { mode: Mode::Off, scale: 1.0 };
        assert!(recede(&off, ms(0)).is_none());
        assert!(emerge(&off, ms(0), EmergeFrom::receded()).is_none());
    }
}
