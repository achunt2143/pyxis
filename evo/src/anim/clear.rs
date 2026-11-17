//! Lumen's Clear: the screen unlocks, and the desktop and every window come
//! into focus about the field the password was typed in.
//! Spec: `docs/internal/pyxis-glass-and-lock-plan.md`, Part 2 and Contracts.
//!
//! - 900 ms. Blur 14 -> 0 px and scale 1.04 -> 1 about the origin
//!   (`SetLockOrigin`, default the screen's centre), both on
//!   `f = 1 - (1 - min(1, t / 0.85))^3`: done at 85% of the run. The
//!   circular reveal is Bloom's (a hole in its opaque lock layer, with a ring);
//!   Evo draws the whole desktop and does not mask.
//! - **Drawn on the live surfaces, not on snapshots.** An unlock has no
//!   snapshot to hand: the windows kept committing while hidden, and a
//!   snapshot per window would be a GPU readback on the very frame the user is
//!   waiting for (Droplet and Recede take theirs a frame ahead of a drawn
//!   animation, which a lock cannot). Bloom's open already blurs live
//!   surfaces with a texture shader (`bloom::apply`, `BloomSurface`), so Clear
//!   is that same machinery with a different look (`BloomFx::clear`: blur
//!   only, no tint, no glow, the window's own corners) and a `TransformAnim`
//!   about the origin for the scale. The cost is the one Bloom's open
//!   measured at nothing: one full-screen pass.
//! - Reduced: windows fade in over at most 150 ms, no blur, no scale. Off:
//!   instant. With Lumen off or Bloom's shader failed: the scale alone.
//! - The `TransformAnim`'s alpha is held at 1 for the whole 900 ms so the
//!   animation (and its RAII count) lives as long as Bloom's ring does, though
//!   the picture is sharp from 85%.

use std::time::Duration;

use smithay::utils::{Logical, Point};

use super::{
    curve::{Animation, CubicBezier, Timing, Vec2, LINEAR},
    settings::AnimationSettings,
    spec,
    window::{Origin, TransformAnim},
};

/// CSS's easeOutCubic as a bezier: the closest the curve type gets to
/// `1 - (1 - x)^3` (within about 0.01 of it).
pub const CUBIC_OUT: CubicBezier = CubicBezier::new(0.215, 0.61, 0.355, 1.0);

/// `f`: how much of the focus pull is done at run progress `t` (0..1).
pub fn pull_done(t: f64) -> f64 {
    let x = (t / spec::CLEAR_PULL_FRACTION).clamp(0.0, 1.0);
    1.0 - (1.0 - x).powi(3)
}

/// The blur (logical px) at run progress `t`.
pub fn blur_at(t: f64) -> f64 {
    spec::CLEAR_BLUR_PX * (1.0 - pull_done(t))
}

/// The windows' pose: scale 1.04 -> 1 about `origin` (output-logical), or
/// `None` when settings say off. Reduced: a fade in, no scale.
pub fn transform(
    settings: &AnimationSettings,
    now: Duration,
    origin: Point<f64, Logical>,
) -> Option<TransformAnim> {
    let duration = settings.duration(spec::CLEAR)?;
    if !settings.motion() {
        return Some(TransformAnim {
            origin: Origin::Frame(0.5, 0.5),
            scale: Animation::fixed(Vec2::splat(1.0)),
            offset: Animation::fixed(Vec2::splat(0.0)),
            alpha: Animation::new(0.0, 1.0, now, Timing::curve(duration, LINEAR)),
        });
    }
    Some(TransformAnim {
        origin: Origin::Point(origin),
        scale: Animation::new(
            Vec2::splat(spec::CLEAR_SCALE_FROM),
            Vec2::splat(1.0),
            now,
            Timing::curve(duration.mul_f64(spec::CLEAR_PULL_FRACTION), CUBIC_OUT),
        ),
        offset: Animation::fixed(Vec2::splat(0.0)),
        // Held at 1 for the whole run: see the module header.
        alpha: Animation::new(1.0, 1.0, now, Timing::curve(duration, LINEAR)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::settings::Mode;
    use smithay::utils::Rectangle;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn frame() -> Rectangle<i32, Logical> {
        Rectangle::new((200, 100).into(), (800, 600).into())
    }

    fn full() -> AnimationSettings {
        AnimationSettings::default()
    }

    #[test]
    fn the_pull_is_the_contracts_formula() {
        assert_eq!(pull_done(0.0), 0.0);
        assert_eq!(pull_done(0.85), 1.0);
        assert_eq!(pull_done(1.0), 1.0, "held after 85%");
        let t: f64 = 0.4;
        let want = 1.0 - (1.0 - t / 0.85).powi(3);
        assert!((pull_done(t) - want).abs() < 1e-12);
        assert_eq!(blur_at(0.0), 14.0);
        assert_eq!(blur_at(0.9), 0.0);
        let mut last = f64::INFINITY;
        for i in 0..=100 {
            let b = blur_at(i as f64 / 100.0);
            assert!(b <= last);
            last = b;
        }
    }

    #[test]
    fn windows_scale_from_104_about_the_origin_and_land_on_identity() {
        let origin = Point::from((640.0, 700.0));
        let a = transform(&full(), ms(1000), origin).unwrap();
        let t0 = a.sample(ms(1000), frame());
        assert_eq!(t0.scale, Vec2::splat(1.04));
        assert_eq!(t0.origin, origin, "the field's centre, not the window's");
        assert_eq!(t0.alpha, 1.0);
        let end = ms(1000) + spec::CLEAR;
        assert!(a.is_finished(end));
        assert!(!a.is_finished(ms(1000) + spec::CLEAR.mul_f64(0.99)), "lives as long as Bloom's ring");
        assert!(a.sample(end, frame()).is_identity());
    }

    #[test]
    fn the_scale_tracks_the_pull_closely() {
        let a = transform(&full(), ms(0), Point::from((0.0, 0.0))).unwrap();
        for i in 0..=18 {
            let t = i as f64 / 20.0;
            let s = a.sample(spec::CLEAR.mul_f64(t), frame()).scale.x;
            let want = 1.0 + 0.04 * (1.0 - pull_done(t));
            assert!((s - want).abs() < 0.001, "t={t}: {s} vs {want}");
        }
    }

    #[test]
    fn it_never_goes_under_one_or_over_104() {
        let a = transform(&full(), ms(0), Point::from((0.0, 0.0))).unwrap();
        for i in 0..=200 {
            let s = a.sample(ms(i * 5), frame()).scale.x;
            assert!((0.9999..=1.0401).contains(&s), "{s}");
        }
    }

    #[test]
    fn reduced_fades_in_and_off_is_instant() {
        let reduced = AnimationSettings { mode: Mode::Reduced, scale: 1.0 };
        let a = transform(&reduced, ms(0), Point::from((0.0, 0.0))).unwrap();
        let mid = a.sample(ms(75), frame());
        assert!(mid.alpha > 0.0 && mid.alpha < 1.0);
        assert!(!mid.moves());
        assert!(a.is_finished(ms(150)));
        let off = AnimationSettings { mode: Mode::Off, scale: 1.0 };
        assert!(transform(&off, ms(0), Point::from((0.0, 0.0))).is_none());
    }
}
