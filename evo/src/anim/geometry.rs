//! Geometry transitions: maximize, unmaximize, fullscreen in and out, and fit
//! to the work area. Design: `docs/internal/evo-animation-plan.md`, lane G.
//!
//! # The technique
//!
//! A window changes frame between two commits of the *client*: Evo asks for a
//! new size (a configure), and Gecko answers by acknowledging at once but only
//! painting the new size a frame or more later. So a transition has two
//! phases, and the spring starts at the second:
//!
//! 1. **Waiting.** Before the configure goes out, `begin_geometry_transition`
//!    (`shell/geometry_anim.rs`) snapshots the window as it is, SSD header
//!    included. Until the new size lands the snapshot is drawn at the old frame
//!    and the live window is not drawn at all: it is already at its final
//!    *place* (placement is instant, per `anim/mod.rs`) but still has the old
//!    *size*, which looks like a jump. It still gets frame callbacks (they go to
//!    every window in the Space), which is what lets it paint.
//! 2. **Landed.** The frame the window has when its size changes (or after
//!    [`PENDING_LIMIT`], if the client never resizes: a refusal, a minimum size)
//!    is the target. A spring (ζ = 1, about 280 ms) carries the *visible rect*
//!    from the old frame to it. Both the snapshot and the live window are drawn
//!    scaled to that rect: the snapshot opaque underneath, the live window
//!    fading in on top of it over the first 40% (an under-layer fade, so
//!    coverage never dips the way a symmetric crossfade's does), after which
//!    the snapshot is freed.
//!
//! Only scaled textures are drawn, never a re-laid-out header, so no text is
//! re-rasterized per frame: the live header was drawn once at its final width.
//!
//! # Interruption
//!
//! The visible rect is two springs (location and size, in pixels), so
//! retargeting keeps both value and velocity. Anything that changes the live
//! frame while a transition runs retargets it at the next frame (a second
//! `fit`, a late-landing size). A *new* request while one runs
//! (maximize then unmaximize) takes a fresh snapshot of the live window,
//! goes back to Waiting, and the springs keep moving on the way: from the
//! current rect and velocity, nothing jumps.
//!
//! # Modes
//!
//! `reduced`: no scaling. The snapshot stays at the old frame and fades out
//! while the live window fades in at the new one, at most 150 ms. `off`:
//! nothing starts. `GeoMotion` is pure (time and the live frame are injected),
//! so all of this is tested without a renderer.

use std::{cell::RefCell, time::Duration};

use smithay::{
    backend::renderer::{
        element::{texture::TextureRenderElement, Kind},
        ImportAll, ImportMem, Renderer, Texture,
    },
    utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

use super::{
    glint::{self, GlintElement, GlintTrack},
    shader::GlesAccess,
    curve::{Animation, Spring, Timing, Vec2, STANDARD},
    settings::AnimationSettings,
    snapshot::Snapshot,
    spec,
    depth,
    transform::{wrap, wrap_all, Transformed, WindowTransform},
    window::{stats, ActiveGuard, AnimKind},
};
use crate::shell::{identity::Radii, WindowElement, WindowRenderElement};

type Rect = Rectangle<i32, Logical>;

/// How long a transition waits for the client's new size before settling onto
/// whatever it has. Shorter than `placement`'s 2 s settling limit on purpose:
/// that one only delays a re-fit, this one holds a frozen picture on screen.
/// A client that is merely slow still ends up right: a size that lands after
/// this retargets the running spring.
pub const PENDING_LIMIT: Duration = Duration::from_millis(750);

/// Whether the Settle glint is drawn: Lumen on. Reduced and off never get as
/// far as asking (no motion). Lumen's switch is read at draw time, so a
/// transition started with it off still ends when its springs rest.
fn glint_enabled() -> bool {
    super::lumen::enabled()
}

/// A rect in fractional logical pixels: what is drawn mid-flight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FRect {
    pub loc: Vec2,
    pub size: Vec2,
}

impl FRect {
    pub fn of(r: Rect) -> Self {
        Self {
            loc: Vec2::new(r.loc.x as f64, r.loc.y as f64),
            size: Vec2::new(r.size.w as f64, r.size.h as f64),
        }
    }
}

