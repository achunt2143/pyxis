//! Per-window animation state: `TransformAnim` (the moving values),
//! `WindowAnim` (one running animation of a kind) and the counters.
//!
//! Counters are by RAII: a `WindowAnim` counts as started when it is made and
//! completed when it is dropped, however it ended (ran out, replaced by a
//! retarget to a new kind, its window destroyed). So `started == completed`
//! and `active == 0` whenever nothing is in flight, which is what the on-device
//! suite asserts.

use std::{
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};

use smithay::utils::{Logical, Point, Rectangle};

use super::{
    curve::{Animation, Timing, Vec2, LINEAR, STANDARD},
    settings::AnimationSettings,
    spec,
    transform::WindowTransform,
};

/// The kinds the counters distinguish (the IPC contract's keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimKind {
    Open,
    Close,
    Minimize,
    Restore,
    Geometry,
}

impl AnimKind {
    pub const ALL: [AnimKind; 5] = [
        AnimKind::Open,
        AnimKind::Close,
        AnimKind::Minimize,
        AnimKind::Restore,
        AnimKind::Geometry,
    ];

    pub fn name(self) -> &'static str {
        match self {
            AnimKind::Open => "open",
            AnimKind::Close => "close",
            AnimKind::Minimize => "minimize",
            AnimKind::Restore => "restore",
            AnimKind::Geometry => "geometry",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Default)]
pub struct AnimStats {
    started: [AtomicU64; 5],
    completed: [AtomicU64; 5],
    active: AtomicU32,
}

impl AnimStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn started(&self, kind: AnimKind) -> u64 {
        self.started[kind.index()].load(Ordering::Relaxed)
    }

    pub fn completed(&self, kind: AnimKind) -> u64 {
        self.completed[kind.index()].load(Ordering::Relaxed)
    }

    pub fn active(&self) -> u32 {
        self.active.load(Ordering::Relaxed)
    }

    /// Count `kind` as started; it completes when the guard is dropped.
    pub fn begin(self: &Arc<Self>, kind: AnimKind) -> ActiveGuard {
        self.started[kind.index()].fetch_add(1, Ordering::Relaxed);
        self.active.fetch_add(1, Ordering::Relaxed);
        ActiveGuard { stats: Arc::clone(self), kind }
    }
}

/// The process-wide counters, shared with the Echo thread.
pub fn stats() -> &'static Arc<AnimStats> {
    static STATS: OnceLock<Arc<AnimStats>> = OnceLock::new();
    STATS.get_or_init(AnimStats::new)
}

#[derive(Debug)]
pub struct ActiveGuard {
    stats: Arc<AnimStats>,
    kind: AnimKind,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.stats.completed[self.kind.index()].fetch_add(1, Ordering::Relaxed);
        self.stats.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The fixed point of a window's scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Origin {
    /// A fraction of the window's frame: (0.5, 0.5) is its centre. Resolved
    /// against the frame at draw time, so a window that is still being placed
    /// scales about where it actually is.
    Frame(f64, f64),
    /// A point in output-logical coordinates, such as a dock icon.
    Point(Point<f64, Logical>),
}

/// The values that make up a `WindowTransform`, each its own `Animation` so
/// each can be retargeted (with its velocity) independently.
#[derive(Clone, Debug, PartialEq)]
pub struct TransformAnim {
    pub origin: Origin,
    pub scale: Animation<Vec2>,
    pub offset: Animation<Vec2>,
    pub alpha: Animation<f64>,
}

/// Where a retarget is heading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformTarget {
    pub scale: Vec2,
    pub offset: Vec2,
    pub alpha: f64,
}

impl TransformTarget {
    pub const IDENTITY: TransformTarget = TransformTarget {
        scale: Vec2::splat(1.0),
        offset: Vec2::splat(0.0),
        alpha: 1.0,
    };
}

impl TransformAnim {
    pub fn is_finished(&self, now: Duration) -> bool {
        self.scale.is_finished(now) && self.offset.is_finished(now) && self.alpha.is_finished(now)
    }

