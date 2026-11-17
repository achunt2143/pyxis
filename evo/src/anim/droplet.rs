//! Droplet (minimize) and Rise (restore): the pose maths. Pure functions over
//! numbers; the payload and the draw are [`super::warp`], the program
//! [`super::lumen`]. Spec: `docs/internal/evo-animation-plan.md`, "Lumen".
//!
//! # The shape
//!
//! One number drives everything, the **progress** `p` toward the dock icon,
//! 0 at the window's frame and 1 at the icon. It is read off lane B's minimize
//! spring ([`progress`]), so a Droplet inherits the spring's counters, stagger
//! and above all its retargeting: a restore that interrupts a minimize turns
//! the *same* spring round from its value and velocity, and the pose, being a
//! function of `p` (and of the spring's speed, which is continuous too),
//! cannot jump. A Rise is the same pose with `p` running back to 0.
//!
//! - **Shape**: the size goes to the icon's short side (a square) and the
//!   corner radii, each from the window's own (the waterdrop's four), to half
//!   of it (a circle) over the first 60%; the size
//!   interpolates geometrically, so it shrinks evenly to the eye.
//! - **Path**: the centre travels a quadratic arc from the frame's centre to
//!   the icon's, its control 40% of the way across and [`ARC_LIFT`] above the
//!   higher of the two, eased in and out (a smoothstep in `p`).
//! - **Light**: blur up to [`BLUR_MAX`], accent tint and glow up to 1 by
//!   about 50%.
//! - **Squash and stretch** along the velocity, up to [`STRETCH_MAX`] : its
//!   reciprocal, scaled by how orb-like the shape already is (a window-sized
//!   rectangle never stretches).
//! - **Landing**: the last 18% shrinks the orb by 55%, the last 14% fades it.
//!   At [`LAND`] Evo says `MinimizeLanded`.
//!
//! # Guarantees (tests)
//!
//! `p = 0` is exactly the frame (its size, centre, the frame's corners,
//! square when maximized, sharp, untinted, opaque, unstretched); `p = 1` is centred on the
//! icon, no bigger than it, invisible. The radius never exceeds half the short
//! side, the stretch stays within bounds, and every quantity is continuous in
//! `p`, so a retarget (continuous in `p` and its rate) cannot jump.

use smithay::utils::{Logical, Rectangle};

use super::minimize::plan;
use crate::shell::identity::Radii;

/// The shape is the icon's circle by this progress.
pub const MORPH_END: f64 = 0.6;
/// Most blur, in logical pixels.
pub const BLUR_MAX: f64 = 6.0;
/// The arc's control point: this far across, this far above the higher end
/// (logical pixels).
pub const ARC_ACROSS: f64 = 0.4;
pub const ARC_LIFT: f64 = 70.0;
/// Squash and stretch at most this : its reciprocal.
pub const STRETCH_MAX: f64 = 1.3;
/// The speed (logical px/s) at which the stretch is at its most.
pub const STRETCH_SPEED: f64 = 2400.0;
/// The landing: shrink by `SHRINK` from `SHRINK_FROM`, fade from `FADE_FROM`.
pub const SHRINK_FROM: f64 = 0.82;
pub const SHRINK: f64 = 0.55;
pub const FADE_FROM: f64 = 0.86;
/// `MinimizeLanded {phase: "land"}` goes out here.
pub const LAND: f64 = 0.9;
/// The glow's width round the orb, logical pixels.
pub const GLOW_RADIUS: f64 = 12.0;

/// A rectangle as plain numbers (any unit; the caller picks one).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn from_logical(r: Rectangle<i32, Logical>) -> Self {
        Self::new(r.loc.x as f64, r.loc.y as f64, r.size.w as f64, r.size.h as f64)
    }

    pub fn center(&self) -> [f64; 2] {
        [self.x + self.w / 2.0, self.y + self.h / 2.0]
    }
}

pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

pub fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The progress from the minimize spring's scale: 0 at the plain frame, 1
/// when the scale has reached `end` (the plan's `scale.y`). Clamped, since
/// the spring is slightly under-damped and overshoots.
pub fn progress(scale_y: f64, end_scale_y: f64) -> f64 {
    let span = 1.0 - end_scale_y;
    if span.abs() < 1e-9 {
        return 0.0;
    }
    ((1.0 - scale_y) / span).clamp(0.0, 1.0)
}

