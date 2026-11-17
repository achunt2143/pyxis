//! Lumen's Veil: the screen locks, and each app window blurs, loses its
//! colour and fades out while the lock page fades in over it.
//! Spec: `docs/internal/pyxis-glass-and-lock-plan.md`, Part 2.
//!
//! - ~600 ms. The window's picture blurs 0 -> 14 px, goes toward grey (the
//!   Lumen program has no desaturate, so this is its tint toward a mid grey:
//!   it keeps some of the picture's light and dark) and fades out.
//! - It is a **ghost** of each app window that was on screen, taken from a
//!   snapshot when the lock begins and drawn only by the locked frame
//!   (`render::locked_elements`), beneath the overlay and above black. These
//!   are the only app pixels Evo draws while locked, and only for the run of
//!   the Veil: the ghost list is pruned to Veil ghosts every locked frame.
//! - The `TransformAnim` (`transform`) is only the fade: alpha 1 -> 0, linear,
//!   which the blur and tint are read off (`progress`). With no picture (Lumen
//!   off, its program failed, a backend that can't snapshot) the ghost draws
//!   the live window under that same fade: the plain fallback. Reduced: a
//!   fade of at most 150 ms with no picture; off: no ghost at all.

use std::time::Duration;

use super::{
    curve::{Animation, CubicBezier, Timing, Vec2, LINEAR},
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

/// How the blur and the greying come on: slowly at first, so the first frames
/// are still the window (the same gentle in-out Recede moves on).
pub const VEIL_CURVE: CubicBezier = CubicBezier::new(0.4, 0.0, 0.2, 1.0);

/// The mid grey the picture is tinted toward. The tint keeps the picture's own
/// light and dark a little, so this is a lowering of colour, not a flat fill.
pub const VEIL_GREY: [f32; 3] = [0.55, 0.55, 0.55];

/// The fade: alpha 1 -> 0 about nothing (no scale, no drift), or `None` when
/// settings say off. Reduced is the same fade capped at 150 ms
/// (`AnimationSettings::duration` does the cap).
pub fn transform(settings: &AnimationSettings, now: Duration) -> Option<TransformAnim> {
    let duration = settings.duration(spec::VEIL)?;
    Some(TransformAnim {
        origin: Origin::Frame(0.5, 0.5),
        scale: Animation::fixed(Vec2::splat(1.0)),
        offset: Animation::fixed(Vec2::splat(0.0)),
        alpha: Animation::new(1.0, 0.0, now, Timing::curve(duration, LINEAR)),
    })
}

/// How far through the Veil a pose is, 0 the window, 1 gone: read off its
/// alpha, which is linear in time.
pub fn progress(alpha: f32) -> f64 {
    (1.0 - alpha as f64).clamp(0.0, 1.0)
}

/// The picture's blur (logical px) and grey tint (0..1) at progress `p`.
pub fn look(p: f64) -> (f64, f64) {
    let e = VEIL_CURVE.ease(p.clamp(0.0, 1.0));
    (spec::VEIL_BLUR_PX * e, spec::VEIL_DESATURATE * e)
}

/// The picture a Veil draws.
pub struct VeilFx {
    source: Source,
    /// The frame's corners when the lock began, logical.
    corners: Radii,
}

impl std::fmt::Debug for VeilFx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VeilFx").field("source", &self.source).finish()
    }
}

impl VeilFx {
    pub fn new(snapshot: Snapshot, corners: Radii) -> Self {
        Self { source: Source::new(snapshot), corners }
    }

    /// One frame at pose `t`, or `None` to draw the plain fallback (the live
    /// window under the same fade): Lumen off or unable to draw.
    pub fn draw<R: GlesAccess>(&self, renderer: &mut R, place: &Placement, t: &WindowTransform) -> Option<Drawn> {
        if !lumen::enabled() {
            return None;
        }
        let p = progress(t.alpha);
        let (blur, tint) = look(p);
        let s = place.scale;
        let f = place.frame;
        let frame_px = [
            (f.loc.x - place.output_origin.x) as f64 * s,
            (f.loc.y - place.output_origin.y) as f64 * s,
            f.size.w as f64 * s,
            f.size.h as f64 * s,
        ];
        let snap = self.source.snapshot();
        let top_left = (place.render_origin + snap.origin).to_f64();
        let mut params = lumen::Params::picture(&self.source, top_left);
        // Only the frame (a client's shadow margin stays out), with its own
        // corners, where it is.
        params.center = [frame_px[0] + frame_px[2] / 2.0, frame_px[1] + frame_px[3] / 2.0];
        params.size = [frame_px[2], frame_px[3]];
        params.uv = self.source.uv_of(top_left, frame_px);
        params.radii = self.corners.scale(s);
        params.blur = blur * s;
        params.tint = tint;
        params.accent = VEIL_GREY;
        params.alpha = t.alpha;
        let element = self.source.element(renderer, &params, Effect::Veil)?;
        lumen::record_progress(Effect::Veil, p);
        Some(Drawn { element, progress: p })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::settings::Mode;
    use smithay::utils::{Logical, Rectangle};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn frame() -> Rectangle<i32, Logical> {
        Rectangle::new((200, 100).into(), (800, 600).into())
    }

    #[test]
    fn it_fades_the_window_out_over_600_ms_without_moving_it() {
        let a = transform(&AnimationSettings::default(), ms(0)).unwrap();
        assert!(a.sample(ms(0), frame()).is_identity());
        let mid = a.sample(ms(300), frame());
        assert!((mid.alpha - 0.5).abs() < 1e-3);
        assert_eq!((mid.scale, mid.offset), (Vec2::splat(1.0), Vec2::splat(0.0)));
        assert_eq!(a.sample(spec::VEIL, frame()).alpha, 0.0);
        assert!(!a.is_finished(ms(599)));
        assert!(a.is_finished(spec::VEIL));
    }

    #[test]
    fn the_blur_goes_to_fourteen_and_the_colour_most_of_the_way_to_grey() {
        assert_eq!(look(0.0), (0.0, 0.0));
        let (blur, tint) = look(1.0);
        assert!((blur - 14.0).abs() < 1e-9);
        assert!((tint - spec::VEIL_DESATURATE).abs() < 1e-9);
        let mut last = (0.0, 0.0);
        for i in 0..=20 {
            let l = look(i as f64 / 20.0);
            assert!(l.0 >= last.0 && l.1 >= last.1, "only ever more");
            last = l;
        }
    }

    #[test]
    fn progress_is_read_off_the_alpha() {
        assert_eq!(progress(1.0), 0.0);
        assert!((progress(0.25) - 0.75).abs() < 1e-6);
        assert_eq!(progress(0.0), 1.0);
    }

    #[test]
    fn reduced_is_a_fade_of_at_most_150_ms_and_off_is_nothing() {
        let reduced = AnimationSettings { mode: Mode::Reduced, scale: 1.0 };
        let a = transform(&reduced, ms(0)).unwrap();
        assert!(a.is_finished(ms(150)));
        assert!(!a.sample(ms(75), frame()).moves());
        let off = AnimationSettings { mode: Mode::Off, scale: 1.0 };
        assert!(transform(&off, ms(0)).is_none());
        let zero = AnimationSettings { mode: Mode::Full, scale: 0.0 };
        assert!(transform(&zero, ms(0)).is_none());
    }

    #[test]
    fn slow_motion_stretches_it() {
        let slow = AnimationSettings { mode: Mode::Full, scale: 10.0 };
        let a = transform(&slow, ms(0)).unwrap();
        assert!(!a.is_finished(ms(5999)));
        assert!(a.is_finished(ms(6000)));
    }
}
