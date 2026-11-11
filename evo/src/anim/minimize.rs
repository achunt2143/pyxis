//! Minimize, restore and "close from where it is": the poses and the
//! constructors. Pure maths over `TransformAnim`; the part that knows about
//! windows and the Space is `shell/minimize.rs`.
//!
//! # The pose
//!
//! A minimizing window moves from its frame into a dock icon's rect. One
//! scale and one offset can't give two axes different timing (each is a single
//! `Animation`), so the travel is split to make **x lead y** ("genie-lite"):
//!
//! - the *offset* carries the horizontal travel, on a slightly faster spring;
//! - the *scale* carries the shrink on the main spring, and the vertical
//!   travel with it, by scaling about a virtual fixed point `origin` chosen so
//!   the frame's centre lands on the icon's centre when the scale arrives
//!   (see [`plan`]). The window is sucked down as it narrows.
//! - alpha is a curve over the last 30% of the run (first 30% on restore).
//!
//! # Interrupting
//!
//! [`retarget`] turns any running animation (open, restore, a minimize going
//! the other way) toward a new pose from where it is now, with its velocity:
//! it samples the old pose, re-expresses it about the new origin (so no point
//! of the window jumps) and starts new springs from there. Nothing is queued.
//! [`close_from`] does the same for a close that interrupts an open, so the
//! snapshot starts from the pose the window had.

use std::time::Duration;

use smithay::utils::{Logical, Point, Rectangle};

use super::{
    curve::{Animation, Animatable, Timing, Vec2, LINEAR},
    settings::AnimationSettings,
    spec,
    window::{Origin, TransformAnim},
};

/// The x spring settles in this fraction of the main spring's time.
pub const LEAD: f64 = 0.82;
/// Alpha's share of a minimize's run: it fades over the last 30%, and over
/// the first 30% of a restore.
pub const FADE_FRACTION: f64 = 0.3;
/// A window never shrinks below this fraction of itself (a degenerate target).
const MIN_SCALE: f64 = 0.02;
/// And a window smaller than its icon doesn't grow to it.
const MAX_END_SCALE: f64 = 0.9;

/// The spring's settling time when the run is drawn as a Lumen Droplet or
/// Rise (`anim::droplet`): the spring is front-loaded, so a 480 ms *read*
/// (the orb landed and faded) needs a longer envelope than genie-lite's.
pub const DROPLET: Duration = Duration::from_millis(720);
pub const RISE: Duration = Duration::from_millis(680);

/// The base duration of a run toward `toward`: Lumen's when it will draw it,
/// lane B's otherwise (genie-lite).
pub fn base_duration(toward: Toward) -> Duration {
    let lumen = super::lumen::usable();
    match (toward, lumen) {
        (Toward::Icon, true) => DROPLET,
        (Toward::Frame, true) => RISE,
        (Toward::Icon, false) => spec::MINIMIZE,
        (Toward::Frame, false) => spec::RESTORE,
    }
}

/// Where a window is heading, and the pose it gets there in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    pub origin: Point<f64, Logical>,
    pub scale: Vec2,
    pub offset: Vec2,
}

/// The fully minimized pose for `frame` heading into `target`. Both are in
/// output-logical coordinates.
pub fn plan(frame: Rectangle<i32, Logical>, target: Rectangle<i32, Logical>) -> Plan {
    let (fw, fh) = (frame.size.w.max(1) as f64, frame.size.h.max(1) as f64);
    let (tw, th) = (target.size.w.max(1) as f64, target.size.h.max(1) as f64);
    let fc = (frame.loc.x as f64 + fw / 2.0, frame.loc.y as f64 + fh / 2.0);
    let tc = (target.loc.x as f64 + tw / 2.0, target.loc.y as f64 + th / 2.0);
    let sx = (tw / fw).clamp(MIN_SCALE, MAX_END_SCALE);
    let sy = (th / fh).clamp(MIN_SCALE, MAX_END_SCALE);
    // Scaling about (ox, oy) by sy moves the centre's y to oy + (fc.y - oy)·sy;
    // choose oy so that is the target's centre. x travels by the offset, so
    // its fixed point is the frame's own centre.
    let oy = (tc.1 - fc.1 * sy) / (1.0 - sy);
    Plan {
        origin: Point::from((fc.0, oy)),
        scale: Vec2::new(sx, sy),
        offset: Vec2::new(tc.0 - fc.0, 0.0),
    }
}