/// The pose that draws a window whose frame is `depicted` as if it were the
/// rect `drawn`: scale about its top-left, then move.
pub fn transform_between(depicted: Rect, drawn: FRect, alpha: f32) -> WindowTransform {
    let (w, h) = (depicted.size.w.max(1) as f64, depicted.size.h.max(1) as f64);
    WindowTransform {
        origin: Point::from((depicted.loc.x as f64, depicted.loc.y as f64)),
        scale: Vec2::new(drawn.size.x / w, drawn.size.y / h),
        offset: Vec2::new(drawn.loc.x - depicted.loc.x as f64, drawn.loc.y - depicted.loc.y as f64),
        alpha,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// A new size has been asked for and has not landed.
    Waiting { stale: Size<i32, Logical>, since: Duration },
    /// The size is in; the live frame at that moment (and since) is `target`.
    Landed { at: Duration, target: Rect },
}

/// What to draw this frame, in output-logical coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoPose {
    /// The snapshot: the frame it depicts, where it is drawn, its alpha.
    pub snapshot: Option<(Rect, FRect, f32)>,
    /// The live window (whose frame is the `live` the pose was asked with):
    /// where it is drawn, its alpha. `None` while waiting.
    pub live: Option<(FRect, f32)>,
}

/// The pure state machine of one transition.
#[derive(Debug)]
pub struct GeoMotion {
    motion: bool,
    spring: Spring,
    /// The spring's nominal duration (mode and scale applied): what the
    /// Settle glint's timing is a fraction of.
    duration: Duration,
    /// How long the live window takes to fade in over the snapshot.
    fade: Duration,
    has_snapshot: bool,
    /// The frame the snapshot depicts.
    depicted: Rect,
    loc: Animation<Vec2>,
    size: Animation<Vec2>,
    phase: Phase,
}

impl GeoMotion {
    /// Start from the visible frame `from`. `wait` is the frame size the
    /// window has *now* when a different one has been asked for (the
    /// transition then waits for it to change); `None` when the size is not
    /// expected to change, which lands at once on `live`. `None` back when the
    /// settings say not to animate, or there would be nothing to draw.
    pub fn start(
        settings: &AnimationSettings,
        now: Duration,
        from: Rect,
        has_snapshot: bool,
        wait: Option<Size<i32, Logical>>,
        live: Rect,
    ) -> Option<Self> {
        let duration = settings.duration(spec::GEOMETRY)?;
        let motion = settings.motion();
        if !motion && !has_snapshot {
            // Reduced motion is a crossfade, which needs the old picture.
            return None;
        }
        let fade = if motion { duration.mul_f64(spec::GEOMETRY_CROSSFADE) } else { duration };
        let mut m = Self {
            motion,
            spring: spec::geometry_spring(duration),
            duration,
            fade,
            has_snapshot,
            depicted: from,
            loc: Animation::fixed(FRect::of(from).loc),
            size: Animation::fixed(FRect::of(from).size),
            phase: Phase::Landed { at: now, target: from },
        };
        match wait {
            Some(stale) => m.phase = Phase::Waiting { stale, since: now },
            None => m.land(now, live),
        }
        Some(m)
    }

    /// A new request while this one runs: the live window is `live` now and
    /// its picture is the new snapshot. The visible rect keeps moving.
    pub fn interrupt(&mut self, now: Duration, live: Rect, wait: Option<Size<i32, Logical>>) {
        self.depicted = live;
        self.has_snapshot = true;
        match wait {
            Some(stale) => self.phase = Phase::Waiting { stale, since: now },
            None => self.land(now, live),
        }
    }

    pub fn is_waiting(&self) -> bool {
        matches!(self.phase, Phase::Waiting { .. })
    }

    fn current(&self, now: Duration) -> FRect {
        let size = self.size.value(now);
        FRect { loc: self.loc.value(now), size: Vec2::new(size.x.max(1.0), size.y.max(1.0)) }
    }

    fn head_for(&mut self, now: Duration, target: Rect) {
        let target = FRect::of(target);
        if self.motion {
            let timing = Timing::spring(self.spring, spec::REST_PIXELS);
            self.loc.retarget_with(now, target.loc, timing);
            self.size.retarget_with(now, target.size, timing);
        } else {
            self.loc = Animation::fixed(target.loc);
            self.size = Animation::fixed(target.size);
        }
    }