    /// The pose at `now`. `frame` is the window's frame in output-logical
    /// coordinates, which `Origin::Frame` is taken against.
    pub fn sample(&self, now: Duration, frame: Rectangle<i32, Logical>) -> WindowTransform {
        let origin = match self.origin {
            Origin::Frame(fx, fy) => Point::from((
                frame.loc.x as f64 + frame.size.w as f64 * fx,
                frame.loc.y as f64 + frame.size.h as f64 * fy,
            )),
            Origin::Point(p) => p,
        };
        WindowTransform {
            origin,
            scale: self.scale.value(now),
            offset: self.offset.value(now),
            alpha: self.alpha.value(now).clamp(0.0, 1.0) as f32,
        }
    }

    /// Turn every value toward `target` from where it is, keeping velocity
    /// (springs) or value (curves). The origin stays.
    pub fn retarget(&mut self, now: Duration, target: TransformTarget) {
        self.scale.retarget(now, target.scale);
        self.offset.retarget(now, target.offset);
        self.alpha.retarget(now, target.alpha);
    }

    fn opacity_only(from: f64, to: f64, now: Duration, duration: Duration, origin: Origin) -> Self {
        Self {
            origin,
            scale: Animation::fixed(Vec2::splat(1.0)),
            offset: Animation::fixed(Vec2::splat(0.0)),
            alpha: Animation::new(from, to, now, Timing::curve(duration, LINEAR)),
        }
    }

    /// The open animation, or `None` when settings say not to animate.
    /// `launch` is the icon the user clicked, when known: the window then
    /// grows out of it instead of out of its own centre.
    pub fn open(
        settings: &AnimationSettings,
        now: Duration,
        launch: Option<Rectangle<i32, Logical>>,
    ) -> Option<Self> {
        let duration = settings.duration(spec::OPEN)?;
        if !settings.motion() {
            return Some(Self::opacity_only(0.0, 1.0, now, duration, Origin::Frame(0.5, 0.5)));
        }
        let curve = spec::OPEN_CURVE;
        let (origin, rise) = match launch {
            Some(r) => (
                Origin::Point(Point::from((
                    r.loc.x as f64 + r.size.w as f64 / 2.0,
                    r.loc.y as f64 + r.size.h as f64 / 2.0,
                ))),
                0.0,
            ),
            None => (Origin::Frame(0.5, 0.5), spec::OPEN_RISE_PX),
        };
        Some(Self {
            origin,
            scale: Animation::new(
                Vec2::splat(spec::OPEN_SCALE_FROM),
                Vec2::splat(1.0),
                now,
                Timing::curve(duration, curve),
            ),
            offset: Animation::new(
                Vec2::new(0.0, rise),
                Vec2::splat(0.0),
                now,
                Timing::curve(duration, curve),
            ),
            alpha: Animation::new(
                0.0,
                1.0,
                now,
                Timing::curve(duration.mul_f64(spec::OPEN_ALPHA_FRACTION), curve),
            ),
        })
    }

    /// Bloom's transform (Lumen's open; `anim::bloom` carries the rest of its
    /// look): scale 0.90 -> 1 about the frame's centre on a ζ 0.86 spring,
    /// rising 16 px on expo out, alpha done by 35%. Reduced is the same
    /// opacity-only fade as `open`; off is `None`.
    pub fn bloom(settings: &AnimationSettings, now: Duration) -> Option<Self> {
        let duration = settings.duration(spec::BLOOM)?;
        if !settings.motion() {
            return Some(Self::opacity_only(0.0, 1.0, now, duration, Origin::Frame(0.5, 0.5)));
        }
        Some(Self {
            origin: Origin::Frame(0.5, 0.5),
            scale: Animation::new(
                Vec2::splat(spec::BLOOM_SCALE_FROM),
                Vec2::splat(1.0),
                now,
                Timing::spring(spec::bloom_spring(duration), spec::REST_NORMALISED),
            ),
            offset: Animation::new(
                Vec2::new(0.0, spec::BLOOM_RISE_PX),
                Vec2::splat(0.0),
                now,
                Timing::curve(duration, spec::OPEN_CURVE),
            ),
            alpha: Animation::new(
                0.0,
                1.0,
                now,
                Timing::curve(duration.mul_f64(spec::BLOOM_ALPHA_FRACTION), STANDARD),
            ),
        })
    }