/// Which way a [`retarget`] is heading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toward {
    /// Into the icon.
    Icon,
    /// Back to the window's frame.
    Frame,
}

fn fade_only(from: f64, to: f64, start: Duration, duration: Duration, origin: Origin) -> TransformAnim {
    TransformAnim {
        origin,
        scale: Animation::fixed(Vec2::splat(1.0)),
        offset: Animation::fixed(Vec2::splat(0.0)),
        alpha: Animation::new(from, to, start, Timing::curve(duration, LINEAR)),
    }
}

fn timings(duration: Duration) -> (Timing, Timing) {
    (
        Timing::spring(spec::minimize_spring(duration), spec::REST_NORMALISED),
        Timing::spring(spec::minimize_spring(duration.mul_f64(LEAD)), spec::REST_PIXELS),
    )
}

/// The longest any of the springs runs: what alpha is timed against, so all
/// three finish together.
fn run_of(scale: &Animation<Vec2>, offset: &Animation<Vec2>) -> Duration {
    scale.duration().max(offset.duration())
}

/// Assemble the springs and alpha from the pose the window is in now
/// (`from_*`, with velocity) to `end`, starting at `start`.
#[allow(clippy::too_many_arguments)]
fn assemble(
    toward: Toward,
    origin: Point<f64, Logical>,
    from_scale: Vec2,
    from_scale_v: Vec2,
    from_offset: Vec2,
    from_offset_v: Vec2,
    from_alpha: f64,
    end: (Vec2, Vec2),
    start: Duration,
    duration: Duration,
) -> TransformAnim {
    let (scale_timing, offset_timing) = timings(duration);
    let scale = Animation::with_velocity(from_scale, end.0, from_scale_v, start, scale_timing);
    let offset = Animation::with_velocity(from_offset, end.1, from_offset_v, start, offset_timing);
    let run = run_of(&scale, &offset);
    let alpha = match toward {
        Toward::Icon => Animation::new(
            from_alpha,
            0.0,
            start + run.mul_f64(1.0 - FADE_FRACTION),
            Timing::curve(run.mul_f64(FADE_FRACTION), LINEAR),
        ),
        Toward::Frame => Animation::new(
            from_alpha,
            1.0,
            start,
            Timing::curve(run.mul_f64(FADE_FRACTION), LINEAR),
        ),
    };
    TransformAnim { origin: Origin::Point(origin), scale, offset, alpha }
}

/// Minimize from the plain pose: the frame into `target`. `start` is `now`
/// plus any stagger. `None` when settings say not to animate. In reduced mode
/// it is an opacity fade in place.
pub fn minimize(
    settings: &AnimationSettings,
    start: Duration,
    frame: Rectangle<i32, Logical>,
    target: Rectangle<i32, Logical>,
) -> Option<TransformAnim> {
    let duration = settings.duration(base_duration(Toward::Icon))?;
    if !settings.motion() {
        return Some(fade_only(1.0, 0.0, start, duration, Origin::Frame(0.5, 0.5)));
    }
    let p = plan(frame, target);
    Some(assemble(
        Toward::Icon,
        p.origin,
        Vec2::splat(1.0),
        Vec2::splat(0.0),
        Vec2::splat(0.0),
        Vec2::splat(0.0),
        1.0,
        (p.scale, p.offset),
        start,
        duration,
    ))
}

/// Restore from the icon: the reverse of [`minimize`], starting from the
/// fully minimized pose.
pub fn restore(
    settings: &AnimationSettings,
    start: Duration,
    frame: Rectangle<i32, Logical>,
    target: Rectangle<i32, Logical>,
) -> Option<TransformAnim> {
    let duration = settings.duration(base_duration(Toward::Frame))?;
    if !settings.motion() {
        return Some(fade_only(0.0, 1.0, start, duration, Origin::Frame(0.5, 0.5)));
    }
    let p = plan(frame, target);
    Some(assemble(
        Toward::Frame,
        p.origin,
        p.scale,
        Vec2::splat(0.0),
        p.offset,
        Vec2::splat(0.0),
        0.0,
        (Vec2::splat(1.0), Vec2::splat(0.0)),
        start,
        duration,
    ))
}