    fn land(&mut self, now: Duration, live: Rect) {
        if let Phase::Waiting { stale, since } = self.phase {
            // How long the client took to paint the size it was asked for:
            // the one number the hold on the old picture depends on.
            tracing::info!(
                "geometry transition: new size landed after {} ms ({}x{} -> {}x{})",
                now.saturating_sub(since).as_millis(),
                stale.w,
                stale.h,
                live.size.w,
                live.size.h
            );
        }
        self.head_for(now, live);
        self.phase = Phase::Landed { at: now, target: live };
    }

    /// The Settle glint's progress at `now` (see `anim::glint`): `None` while
    /// waiting, outside its window, or without full motion.
    pub fn glint(&self, now: Duration) -> Option<f64> {
        let Phase::Landed { at, .. } = self.phase else {
            return None;
        };
        if !self.motion {
            return None;
        }
        glint::progress(self.landed_progress(at, now))
    }

    /// How far through the nominal duration the landed phase is.
    fn landed_progress(&self, at: Duration, now: Duration) -> f64 {
        let secs = self.duration.as_secs_f64();
        if secs <= 0.0 {
            return 1.0;
        }
        now.saturating_sub(at).as_secs_f64() / secs
    }

    /// Whether the glint still has to cross: a transition in full motion is
    /// not over until it has, even if its springs came to rest early (a small
    /// fit), so the band never stops half way.
    fn glint_pending(&self, at: Duration, now: Duration) -> bool {
        self.motion && glint_enabled() && self.landed_progress(at, now) < 1.0
    }

    fn fade_progress(&self, at: Duration, now: Duration) -> f64 {
        if !self.has_snapshot {
            return 1.0;
        }
        let secs = self.fade.as_secs_f64();
        if secs <= 0.0 {
            return 1.0;
        }
        (now.saturating_sub(at).as_secs_f64() / secs).clamp(0.0, 1.0)
    }

    /// The pose at `now`, with the window's current frame `live`; `None` once
    /// the transition is over (the caller drops it, which completes it).
    pub fn pose(&mut self, now: Duration, live: Rect) -> Option<GeoPose> {
        if let Phase::Waiting { stale, since } = self.phase {
            if live.size != stale || now.saturating_sub(since) >= PENDING_LIMIT {
                self.land(now, live);
            }
        }
        match self.phase {
            Phase::Waiting { .. } => {
                let drawn = if self.motion { self.current(now) } else { FRect::of(self.depicted) };
                Some(GeoPose { snapshot: Some((self.depicted, drawn, 1.0)), live: None })
            }
            Phase::Landed { at, target } => {
                if live != target {
                    // The frame moved again under us (a re-fit, a size that
                    // landed late): head for it from here, keeping velocity.
                    self.head_for(now, live);
                    self.phase = Phase::Landed { at, target: live };
                }
                let f = self.fade_progress(at, now);
                if f >= 1.0 && self.size.is_finished(now) && self.loc.is_finished(now) && !self.glint_pending(at, now) {
                    return None;
                }
                let drawn = self.current(now);
                let eased = if self.has_snapshot { STANDARD.ease(f) } else { 1.0 };
                let snapshot = (self.has_snapshot && f < 1.0).then(|| {
                    if self.motion {
                        // Opaque underneath the fading-in live window.
                        (self.depicted, drawn, 1.0)
                    } else {
                        // Reduced: a plain crossfade between the two frames.
                        (self.depicted, FRect::of(self.depicted), (1.0 - eased) as f32)
                    }
                });
                Some(GeoPose { snapshot, live: Some((drawn, eased as f32)) })
            }
        }
    }