    /// The close animation of a snapshot.
    pub fn close(settings: &AnimationSettings, now: Duration) -> Option<Self> {
        let duration = settings.duration(spec::CLOSE)?;
        if !settings.motion() {
            return Some(Self::opacity_only(1.0, 0.0, now, duration, Origin::Frame(0.5, 0.5)));
        }
        let timing = Timing::curve(duration, spec::CLOSE_CURVE);
        Some(Self {
            origin: Origin::Frame(0.5, 0.5),
            scale: Animation::new(Vec2::splat(1.0), Vec2::splat(spec::CLOSE_SCALE_TO), now, timing),
            offset: Animation::fixed(Vec2::splat(0.0)),
            alpha: Animation::new(1.0, 0.0, now, timing),
        })
    }

    /// A popup's fade in (scaling about its top-left, the anchor corner of a
    /// menu that opens down and right).
    pub fn popup_in(settings: &AnimationSettings, now: Duration) -> Option<Self> {
        let duration = settings.duration(spec::POPUP_IN)?;
        if !settings.motion() {
            return Some(Self::opacity_only(0.0, 1.0, now, duration, Origin::Frame(0.0, 0.0)));
        }
        let timing = Timing::curve(duration, spec::POPUP_IN_CURVE);
        Some(Self {
            origin: Origin::Frame(0.0, 0.0),
            scale: Animation::new(Vec2::splat(spec::POPUP_SCALE_FROM), Vec2::splat(1.0), now, timing),
            offset: Animation::fixed(Vec2::splat(0.0)),
            alpha: Animation::new(0.0, 1.0, now, timing),
        })
    }
}

/// One running animation of a window: what it is, its values, and its count.
#[derive(Debug)]
pub struct WindowAnim {
    pub kind: AnimKind,
    pub anim: TransformAnim,
    /// The genie warp this minimize or restore is drawn as, when it has one
    /// (see `anim::warp`). The `TransformAnim` still drives it: the warp's
    /// progress is read off the spring's scale, and it is also what is drawn
    /// when the warp can't be (genie-lite).
    pub warp: Option<Box<super::warp::Warp>>,
    /// Bloom's blur, tint, corners and glow, when this open is drawn as Bloom
    /// (see `anim::bloom`); the `TransformAnim` is its scale, rise and alpha.
    pub bloom: Option<super::bloom::BloomFx>,
    /// The picture a show desktop's Recede, or its Emerge, is drawn from
    /// through Lumen (see `anim::recede`); the `TransformAnim` is its motion.
    pub recede: Option<Box<super::recede::RecedeFx>>,
    _guard: ActiveGuard,
}

impl WindowAnim {
    pub fn new(kind: AnimKind, anim: TransformAnim) -> Self {
        Self::with_stats(stats(), kind, anim)
    }

    pub fn with_stats(stats: &Arc<AnimStats>, kind: AnimKind, anim: TransformAnim) -> Self {
        Self { kind, anim, warp: None, bloom: None, recede: None, _guard: stats.begin(kind) }
    }

    /// Draw this animation as a genie warp of `warp`'s snapshot.
    pub fn with_warp(mut self, warp: Option<Box<super::warp::Warp>>) -> Self {
        self.warp = warp;
        self
    }

    /// Draw this minimize or restore as a Recede or Emerge of `fx`'s picture.
    pub fn with_recede(mut self, fx: Option<Box<super::recede::RecedeFx>>) -> Self {
        self.recede = fx;
        self
    }

    /// Draw this open as Bloom.
    pub fn with_bloom(mut self, bloom: Option<super::bloom::BloomFx>) -> Self {
        self.bloom = bloom;
        self
    }