/// Turn `prev`, whatever it is, toward the icon or back to the frame, from
/// its current pose and velocity. The pose is carried over exactly: it is
/// re-expressed about the new origin, so no point of the window jumps when
/// the origin changes (an open scales about the centre, a minimize about a
/// point off toward the icon).
pub fn retarget(
    prev: &TransformAnim,
    now: Duration,
    start: Duration,
    toward: Toward,
    settings: &AnimationSettings,
    frame: Rectangle<i32, Logical>,
    target: Rectangle<i32, Logical>,
) -> Option<TransformAnim> {
    let duration = settings.duration(base_duration(toward))?;
    let alpha = prev.alpha.value(now);
    if !settings.motion() {
        let to = if toward == Toward::Icon { 0.0 } else { 1.0 };
        return Some(fade_only(alpha, to, start, duration, Origin::Frame(0.5, 0.5)));
    }
    let o1 = match prev.origin {
        Origin::Frame(fx, fy) => Point::from((
            frame.loc.x as f64 + frame.size.w as f64 * fx,
            frame.loc.y as f64 + frame.size.h as f64 * fy,
        )),
        Origin::Point(p) => p,
    };
    let p = plan(frame, target);
    let o2 = p.origin;
    let (s, sv) = (prev.scale.value(now), prev.scale.velocity(now));
    let (d, dv) = (prev.offset.value(now), prev.offset.velocity(now));
    // d2 = (o1 - o2)(1 - s) + d,   d2' = d' - (o1 - o2) s'
    let (ex, ey) = (o1.x - o2.x, o1.y - o2.y);
    let d2 = Vec2::new(ex * (1.0 - s.x) + d.x, ey * (1.0 - s.y) + d.y);
    let dv2 = Vec2::new(dv.x - ex * sv.x, dv.y - ey * sv.y);
    let end = match toward {
        Toward::Icon => (p.scale, p.offset),
        Toward::Frame => (Vec2::splat(1.0), Vec2::splat(0.0)),
    };
    Some(assemble(toward, o2, s, sv, d2, dv2, alpha, end, start, duration))
}