    /// Whether this can no longer produce a pose, judged by the clock alone:
    /// for a window that is not being drawn (minimized mid-transition), which
    /// would otherwise never be asked.
    pub fn expired(&self, now: Duration) -> bool {
        match self.phase {
            Phase::Waiting { since, .. } => now.saturating_sub(since) > PENDING_LIMIT + Duration::from_millis(100),
            Phase::Landed { at, .. } => {
                self.fade_progress(at, now) >= 1.0
                    && self.size.is_finished(now)
                    && self.loc.is_finished(now)
                    && !self.glint_pending(at, now)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The transition on a window: motion + snapshot + counter.
// ---------------------------------------------------------------------------

pub struct GeoTransition {
    pub motion: GeoMotion,
    snapshot: Option<Snapshot>,
    glint: GlintTrack,
    /// Where the snapshot's window rendered from (its buffer origin), global
    /// logical, when the snapshot was taken.
    render_origin: Point<i32, Logical>,
    _guard: ActiveGuard,
}

impl GeoTransition {
    pub fn new(motion: GeoMotion, snapshot: Option<Snapshot>, render_origin: Point<i32, Logical>) -> Self {
        Self { motion, snapshot, glint: GlintTrack::default(), render_origin, _guard: stats().begin(AnimKind::Geometry) }
    }

    /// Swap in a fresh snapshot of the live window (see `GeoMotion::interrupt`).
    pub fn retake(&mut self, snapshot: Snapshot, render_origin: Point<i32, Logical>) {
        self.snapshot = Some(snapshot);
        self.render_origin = render_origin;
    }
}

#[derive(Default)]
struct GeoSlot(RefCell<Option<GeoTransition>>);

thread_local! {
    /// Windows that may hold a transition, so `sweep` can end the ones nobody
    /// draws. `WindowElement` is not `Send`, and all of this is the
    /// compositor thread's.
    static REGISTRY: RefCell<Vec<WindowElement>> = const { RefCell::new(Vec::new()) };
}

impl WindowElement {
    fn geo_slot(&self) -> &GeoSlot {
        self.user_data().insert_if_missing(GeoSlot::default);
        self.user_data().get::<GeoSlot>().expect("just inserted")
    }

    /// Whether a geometry transition is running (waiting for the client
    /// included).
    pub fn geometry_animating(&self) -> bool {
        self.user_data().get::<GeoSlot>().is_some_and(|s| s.0.borrow().is_some())
    }

    pub fn start_geometry(&self, transition: GeoTransition) {
        *self.geo_slot().0.borrow_mut() = Some(transition);
        REGISTRY.with(|r| {
            let mut r = r.borrow_mut();
            if !r.contains(self) {
                r.push(self.clone());
            }
        });
    }

    /// Stop, completing the transition.
    pub fn cancel_geometry(&self) {
        if let Some(slot) = self.user_data().get::<GeoSlot>() {
            slot.0.borrow_mut().take();
        }
    }

    pub fn with_geometry<T>(&self, f: impl FnOnce(&mut GeoTransition) -> T) -> Option<T> {
        let slot = self.user_data().get::<GeoSlot>()?;
        let mut running = slot.0.borrow_mut();
        running.as_mut().map(f)
    }
}

/// End the transitions that are over, or whose window is gone or not drawn.
/// Run once per frame, after the windows have been drawn (which is what
/// advances the ones that are).
pub fn sweep(now: Duration) {
    REGISTRY.with(|r| {
        r.borrow_mut().retain(|w| {
            if !w.geometry_animating() {
                return false;
            }
            let dead = !smithay::utils::IsAlive::alive(w);
            let over = w.with_geometry(|t| t.motion.expired(now)).unwrap_or(true);
            if dead || over {
                w.cancel_geometry();
                return false;
            }
            true
        })
    });
}

/// How many windows hold a transition.
pub fn count() -> usize {
    REGISTRY.with(|r| r.borrow().iter().filter(|w| w.geometry_animating()).count())
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// What a transitioning window draws this frame, front to back within each
/// list; the live window goes in front of its snapshot.
pub struct Drawn<R: Renderer + ImportAll + ImportMem + GlesAccess> {
    pub live: Vec<Transformed<WindowRenderElement<R>>>,
    pub snapshot: Vec<Transformed<TextureRenderElement<R::TextureId>>>,
    /// The pose that takes the live frame (the `live_frame` given to `draw`)
    /// to the rect being drawn now: what the window's shadow follows, so it
    /// hugs the moving window instead of sitting at the final frame. Full
    /// alpha (the crossfade is the window's, not the shadow's), and while
    /// waiting for the client it is the snapshot's rect.
    pub shadow_pose: WindowTransform,
    /// Lumen's Settle glint over the window (`anim::glint`), drawn in front
    /// of `live`.
    pub glint: Option<GlintElement>,
}

/// Draw `window`'s transition, or `None` if it has none (or it just ended, or
/// its snapshot cannot be drawn: then the window is simply drawn live).
/// `live_frame` is the window's frame now, output-logical; `live_origin` its
/// render origin in physical pixels relative to the output.
pub fn draw<R>(
    window: &WindowElement,
    renderer: &mut R,
    live_frame: Rect,
    live_origin: Point<i32, Physical>,
    output_origin: Point<i32, Logical>,
    scale: Scale<f64>,
    now: Duration,
) -> Option<Drawn<R>>
where
    R: Renderer + ImportAll + ImportMem + GlesAccess,
    R::TextureId: Clone + Texture + Send + 'static,
{
    let slot = window.user_data().get::<GeoSlot>()?;
    let mut running = slot.0.borrow_mut();
    let transition = running.as_mut()?;
    let Some(pose) = transition.motion.pose(now, live_frame) else {
        *running = None;
        return None;
    };
    // The snapshot is done with once the live window is fully in.
    if pose.snapshot.is_none() {
        transition.snapshot = None;
    }

    let mut snapshot_elements = Vec::new();
    if let Some((depicted, drawn, alpha)) = pose.snapshot {
        let texture = transition.snapshot.as_ref().and_then(|snap| snap.texture(renderer));
        let Some(texture) = texture else {
            // No picture to hold the old frame: show the live window.
            *running = None;
            return None;
        };
        let snap = transition.snapshot.as_ref()?;
        let origin = (transition.render_origin - output_origin).to_physical_precise_round(scale);
        let size = snap.size.to_f64().to_logical(scale).to_i32_round();
        let element = TextureRenderElement::from_static_texture(
            snap.id.clone(),
            renderer.context_id(),
            (origin + snap.origin).to_f64(),
            texture,
            1,
            Transform::Normal,
            Some(alpha),
            None,
            Some(size),
            None,
            Kind::Unspecified,
        );
        let phys = transform_between(depicted, drawn, alpha).to_physical(output_origin, scale);
        snapshot_elements.push(wrap(element, phys));
    }

    let live = match pose.live {
        Some((drawn, alpha)) => {
            let elements: Vec<WindowRenderElement<R>> =
                window.frame_render_elements(renderer, live_origin, scale, alpha, true);
            let phys = transform_between(live_frame, drawn, alpha).to_physical(output_origin, scale);
            wrap_all(elements, phys)
        }
        None => Vec::new(),
    };
    let rect = pose.live.map(|(drawn, _)| drawn).or(pose.snapshot.map(|(_, drawn, _)| drawn))?;
    let shadow_pose = transform_between(live_frame, rect, 1.0);
    let glint = match (transition.motion.glint(now), pose.live) {
        (Some(g), Some(_)) if glint_enabled() => glint::program(renderer).and_then(|program| {
            let px = scale.x * (shadow_pose.scale.x + shadow_pose.scale.y) / 2.0;
            let frame = depth::pose_frame(live_frame, Some(&shadow_pose), output_origin, scale.x);
            // The frame's corners as they are now: they square off as a
            // window maximizes (the shadow's appear fades with them) and
            // round again as it leaves.
            transition.glint.element(program, frame, glint_radii(window.is_ssd(), window.shadow_appear(now), px), g, px)
        }),
        _ => None,
    };
    Some(Drawn { live, snapshot: snapshot_elements, shadow_pose, glint })
}

/// The glint's clip: the frame's corners (the waterdrop for an Evo frame, the
/// client's assumed radius otherwise) times `appear`, which is 0 once the
/// window is square, in physical px.
fn glint_radii(ssd: bool, appear: f64, px: f64) -> Radii {
    let base = if ssd { depth::FRAME_RADII } else { Radii::uniform(depth::CLIENT_RADIUS) };
    base.scale(appear * px)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::settings::Mode;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_glint_clips_to_the_waterdrop_and_squares_off_with_the_window() {
        assert_eq!(glint_radii(true, 1.0, 1.0), Radii([16.0, 4.0, 16.0, 16.0]));
        assert_eq!(glint_radii(true, 1.0, 2.0), Radii([32.0, 8.0, 32.0, 32.0]));
        assert!(glint_radii(true, 0.0, 1.0).is_square(), "maximized: square");
        assert_eq!(glint_radii(false, 1.0, 1.0), Radii::uniform(depth::CLIENT_RADIUS));
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn full() -> AnimationSettings {
        AnimationSettings::default()
    }

    fn reduced() -> AnimationSettings {
        AnimationSettings { mode: Mode::Reduced, scale: 1.0 }
    }

    const OLD: fn() -> Rect = || rect(200, 150, 800, 600);
    const NEW: fn() -> Rect = || rect(0, 44, 1280, 700);

    fn waiting(settings: &AnimationSettings, now: Duration) -> GeoMotion {
        GeoMotion::start(settings, now, OLD(), true, Some(OLD().size), OLD()).unwrap()
    }

    fn drawn_live(p: &GeoPose) -> FRect {
        p.live.unwrap().0
    }

    #[test]
    fn transform_between_maps_one_frame_onto_another() {
        let t = transform_between(OLD(), FRect::of(NEW()), 1.0);
        assert_eq!(t.origin, Point::from((200.0, 150.0)));
        assert_eq!(t.scale, Vec2::new(1280.0 / 800.0, 700.0 / 600.0));
        assert_eq!(t.offset, Vec2::new(-200.0, -106.0));
        // Applied to the old frame's top-left corner it lands on the new one's.
        let p = transform_between(OLD(), FRect::of(OLD()), 1.0);
        assert!(p.is_identity());
    }

    #[test]
    fn it_waits_showing_the_old_frame_until_the_size_lands() {
        let mut m = waiting(&full(), ms(0));
        // The window is already at its final place with its old size: that is
        // not drawn, the snapshot is, at the old frame.
        let at_new_place_old_size = rect(0, 44, 800, 600);
        for t in [0, 100, 400] {
            let p = m.pose(ms(t), at_new_place_old_size).unwrap();
            assert!(p.live.is_none(), "live hidden at {t} ms");
            let (depicted, drawn, alpha) = p.snapshot.unwrap();
            assert_eq!((depicted, drawn, alpha), (OLD(), FRect::of(OLD()), 1.0));
        }
        assert!(m.is_waiting());
    }

    #[test]
    fn the_spring_starts_when_the_size_lands_not_before() {
        let mut m = waiting(&full(), ms(0));
        m.pose(ms(100), rect(0, 44, 800, 600));
        // The size lands at 130 ms: the rect is still the old frame there.
        let p = m.pose(ms(130), NEW()).unwrap();
        assert!(!m.is_waiting());
        let live = drawn_live(&p);
        assert!((live.loc.x - 200.0).abs() < 1e-6 && (live.size.x - 800.0).abs() < 1e-6, "{live:?}");
        // And it gets there: monotonic in size, exactly the target at the end.
        let mut last = live.size.x;
        for t in (140..130 + 400).step_by(10) {
            if let Some(p) = m.pose(ms(t), NEW()) {
                let w = drawn_live(&p).size.x;
                assert!(w >= last - 1e-9, "width never shrinks on the way to a bigger frame ({t} ms)");
                assert!(w <= 1280.0 + 1e-6, "critical damping does not overshoot");
                last = w;
            }
        }
        assert!(m.pose(ms(130) + spec::GEOMETRY * 2, NEW()).is_none(), "over");
    }

    #[test]
    fn it_settles_onto_whatever_the_client_committed_when_it_never_resizes() {
        let mut m = waiting(&full(), ms(0));
        let refused = rect(0, 44, 800, 600); // moved, never resized
        assert!(m.pose(PENDING_LIMIT - ms(1), refused).unwrap().live.is_none());
        let p = m.pose(PENDING_LIMIT, refused).unwrap();
        assert!(!m.is_waiting(), "gave up waiting at the limit");
        assert!(p.live.is_some());
        // It ends on the frame the client has, with no stall.
        let end = PENDING_LIMIT + spec::GEOMETRY * 2;
        assert!(m.pose(end, refused).is_none());
        assert!(m.expired(end));
        assert!(PENDING_LIMIT <= Duration::from_secs(2), "inside the settling marker's 2 s");
    }

    #[test]
    fn a_size_that_lands_after_the_limit_retargets_instead_of_snapping() {
        let mut m = waiting(&full(), ms(0));
        let still_old = rect(0, 44, 800, 600);
        m.pose(PENDING_LIMIT + ms(10), still_old); // gave up: heading for the old size
        let before = drawn_live(&m.pose(PENDING_LIMIT + ms(100), still_old).unwrap());
        let after = drawn_live(&m.pose(PENDING_LIMIT + ms(101), NEW()).unwrap());
        assert!((before.size.x - after.size.x).abs() < 20.0, "no jump: {before:?} -> {after:?}");
        assert!(m.pose(PENDING_LIMIT + ms(101) + spec::GEOMETRY * 2, NEW()).is_none());
    }

    #[test]
    fn the_crossfade_covers_the_first_forty_percent() {
        let mut m = waiting(&full(), ms(0));
        m.pose(ms(10), NEW());
        let fade = spec::GEOMETRY.mul_f64(spec::GEOMETRY_CROSSFADE);
        let landed = ms(10);
        // Start: the live window is invisible over an opaque snapshot.
        let p0 = m.pose(landed, NEW()).unwrap();
        assert_eq!(p0.live.unwrap().1, 0.0);
        assert_eq!(p0.snapshot.unwrap().2, 1.0);
        // Half way through the fade: live partly in, snapshot still opaque.
        let mid = m.pose(landed + fade / 2, NEW()).unwrap();
        let a = mid.live.unwrap().1;
        assert!(a > 0.0 && a < 1.0);
        assert_eq!(mid.snapshot.unwrap().2, 1.0, "under-layer: coverage never dips");
        // Its end: live fully in and the snapshot is gone.
        let end = m.pose(landed + fade, NEW()).unwrap();
        assert_eq!(end.live.unwrap().1, 1.0);
        assert!(end.snapshot.is_none(), "snapshot freed at 40%");
        // And alpha only ever rises.
        let mut last = 0.0;
        for t in 0..=40 {
            let at = landed + fade.mul_f64(t as f64 / 40.0);
            let a = m.pose(at, NEW()).unwrap().live.unwrap().1;
            assert!(a >= last);
            last = a;
        }
    }

    #[test]
    fn both_layers_are_drawn_scaled_to_the_same_rect() {
        let mut m = waiting(&full(), ms(0));
        let p = m.pose(ms(5), NEW()).unwrap();
        let mid = m.pose(ms(5) + spec::GEOMETRY / 4, NEW()).unwrap();
        assert!(p.live.is_some());
        assert_eq!(mid.snapshot.unwrap().1, mid.live.unwrap().0);
    }

    #[test]
    fn maximize_then_unmaximize_mid_flight_turns_round_from_the_current_rect() {
        let mut m = waiting(&full(), ms(0));
        m.pose(ms(10), NEW()); // maximize lands
        let t = ms(10) + ms(90);
        let mid = drawn_live(&m.pose(t, NEW()).unwrap());
        assert!(mid.size.x > 800.0 && mid.size.x < 1280.0, "in flight: {mid:?}");
        // The user unmaximizes: a new snapshot of the live (maximized) window,
        // waiting for the restored size.
        m.interrupt(t, NEW(), Some(NEW().size));
        let held = m.pose(t, rect(200, 150, 1280, 700)).unwrap();
        let (depicted, drawn, _) = held.snapshot.unwrap();
        assert_eq!(depicted, NEW(), "the new snapshot depicts the live window");
        assert!(held.live.is_none());
        assert!((drawn.size.x - mid.size.x).abs() < 1e-6, "still where it was: {drawn:?} vs {mid:?}");
        // Velocity is kept while waiting: the rect keeps moving towards the old
        // target rather than freezing.
        let later = m.pose(t + ms(30), rect(200, 150, 1280, 700)).unwrap().snapshot.unwrap().1;
        assert!(later.size.x > drawn.size.x, "carried on");
        // The restored size lands: head for it, from here.
        let landed = t + ms(60);
        let before = m.pose(landed, rect(200, 150, 1280, 700)).unwrap().snapshot.unwrap().1;
        let p = m.pose(landed, OLD()).unwrap();
        let start = drawn_live(&p);
        assert!((start.size.x - before.size.x).abs() < 1e-6, "no jump on landing");
        let mut last = start.size.x;
        let mut turned = false;
        for k in 1..60 {
            let w = drawn_live(&m.pose(landed + ms(k * 10), OLD()).unwrap_or(p)).size.x;
            if w < last {
                turned = true;
            }
            last = w;
        }
        assert!(turned, "it came back down");
        assert!(m.pose(landed + spec::GEOMETRY * 3, OLD()).is_none());
    }

    #[test]
    fn a_move_that_needs_no_snapshot_is_the_live_window_translated() {
        // Same size, new place, nothing to wait for.
        let moved = rect(200, 44, 800, 600);
        let mut m = GeoMotion::start(&full(), ms(0), OLD(), false, None, OLD()).unwrap();
        // The window is placed, then drawn.
        let p = m.pose(ms(1), moved).unwrap();
        assert!(p.snapshot.is_none());
        let live = p.live.unwrap();
        assert_eq!(live.1, 1.0, "no crossfade without a snapshot");
        assert!(live.0.loc.y > 44.0, "starts near where it was");
        assert!(m.pose(ms(1) + spec::GEOMETRY * 2, moved).is_none());
    }

    #[test]
    fn nothing_to_draw_means_nothing_starts() {
        let off = AnimationSettings { mode: Mode::Off, scale: 1.0 };
        assert!(GeoMotion::start(&off, ms(0), OLD(), true, None, NEW()).is_none());
        let zero = AnimationSettings { mode: Mode::Full, scale: 0.0 };
        assert!(GeoMotion::start(&zero, ms(0), OLD(), true, None, NEW()).is_none());
        assert!(GeoMotion::start(&reduced(), ms(0), OLD(), false, None, NEW()).is_none(), "no picture, no crossfade");
    }

    #[test]
    fn reduced_is_a_short_crossfade_between_the_two_frames_with_no_scaling() {
        let mut m = waiting(&reduced(), ms(0));
        let p = m.pose(ms(10), NEW()).unwrap();
        // Landed: the live window is at the new frame from the first frame.
        assert_eq!(p.live.unwrap().0, FRect::of(NEW()));
        let (depicted, drawn, alpha) = p.snapshot.unwrap();
        assert_eq!((depicted, drawn), (OLD(), FRect::of(OLD())), "the old frame is not scaled");
        assert_eq!(alpha, 1.0);
        let mid = m.pose(ms(10) + ms(60), NEW()).unwrap();
        let (live_a, snap_a) = (mid.live.unwrap().1, mid.snapshot.unwrap().2);
        assert!(live_a > 0.0 && snap_a < 1.0);
        assert!((live_a + snap_a - 1.0).abs() < 1e-5, "a true crossfade");
        assert_eq!(mid.live.unwrap().0, FRect::of(NEW()));
        assert!(m.pose(ms(10) + ms(150), NEW()).is_none(), "capped at 150 ms");
    }

    #[test]
    fn scale_stretches_the_spring_and_the_fade() {
        let slow = AnimationSettings { mode: Mode::Full, scale: 10.0 };
        let mut m = GeoMotion::start(&slow, ms(0), OLD(), true, None, NEW()).unwrap();
        let fade = spec::GEOMETRY.mul_f64(spec::GEOMETRY_CROSSFADE) * 10;
        assert!(m.pose(fade - ms(1), NEW()).unwrap().snapshot.is_some());
        assert!(m.pose(fade, NEW()).unwrap().snapshot.is_none());
        assert!(m.pose(spec::GEOMETRY * 5, NEW()).is_some(), "the spring is slowed too");
    }

    #[test]
    fn expiry_ends_a_transition_nobody_draws() {
        let m = waiting(&full(), ms(0));
        assert!(!m.expired(PENDING_LIMIT));
        assert!(m.expired(PENDING_LIMIT + ms(200)));
    }

    #[test]
    fn the_settle_glint_crosses_from_forty_two_percent_to_the_end() {
        let mut m = waiting(&full(), ms(0));
        assert_eq!(m.glint(ms(0)), None, "nothing while waiting");
        m.pose(ms(10), NEW()); // lands at 10 ms
        let at = |f: f64| ms(10) + spec::GEOMETRY.mul_f64(f);
        assert_eq!(m.glint(at(0.30)), None);
        assert!(m.glint(at(glint::START + 0.001)).is_some_and(|g| g < 0.01), "starts at 42%");
        let mid = m.glint(at(0.71)).unwrap();
        assert!((mid - 0.5).abs() < 1e-6, "{mid}");
        // The transition is held until the band is across, even once the
        // springs have come to rest.
        assert!(m.pose(at(0.99), NEW()).is_some());
        assert!(!m.expired(at(0.99)));
        assert!(m.pose(at(2.0), NEW()).is_none());
    }

    #[test]
    fn reduced_has_no_glint() {
        let mut m = waiting(&reduced(), ms(0));
        m.pose(ms(10), NEW());
        for k in 0..20 {
            assert_eq!(m.glint(ms(10 + k * 8)), None);
        }
    }

    #[test]
    fn counters_balance_by_ownership() {
        // A transition counts from the moment it is made until it is dropped.
        let before = stats().started(AnimKind::Geometry);
        let done_before = stats().completed(AnimKind::Geometry);
        let t = GeoTransition::new(waiting(&full(), ms(0)), None, (0, 0).into());
        assert_eq!(stats().started(AnimKind::Geometry), before + 1);
        drop(t);
        assert_eq!(stats().completed(AnimKind::Geometry), done_before + 1);
    }
}