/// [`progress`] for `frame` heading into `target`, given the pose's current
/// scale (the same plan lane B's springs aim at).
pub fn progress_for(scale_y: f64, frame: Rectangle<i32, Logical>, target: Rectangle<i32, Logical>) -> f64 {
    progress(scale_y, plan(frame, target).scale.y)
}

/// How much of the window's drop shadow is left: it fades with the shape
/// morph, early, since it is drawn at the frame and the shape leaves it.
pub fn shadow_keep(p: f64) -> f64 {
    1.0 - smoothstep(0.0, 0.15, p)
}

/// The arc's control point for a trip from `a` to `b`, `lift` above the
/// higher end.
pub fn control(a: [f64; 2], b: [f64; 2], lift: f64) -> [f64; 2] {
    [lerp(a[0], b[0], ARC_ACROSS), a[1].min(b[1]) - lift]
}

/// The quadratic arc at `s`, and its derivative in `s`.
pub fn arc(a: [f64; 2], c: [f64; 2], b: [f64; 2], s: f64) -> ([f64; 2], [f64; 2]) {
    let u = 1.0 - s;
    let point = [
        u * u * a[0] + 2.0 * u * s * c[0] + s * s * b[0],
        u * u * a[1] + 2.0 * u * s * c[1] + s * s * b[1],
    ];
    let tangent = [
        2.0 * u * (c[0] - a[0]) + 2.0 * s * (b[0] - c[0]),
        2.0 * u * (c[1] - a[1]) + 2.0 * s * (b[1] - c[1]),
    ];
    (point, tangent)
}

/// How far along the arc the centre is at progress `p` (eased in and out),
/// and its rate `ds/dp`.
pub fn travel(p: f64) -> (f64, f64) {
    let p = p.clamp(0.0, 1.0);
    (p * p * (3.0 - 2.0 * p), 6.0 * p * (1.0 - p))
}

/// How orb-like the shape is: 0 the window, 1 the icon's circle.
pub fn morph(p: f64) -> f64 {
    smoothstep(0.0, MORPH_END, p)
}

/// The stretch for a shape `orb`-like moving at `speed` (px/s), `full_at`
/// being the speed of the most stretch. Always within `[1, STRETCH_MAX]`.
pub fn stretch_for(orb: f64, speed: f64, full_at: f64) -> f64 {
    if !speed.is_finite() || full_at <= 0.0 {
        return 1.0;
    }
    1.0 + (STRETCH_MAX - 1.0) * orb.clamp(0.0, 1.0) * (speed.abs() / full_at).clamp(0.0, 1.0)
}

/// One frame of a Droplet or Rise, in the units of the rects given.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub center: [f64; 2],
    pub size: [f64; 2],
    pub radii: Radii,
    pub blur: f64,
    pub tint: f64,
    pub glow: f64,
    pub glow_radius: f64,
    pub alpha: f64,
    pub stretch: f64,
    /// Of the stretch axis (the velocity's direction), radians.
    pub angle: f64,
}