    /// The pose at `now`, or `None` once it has finished (the caller then
    /// drops it, which completes it).
    pub fn sample(&self, now: Duration, frame: Rectangle<i32, Logical>) -> Option<WindowTransform> {
        (!self.anim.is_finished(now)).then(|| self.anim.sample(now, frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::settings::{AnimationSettings, Mode};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn frame() -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((100, 100)), (800, 600).into())
    }

    fn full() -> AnimationSettings {
        AnimationSettings::default()
    }

    #[test]
    fn open_grows_from_092_and_rises_then_lands_on_identity() {
        let a = TransformAnim::open(&full(), ms(1000), None).unwrap();
        let t0 = a.sample(ms(1000), frame());
        assert_eq!(t0.scale, Vec2::splat(spec::OPEN_SCALE_FROM));
        assert_eq!(t0.offset, Vec2::new(0.0, spec::OPEN_RISE_PX));
        assert_eq!(t0.alpha, 0.0);
        assert_eq!(t0.origin, Point::from((500.0, 400.0)), "the frame's centre");
        let end = ms(1000) + spec::OPEN;
        assert!(a.is_finished(end));
        assert!(a.sample(end, frame()).is_identity());
    }

    #[test]
    fn open_alpha_is_done_by_sixty_percent() {
        let a = TransformAnim::open(&full(), ms(0), None).unwrap();
        let at = spec::OPEN.mul_f64(0.6);
        assert_eq!(a.sample(at, frame()).alpha, 1.0);
        assert!(a.sample(at, frame()).scale.x < 1.0, "scale is still settling");
    }

    #[test]
    fn open_from_a_launch_icon_scales_about_it_without_rising() {
        let icon = Rectangle::new(Point::from((10, 740)), (48, 48).into());
        let a = TransformAnim::open(&full(), ms(0), Some(icon)).unwrap();
        let t = a.sample(ms(0), frame());
        assert_eq!(t.origin, Point::from((34.0, 764.0)));
        assert_eq!(t.offset, Vec2::splat(0.0));
    }

    #[test]
    fn origin_follows_the_frame_as_it_moves() {
        let a = TransformAnim::open(&full(), ms(0), None).unwrap();
        let moved = Rectangle::new(Point::from((0, 0)), (800, 600).into());
        assert_eq!(a.sample(ms(10), moved).origin, Point::from((400.0, 300.0)));
    }

    #[test]
    fn bloom_grows_from_090_rises_16_and_is_opaque_by_35_percent() {
        let a = TransformAnim::bloom(&full(), ms(1000)).unwrap();
        let t0 = a.sample(ms(1000), frame());
        assert_eq!(t0.scale, Vec2::splat(spec::BLOOM_SCALE_FROM));
        assert_eq!(t0.offset, Vec2::new(0.0, spec::BLOOM_RISE_PX));
        assert_eq!(t0.alpha, 0.0);
        assert_eq!(t0.origin, Point::from((500.0, 400.0)));
        let opaque = ms(1000) + spec::BLOOM.mul_f64(spec::BLOOM_ALPHA_FRACTION);
        assert_eq!(a.sample(opaque, frame()).alpha, 1.0);
        let end = ms(1000) + spec::BLOOM;
        assert!(a.is_finished(end), "lands within its ~420 ms");
        assert!(a.sample(end, frame()).is_identity());
        assert!(!a.is_finished(ms(1000) + spec::BLOOM.mul_f64(0.9)), "the rise runs the whole way");
    }

    #[test]
    fn bloom_overshoots_barely_if_at_all() {
        let a = TransformAnim::bloom(&full(), ms(0)).unwrap();
        for i in 0..=84 {
            let s = a.sample(ms(i * 5), frame()).scale.x;
            assert!(s <= 1.002, "scale {s} at {} ms", i * 5);
        }
    }

    #[test]
    fn bloom_reduced_is_the_plain_fade_and_off_is_none() {
        let reduced = AnimationSettings { mode: Mode::Reduced, scale: 1.0 };
        let a = TransformAnim::bloom(&reduced, ms(0)).unwrap();
        assert!(!a.sample(ms(60), frame()).moves());
        assert!(a.is_finished(ms(150)));
        let off = AnimationSettings { mode: Mode::Off, scale: 1.0 };
        assert!(TransformAnim::bloom(&off, ms(0)).is_none());
    }

    #[test]
    fn close_shrinks_to_094_and_fades_out() {
        let a = TransformAnim::close(&full(), ms(0)).unwrap();
        let t0 = a.sample(ms(0), frame());
        assert_eq!((t0.scale, t0.alpha), (Vec2::splat(1.0), 1.0));
        let end = a.sample(spec::CLOSE - ms(1), frame());
        assert!(end.alpha < 0.05 && end.scale.x < 0.95);
        assert!(a.is_finished(spec::CLOSE));
        assert_eq!(a.sample(spec::CLOSE, frame()).alpha, 0.0);
    }

    #[test]
    fn reduced_is_opacity_only_and_capped() {
        let reduced = AnimationSettings { mode: Mode::Reduced, scale: 1.0 };
        let a = TransformAnim::open(&reduced, ms(0), None).unwrap();
        let mid = a.sample(ms(60), frame());
        assert_eq!(mid.scale, Vec2::splat(1.0));
        assert_eq!(mid.offset, Vec2::splat(0.0));
        assert!(mid.alpha > 0.0 && mid.alpha < 1.0);
        assert!(a.is_finished(ms(150)), "open is capped at 150 ms in reduced mode");
        let c = TransformAnim::close(&reduced, ms(0)).unwrap();
        assert!(!c.sample(ms(50), frame()).moves());
    }

    #[test]
    fn off_makes_nothing() {
        let off = AnimationSettings { mode: Mode::Off, scale: 1.0 };
        assert!(TransformAnim::open(&off, ms(0), None).is_none());
        assert!(TransformAnim::close(&off, ms(0)).is_none());
        assert!(TransformAnim::popup_in(&off, ms(0)).is_none());
        let zero = AnimationSettings { mode: Mode::Full, scale: 0.0 };
        assert!(TransformAnim::open(&zero, ms(0), None).is_none());
    }

    #[test]
    fn scale_stretches_durations() {
        let slow = AnimationSettings { mode: Mode::Full, scale: 10.0 };
        let a = TransformAnim::open(&slow, ms(0), None).unwrap();
        assert!(!a.is_finished(ms(2599)));
        assert!(a.is_finished(ms(2600)));
    }

    #[test]
    fn state_machine_counts_start_and_completion_with_injected_time() {
        let stats = AnimStats::new();
        let mut slot = Some(WindowAnim::with_stats(
            &stats,
            AnimKind::Open,
            TransformAnim::open(&full(), ms(0), None).unwrap(),
        ));
        assert_eq!((stats.started(AnimKind::Open), stats.completed(AnimKind::Open), stats.active()), (1, 0, 1));

        // Mid-flight it samples, and nothing completes.
        assert!(slot.as_ref().unwrap().sample(ms(100), frame()).is_some());
        assert_eq!(stats.active(), 1);

        // Past the end it yields nothing; the owner drops it, which completes.
        assert!(slot.as_ref().unwrap().sample(spec::OPEN, frame()).is_none());
        slot = None;
        assert!(slot.is_none());
        assert_eq!((stats.started(AnimKind::Open), stats.completed(AnimKind::Open), stats.active()), (1, 1, 0));
        assert_eq!(stats.started(AnimKind::Close), 0);
    }

    #[test]
    fn replacing_an_animation_completes_the_old_one() {
        let stats = AnimStats::new();
        let mut slot = Some(WindowAnim::with_stats(
            &stats,
            AnimKind::Open,
            TransformAnim::open(&full(), ms(0), None).unwrap(),
        ));
        slot = Some(WindowAnim::with_stats(
            &stats,
            AnimKind::Minimize,
            TransformAnim::close(&full(), ms(50)).unwrap(),
        ));
        assert_eq!(stats.active(), 1);
        assert_eq!(stats.completed(AnimKind::Open), 1, "interrupted counts as completed");
        drop(slot);
        assert_eq!(stats.active(), 0);
        for kind in AnimKind::ALL {
            assert_eq!(stats.started(kind), stats.completed(kind));
        }
    }

    #[test]
    fn retargeting_mid_open_turns_it_round_without_a_jump() {
        let mut a = TransformAnim::open(&full(), ms(0), None).unwrap();
        let before = a.sample(ms(80), frame());
        a.retarget(ms(80), TransformTarget { scale: Vec2::splat(0.5), offset: Vec2::splat(0.0), alpha: 0.0 });
        let after = a.sample(ms(80), frame());
        assert_eq!(before.scale, after.scale);
        assert_eq!(before.alpha, after.alpha);
        assert_eq!(a.sample(ms(80) + spec::OPEN * 2, frame()).scale, Vec2::splat(0.5));
    }

    #[test]
    fn popup_in_fades_and_scales_from_097_about_its_corner() {
        let a = TransformAnim::popup_in(&full(), ms(0)).unwrap();
        let popup = Rectangle::new(Point::from((300, 200)), (120, 200).into());
        let t = a.sample(ms(0), popup);
        assert_eq!(t.origin, Point::from((300.0, 200.0)));
        assert_eq!(t.scale, Vec2::splat(spec::POPUP_SCALE_FROM));
        assert_eq!(t.alpha, 0.0);
        assert!(a.is_finished(spec::POPUP_IN));
    }
}