/// A close that starts from `prev`'s current pose instead of the plain one:
/// the snapshot of a window that was still opening (or restoring) shrinks and
/// fades from the size and opacity it had, instead of jumping to full size.
pub fn close_from(
    prev: &TransformAnim,
    now: Duration,
    settings: &AnimationSettings,
) -> Option<TransformAnim> {
    let duration = settings.duration(spec::CLOSE)?;
    let alpha = prev.alpha.value(now);
    if !settings.motion() {
        return Some(fade_only(alpha, 0.0, now, duration, prev.origin));
    }
    let timing = Timing::curve(duration, spec::CLOSE_CURVE);
    let s = prev.scale.value(now);
    let d = prev.offset.value(now);
    Some(TransformAnim {
        origin: prev.origin,
        scale: Animation::new(s, s.scale(spec::CLOSE_SCALE_TO), now, timing),
        offset: Animation::fixed(d),
        alpha: Animation::new(alpha, 0.0, now, timing),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::settings::Mode;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }
    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }
    fn frame() -> Rectangle<i32, Logical> {
        rect(200, 80, 800, 600)
    }
    fn icon() -> Rectangle<i32, Logical> {
        rect(900, 740, 48, 48)
    }
    fn full() -> AnimationSettings {
        AnimationSettings::default()
    }

    /// Where the frame's centre is drawn under `a` at `t`.
    fn centre(a: &TransformAnim, t: Duration, f: Rectangle<i32, Logical>) -> (f64, f64) {
        let p = a.sample(t, f);
        let c = (f.loc.x as f64 + f.size.w as f64 / 2.0, f.loc.y as f64 + f.size.h as f64 / 2.0);
        (
            p.origin.x + (c.0 - p.origin.x) * p.scale.x + p.offset.x,
            p.origin.y + (c.1 - p.origin.y) * p.scale.y + p.offset.y,
        )
    }

    fn size_at(a: &TransformAnim, t: Duration, f: Rectangle<i32, Logical>) -> (f64, f64) {
        let p = a.sample(t, f);
        (f.size.w as f64 * p.scale.x, f.size.h as f64 * p.scale.y)
    }

    #[test]
    fn the_plan_lands_the_frame_centre_on_the_icon_centre() {
        let p = plan(frame(), icon());
        let end = TransformAnim {
            origin: Origin::Point(p.origin),
            scale: Animation::fixed(p.scale),
            offset: Animation::fixed(p.offset),
            alpha: Animation::fixed(0.0),
        };
        let (x, y) = centre(&end, ms(0), frame());
        assert!((x - 924.0).abs() < 1e-6, "x {x}");
        assert!((y - 764.0).abs() < 1e-6, "y {y}");
        assert!((p.scale.x - 0.06).abs() < 1e-9 && (p.scale.y - 0.08).abs() < 1e-9);
    }

    #[test]
    fn degenerate_targets_do_not_blow_up() {
        // A zero-size target shrinks to the floor, a tiny window to its own size.
        let p = plan(frame(), rect(10, 10, 0, 0));
        assert!(p.scale.x >= MIN_SCALE && p.origin.y.is_finite());
        let q = plan(rect(0, 0, 20, 20), icon());
        assert!(q.scale.x <= MAX_END_SCALE && q.origin.y.is_finite());
    }

    #[test]
    fn minimize_starts_plain_ends_at_the_icon_and_fades_in_the_last_30_percent() {
        let a = minimize(&full(), ms(1000), frame(), icon()).unwrap();
        let t0 = a.sample(ms(1000), frame());
        assert_eq!((t0.scale, t0.alpha), (Vec2::splat(1.0), 1.0));
        let (x0, y0) = centre(&a, ms(1000), frame());
        assert!((x0 - 600.0).abs() < 1e-6 && (y0 - 380.0).abs() < 1e-6, "starts on the frame's centre");

        let run = run_of(&a.scale, &a.offset);
        assert!(run > ms(60) && run < ms(800), "run {run:?}");
        let end = ms(1000) + run;
        assert!(a.is_finished(end));
        let (xe, ye) = centre(&a, end, frame());
        assert!((xe - 924.0).abs() < 0.01 && (ye - 764.0).abs() < 0.01, "ends on the icon: {xe},{ye}");
        let (w, h) = size_at(&a, end, frame());
        assert!((w - 48.0).abs() < 0.5 && (h - 48.0).abs() < 0.5, "icon-sized: {w}x{h}");
        assert_eq!(a.sample(end, frame()).alpha, 0.0);

        // Fully opaque until 70% of the run, gone by the end.
        assert_eq!(a.sample(ms(1000) + run.mul_f64(0.69), frame()).alpha, 1.0);
        let mid = a.sample(ms(1000) + run.mul_f64(0.85), frame()).alpha;
        assert!(mid > 0.0 && mid < 1.0, "fading: {mid}");
    }

    #[test]
    fn x_leads_y() {
        // Progress along each axis, as a fraction of the way to the icon.
        let a = minimize(&full(), ms(0), frame(), icon()).unwrap();
        let (x0, y0) = (600.0, 380.0);
        let (xe, ye) = (924.0, 764.0);
        let mut led = 0;
        for i in 1..20 {
            let t = ms(i * 10);
            let (x, y) = centre(&a, t, frame());
            let (px, py) = ((x - x0) / (xe - x0), (y - y0) / (ye - y0));
            if px > py + 1e-6 {
                led += 1;
            }
        }
        assert!(led >= 10, "x was ahead of y in only {led} of 19 samples");
    }

    #[test]
    fn restore_is_the_reverse() {
        let a = restore(&full(), ms(0), frame(), icon()).unwrap();
        let t0 = a.sample(ms(0), frame());
        assert_eq!(t0.alpha, 0.0);
        assert!((t0.scale.x - 0.06).abs() < 1e-9);
        let (x, y) = centre(&a, ms(0), frame());
        assert!((x - 924.0).abs() < 1e-6 && (y - 764.0).abs() < 1e-6);
        let run = run_of(&a.scale, &a.offset);
        // Opaque early, and lands on the frame.
        assert_eq!(a.sample(run.mul_f64(0.31), frame()).alpha, 1.0);
        assert!(a.is_finished(run));
        assert!(a.sample(run, frame()).is_identity() || a.sample(run, frame()).scale == Vec2::splat(1.0));
        let (xe, ye) = centre(&a, run, frame());
        assert!((xe - 600.0).abs() < 0.01 && (ye - 380.0).abs() < 0.01);
    }

    #[test]
    fn stagger_holds_the_start_pose() {
        let a = minimize(&full(), ms(500), frame(), icon()).unwrap();
        assert!(!a.is_finished(ms(480)));
        assert!(a.sample(ms(480), frame()).scale == Vec2::splat(1.0), "still plain before its turn");
        assert_eq!(a.sample(ms(480), frame()).alpha, 1.0);
        assert!(a.sample(ms(540), frame()).scale.x < 1.0, "moving once its turn comes");
    }

    #[test]
    fn restoring_mid_minimize_continues_from_the_pose_and_velocity() {
        let m = minimize(&full(), ms(0), frame(), icon()).unwrap();
        let t = ms(90);
        let before = m.sample(t, frame());
        let (bx, by) = centre(&m, t, frame());
        let (vs, vo) = (m.scale.velocity(t), m.offset.velocity(t));
        let r = retarget(&m, t, t, Toward::Frame, &full(), frame(), icon()).unwrap();
        let after = r.sample(t, frame());
        assert!((before.scale.x - after.scale.x).abs() < 1e-9 && (before.scale.y - after.scale.y).abs() < 1e-9);
        assert!((before.alpha - after.alpha).abs() < 1e-6);
        let (ax, ay) = centre(&r, t, frame());
        assert!((ax - bx).abs() < 1e-6 && (ay - by).abs() < 1e-6, "no jump: ({bx},{by}) -> ({ax},{ay})");
        // The scale spring carries its velocity through.
        assert!((r.scale.velocity(t).x - vs.x).abs() < 1e-6);
        assert!((r.scale.velocity(t).y - vs.y).abs() < 1e-6);
        let _ = vo;
        // And it lands on the frame.
        let run = run_of(&r.scale, &r.offset);
        let (ex, ey) = centre(&r, t + run, frame());
        assert!((ex - 600.0).abs() < 0.01 && (ey - 380.0).abs() < 0.01);
        assert_eq!(r.sample(t + run.mul_f64(0.31), frame()).alpha, 1.0);
    }

    #[test]
    fn any_point_of_the_window_is_continuous_across_a_retarget() {
        // The origin changes between an open (about the centre) and a minimize
        // (about a point toward the icon); a corner must not jump.
        let o = TransformAnim::open(&full(), ms(0), None).unwrap();
        let t = ms(70);
        let corner = |a: &TransformAnim| {
            let p = a.sample(t, frame());
            let c = (frame().loc.x as f64, frame().loc.y as f64);
            (
                p.origin.x + (c.0 - p.origin.x) * p.scale.x + p.offset.x,
                p.origin.y + (c.1 - p.origin.y) * p.scale.y + p.offset.y,
            )
        };
        let m = retarget(&o, t, t, Toward::Icon, &full(), frame(), icon()).unwrap();
        let (a, b) = (corner(&o), corner(&m));
        assert!((a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6, "{a:?} vs {b:?}");
        assert!((o.alpha.value(t) - m.sample(t, frame()).alpha as f64).abs() < 1e-6);
    }

    #[test]
    fn minimize_mid_restore_turns_around_and_fades_out_at_the_end() {
        let r = restore(&full(), ms(0), frame(), icon()).unwrap();
        let t = ms(70);
        let m = retarget(&r, t, t, Toward::Icon, &full(), frame(), icon()).unwrap();
        let run = run_of(&m.scale, &m.offset);
        assert!(m.is_finished(t + run));
        assert_eq!(m.sample(t + run, frame()).alpha, 0.0);
        let (x, y) = centre(&m, t + run, frame());
        assert!((x - 924.0).abs() < 0.01 && (y - 764.0).abs() < 0.01);
    }

    #[test]
    fn reduced_is_a_fade_in_place_capped_at_150ms() {
        let reduced = AnimationSettings { mode: Mode::Reduced, scale: 1.0 };
        let a = minimize(&reduced, ms(0), frame(), icon()).unwrap();
        let mid = a.sample(ms(60), frame());
        assert!(!mid.moves() && mid.alpha < 1.0 && mid.alpha > 0.0);
        assert!(a.is_finished(ms(150)));
        let r = restore(&reduced, ms(0), frame(), icon()).unwrap();
        assert!(r.is_finished(ms(150)) && !r.sample(ms(10), frame()).moves());
        // A retarget in reduced mode fades from the alpha it has.
        let back = retarget(&a, ms(60), ms(60), Toward::Frame, &reduced, frame(), icon()).unwrap();
        assert!((back.sample(ms(60), frame()).alpha - mid.alpha).abs() < 1e-6);
        assert!(back.is_finished(ms(60) + ms(150)));
    }

    #[test]
    fn off_makes_nothing() {
        let off = AnimationSettings { mode: Mode::Off, scale: 1.0 };
        assert!(minimize(&off, ms(0), frame(), icon()).is_none());
        assert!(restore(&off, ms(0), frame(), icon()).is_none());
        let a = minimize(&full(), ms(0), frame(), icon()).unwrap();
        assert!(retarget(&a, ms(10), ms(10), Toward::Frame, &off, frame(), icon()).is_none());
        assert!(close_from(&a, ms(10), &off).is_none());
    }

    #[test]
    fn scale_stretches_a_minimize() {
        let slow = AnimationSettings { mode: Mode::Full, scale: 10.0 };
        let a = minimize(&slow, ms(0), frame(), icon()).unwrap();
        let fast = minimize(&full(), ms(0), frame(), icon()).unwrap();
        assert!(run_of(&a.scale, &a.offset) > run_of(&fast.scale, &fast.offset) * 5);
    }

    #[test]
    fn a_close_mid_open_starts_from_the_current_pose() {
        let o = TransformAnim::open(&full(), ms(0), None).unwrap();
        let t = ms(60);
        let pose = o.sample(t, frame());
        assert!(pose.scale.x < 1.0 && pose.alpha > 0.0 && pose.alpha < 1.0, "mid-open");
        let c = close_from(&o, t, &full()).unwrap();
        let start = c.sample(t, frame());
        assert_eq!(start.scale, pose.scale, "no jump to full size");
        assert_eq!(start.offset, pose.offset);
        assert!((start.alpha - pose.alpha).abs() < 1e-6);
        assert_eq!(start.origin, pose.origin);
        // It shrinks further and fades out, ending invisible.
        let end = c.sample(t + spec::CLOSE, frame());
        assert_eq!(end.alpha, 0.0);
        assert!(end.scale.x < pose.scale.x);
        assert!(c.is_finished(t + spec::CLOSE));
    }

    #[test]
    fn a_close_of_a_settled_window_matches_the_plain_close() {
        let done = TransformAnim::open(&full(), ms(0), None).unwrap();
        let c = close_from(&done, spec::OPEN + ms(1), &full()).unwrap();
        let s = c.sample(spec::OPEN + ms(1), frame());
        assert_eq!((s.scale, s.alpha), (Vec2::splat(1.0), 1.0));
        let end = c.sample(spec::OPEN + ms(1) + spec::CLOSE - ms(1), frame());
        assert!(end.scale.x < 0.95 && end.alpha < 0.05);
    }

    #[test]
    fn a_droplet_reads_about_480ms_and_lands_at_90_percent_early() {
        // The orb has landed and faded (p >= 0.99) about 480 ms in, on the
        // envelope DROPLET gives the spring (independent of the Lumen switch).
        let p = plan(frame(), icon());
        let a = assemble(
            Toward::Icon,
            p.origin,
            Vec2::splat(1.0),
            Vec2::splat(0.0),
            Vec2::splat(0.0),
            Vec2::splat(0.0),
            1.0,
            (p.scale, p.offset),
            ms(0),
            DROPLET,
        );
        let reach = |th: f64| {
            (0..3000u64)
                .find(|&t| crate::anim::droplet::progress(a.scale.value(ms(t)).y, p.scale.y) >= th)
                .unwrap()
        };
        let read = reach(0.99);
        assert!((440..=520).contains(&read), "reads in {read} ms");
        assert!(reach(crate::anim::droplet::LAND) < read);
    }
}