/// The pose of `frame` heading into `icon` at progress `p`, the spring
/// moving at `dp_dt` (progress per second, either sign). `px` is the size of
/// a logical pixel in the rects' units (the output scale for physical rects),
/// for the fixed lengths of the spec. `corners` are the window's own, logical
/// (the waterdrop, or square for a maximized window): each goes to the orb's
/// radius on its own.
pub fn pose(frame: Rect, icon: Rect, p: f64, dp_dt: f64, px: f64, corners: Radii) -> Pose {
    let p = p.clamp(0.0, 1.0);
    let m = morph(p);
    let d = icon.w.min(icon.h).max(1.0);
    let (fw, fh) = (frame.w.max(1.0), frame.h.max(1.0));
    // Geometric: equal ratios in equal steps.
    let shrink = 1.0 - SHRINK * smoothstep(SHRINK_FROM, 1.0, p);
    let w = fw * (d / fw).powf(m) * shrink;
    let h = fh * (d / fh).powf(m) * shrink;
    let half_short = w.min(h) / 2.0;
    let radii = corners
        .scale(px)
        .fit(fw, fh)
        .mix(Radii::uniform(half_short), m)
        .clamp_to(half_short);

    let a = frame.center();
    let b = icon.center();
    let c = control(a, b, ARC_LIFT * px);
    let (s, ds_dp) = travel(p);
    let (center, tangent) = arc(a, c, b, s);
    let tlen = tangent[0].hypot(tangent[1]);
    let speed = tlen * ds_dp * dp_dt.abs();
    let (stretch, angle) = if tlen > 1e-6 {
        (stretch_for(m, speed, STRETCH_SPEED * px), tangent[1].atan2(tangent[0]))
    } else {
        (1.0, 0.0)
    };

    Pose {
        center,
        size: [w, h],
        radii,
        blur: BLUR_MAX * px * smoothstep(0.0, MORPH_END, p),
        tint: smoothstep(0.05, 0.5, p),
        glow: smoothstep(0.1, 0.5, p),
        glow_radius: GLOW_RADIUS * px,
        alpha: 1.0 - smoothstep(FADE_FROM, 1.0, p),
        stretch,
        angle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win() -> Rect {
        Rect::new(200.0, 80.0, 800.0, 600.0)
    }

    fn icons() -> [(&'static str, Rect); 3] {
        [
            ("below", Rect::new(900.0, 740.0, 48.0, 48.0)),
            ("left", Rect::new(8.0, 380.0, 48.0, 48.0)),
            ("right", Rect::new(1224.0, 380.0, 48.0, 48.0)),
        ]
    }

    #[test]
    fn p0_is_exactly_the_frame() {
        for (name, icon) in icons() {
            let q = pose(win(), icon, 0.0, 3.0, 1.0, Radii::FRAME);
            assert_eq!(q.center, [600.0, 380.0], "{name}");
            assert_eq!(q.size, [800.0, 600.0], "{name}");
            assert_eq!(q.radii, Radii::FRAME, "the frame's corners");
            assert_eq!((q.blur, q.tint, q.glow, q.alpha, q.stretch), (0.0, 0.0, 0.0, 1.0, 1.0));
        }
    }

    #[test]
    fn a_maximized_window_starts_square_and_still_lands_round() {
        let icon = icons()[0].1;
        assert!(pose(win(), icon, 0.0, 0.0, 1.0, Radii::SQUARE).radii.is_square());
        let q = pose(win(), icon, 1.0, 0.0, 1.0, Radii::SQUARE);
        assert_eq!(q.radii, Radii::uniform(q.size[0] / 2.0));
    }

    #[test]
    fn the_sharp_corner_stays_sharper_on_the_way() {
        let icon = icons()[0].1;
        let q = pose(win(), icon, 0.2, 0.0, 1.0, Radii::FRAME);
        assert!(q.radii.top_right() < q.radii.top_left());
        assert_eq!(q.radii.top_left(), q.radii.bottom_right());
        // Scaled by the output: physical corners.
        assert_eq!(pose(win(), icon, 0.0, 0.0, 2.0, Radii::FRAME).radii, Radii::FRAME.scale(2.0));
    }

    #[test]
    fn p1_is_the_icon_landed_and_gone() {
        for (name, icon) in icons() {
            let q = pose(win(), icon, 1.0, 0.0, 1.0, Radii::FRAME);
            let c = icon.center();
            assert!((q.center[0] - c[0]).abs() < 1e-9 && (q.center[1] - c[1]).abs() < 1e-9, "{name}");
            assert!(q.size[0] <= icon.w && q.size[1] <= icon.h);
            assert!((q.size[0] - q.size[1]).abs() < 1e-9, "a circle");
            assert_eq!(q.radii, Radii::uniform(q.size[0] / 2.0));
            assert_eq!(q.alpha, 0.0);
            assert_eq!((q.tint, q.glow), (1.0, 1.0));
        }
    }

    #[test]
    fn a_circle_by_60_percent() {
        let icon = icons()[0].1;
        let q = pose(win(), icon, MORPH_END, 0.0, 1.0, Radii::FRAME);
        assert!((q.size[0] - 48.0).abs() < 1e-9 && (q.size[1] - 48.0).abs() < 1e-9);
        assert_eq!(q.radii, Radii::uniform(24.0));
        // Tint and glow are full by about half way.
        let half = pose(win(), icon, 0.5, 0.0, 1.0, Radii::FRAME);
        assert_eq!((half.tint, half.glow), (1.0, 1.0));
        assert!(half.blur > 0.0 && half.blur <= BLUR_MAX);
    }

    #[test]
    fn the_radius_never_exceeds_half_the_short_side_and_stretch_is_bounded() {
        for (_, icon) in icons() {
            for frame in [win(), Rect::new(100.0, 100.0, 40.0, 500.0), Rect::new(0.0, 0.0, 20.0, 20.0)] {
                for i in 0..=200 {
                    let p = i as f64 / 200.0;
                    for v in [0.0, 1.0, 10.0, 1e4, -50.0] {
                        let q = pose(frame, icon, p, v, 1.5, Radii::FRAME);
                        assert!(q.radii.max() <= q.size[0].min(q.size[1]) / 2.0 + 1e-9, "p={p}");
                        assert!(q.radii.0.iter().all(|r| *r >= 0.0));
                        assert!((1.0..=STRETCH_MAX + 1e-12).contains(&q.stretch), "p={p} v={v}: {}", q.stretch);
                        assert!((0.0..=1.0).contains(&q.alpha));
                    }
                }
            }
        }
    }

    #[test]
    fn a_window_sized_shape_never_stretches_an_orb_does() {
        let icon = icons()[0].1;
        let fast = 20.0;
        assert_eq!(pose(win(), icon, 0.0, fast, 1.0, Radii::FRAME).stretch, 1.0);
        let mid = pose(win(), icon, 0.5, fast, 1.0, Radii::FRAME);
        assert!(mid.stretch > 1.1, "{}", mid.stretch);
        // Along the path: the axis is the tangent's direction (down-right here).
        assert!(mid.angle > 0.0 && mid.angle < std::f64::consts::PI);
        // At rest, no stretch.
        assert_eq!(pose(win(), icon, 0.5, 0.0, 1.0, Radii::FRAME).stretch, 1.0);
    }

    #[test]
    fn the_arc_rises_above_the_higher_end() {
        // An icon level with the window's centre: the path goes up and over.
        let icon = icons()[2].1;
        let top = (0..=100)
            .map(|i| pose(win(), icon, i as f64 / 100.0, 0.0, 1.0, Radii::FRAME).center[1])
            .fold(f64::MAX, f64::min);
        assert!(top < 380.0 - 20.0, "rises: {top}");
    }

    #[test]
    fn every_quantity_is_continuous_in_p() {
        // So a retarget, which keeps p and its rate, cannot jump.
        for (name, icon) in icons() {
            let step = 1e-4;
            let mut prev = pose(win(), icon, 0.0, 2.0, 1.0, Radii::FRAME);
            for i in 1..=10_000 {
                let p = i as f64 * step;
                let q = pose(win(), icon, p, 2.0, 1.0, Radii::FRAME);
                let dc = (q.center[0] - prev.center[0]).hypot(q.center[1] - prev.center[1]);
                assert!(dc < 1.0, "{name} centre jumps at p={p}: {dc}");
                assert!((q.size[0] - prev.size[0]).abs() < 1.0 && (q.size[1] - prev.size[1]).abs() < 1.0);
                for k in 0..4 {
                    assert!((q.radii.0[k] - prev.radii.0[k]).abs() < 1.0);
                }
                assert!((q.alpha - prev.alpha).abs() < 0.01 && (q.tint - prev.tint).abs() < 0.01);
                assert!((q.stretch - prev.stretch).abs() < 0.01, "{name} stretch jumps at p={p}");
                prev = q;
            }
        }
    }

    #[test]
    fn progress_reads_the_spring() {
        assert_eq!(progress(1.0, 0.08), 0.0);
        assert_eq!(progress(0.08, 0.08), 1.0);
        assert!((progress(0.54, 0.08) - 0.5).abs() < 1e-9);
        assert_eq!(progress(0.07, 0.08), 1.0, "overshoot clamps");
        assert_eq!(progress(0.5, 1.0), 0.0, "degenerate");
        assert_eq!(shadow_keep(0.0), 1.0);
        assert_eq!(shadow_keep(0.2), 0.0);
    }
}
