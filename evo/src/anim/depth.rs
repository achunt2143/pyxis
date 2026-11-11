//! Depth: what makes a window read as a card above the ones below it. The drop
//! shadow, how it breathes with focus and with a move grab, and the rounded
//! bottom corners that match the title bar's rounded top.
//!
//! Shadows and corners are *static styling*: they are drawn in `reduced` and
//! `off` modes too. Only the transitions (focus, lift) are animation, and
//! those follow the mode: instant when off, the usual short fade when reduced.
//!
//! This file is the model and the arithmetic, all of it testable without a
//! renderer: the style (how big and dark a shadow is), the geometry (where it
//! goes around a frame), the per-window focus/lift state. `shader.rs` draws it.
//!
//! # Rules
//!
//! - Never under the shell, the overlay or a popup, never under a window that
//!   has yet to say who it is (the `vela-default` pre-window), and never under
//!   a maximized or fullscreen window, which also gets square corners.
//! - A shadow takes no input: it is a render element and nothing more. The
//!   resize band is invisible and lies outside the frame, and is hit-tested by
//!   `WindowElement`, not by what is drawn there.
//! - The shadow follows the window's animation transform: `shadow_params`
//!   takes the pose, so open, close and the phase 2 ghosts carry their shadow.

use std::{cell::RefCell, time::Duration};

use smithay::{
    backend::renderer::{element::Id, utils::CommitCounter},
    utils::{Logical, Physical, Point, Rectangle, Size},
};

use super::{
    curve::{Animation, Timing},
    settings::AnimationSettings,
    shader::{Programs, ShadowElement, ShadowParams},
    spec::{FOCUS, LIFT, STANDARD_CURVE},
    transform::WindowTransform,
};
use crate::shell::{placement::is_bindable_app_id, WindowElement};

/// Corner radius of an SSD frame, logical px. The title bar's rounded top is
/// rasterized at the same radius (`ssd::GLASS_CORNER_RADIUS`).
pub const FRAME_RADIUS: f64 = 16.0;
/// Radius assumed for a window Evo does not decorate (a client-drawn frame, an
/// X11 window): only the shadow follows it.
pub const CLIENT_RADIUS: f64 = 8.0;

/// A shadow's look at one instant, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowStyle {
    /// Deviation of the gaussian: the blur.
    pub sigma: f64,
    /// How far the shadow's source sits below the frame (the light is above).
    pub offset_y: f64,
    /// Peak alpha of the shadow.
    pub strength: f64,
    /// The frame's corner radius.
    pub radius: f64,
}

// Inactive windows sit low and tight, the focused one floats higher with a
// larger, softer shadow, and a window in the hand is higher again.
const INACTIVE: ShadowStyle = ShadowStyle { sigma: 6.0, offset_y: 3.0, strength: 0.34, radius: 0.0 };
const ACTIVE: ShadowStyle = ShadowStyle { sigma: 14.0, offset_y: 9.0, strength: 0.56, radius: 0.0 };
const LIFTED_EXTRA: ShadowStyle = ShadowStyle { sigma: 6.0, offset_y: 8.0, strength: 0.10, radius: 0.0 };

/// The header bar's brightness while the window is inactive, as a fraction of
/// its active alpha (glass, title and buttons alike).
pub const HEADER_INACTIVE: f32 = 0.55;

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// The facts about a window that decide its depth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthInput {
    /// An app window: not the shell, not the overlay, with a real app id.
    pub app: bool,
    /// Maximized or fullscreen: no shadow, square corners.
    pub square: bool,
    /// Evo draws its frame (rounded at `FRAME_RADIUS`).
    pub ssd: bool,
    /// 0 inactive .. 1 active, mid-crossfade in between.
    pub focus: f64,
    /// 0 .. 1, the grab lift.
    pub lift: f64,
}

/// The shadow a window gets, or `None` for no shadow.
pub fn plan(input: &DepthInput) -> Option<ShadowStyle> {
    if !input.app || input.square {
        return None;
    }
    Some(ShadowStyle {
        sigma: lerp(INACTIVE.sigma, ACTIVE.sigma, input.focus) + LIFTED_EXTRA.sigma * input.lift,
        offset_y: lerp(INACTIVE.offset_y, ACTIVE.offset_y, input.focus) + LIFTED_EXTRA.offset_y * input.lift,
        strength: lerp(INACTIVE.strength, ACTIVE.strength, input.focus) + LIFTED_EXTRA.strength * input.lift,
        radius: if input.ssd { FRAME_RADIUS } else { CLIENT_RADIUS },
    })
}

/// The radius client content is clipped to at its bottom corners, logical px;
/// 0 means no clip. Only an Evo-drawn frame has content to match, and a
/// maximized window is square.
pub fn content_radius(ssd: bool, square: bool) -> f64 {
    if ssd && !square {
        FRAME_RADIUS
    } else {
        0.0
    }
}

/// How the pose moves a frame, as the `Rescale`/`Relocate` pair does: scaled
/// about the pose's origin, then offset. Output-physical pixels.
pub fn pose_frame(
    frame: Rectangle<i32, Logical>,
    pose: Option<&WindowTransform>,
    output_origin: Point<i32, Logical>,
    output_scale: f64,
) -> Rectangle<i32, Physical> {
    let (x, y, w, h) = match pose {
        Some(p) => (
            p.origin.x + (frame.loc.x as f64 - p.origin.x) * p.scale.x + p.offset.x,
            p.origin.y + (frame.loc.y as f64 - p.origin.y) * p.scale.y + p.offset.y,
            frame.size.w as f64 * p.scale.x,
            frame.size.h as f64 * p.scale.y,
        ),
        None => (frame.loc.x as f64, frame.loc.y as f64, frame.size.w as f64, frame.size.h as f64),
    };
    let px = |v: f64| (v * output_scale).round() as i32;
    Rectangle::new(
        Point::from((px(x - output_origin.x as f64), px(y - output_origin.y as f64))),
        Size::from((px(w), px(h))),
    )
}

/// How many standard deviations of blur a shadow is drawn out to.
const REACH_SIGMAS: f64 = 3.0;

/// The shader's parameters for `frame` (output-physical) in `style`. `px` is
/// physical pixels per logical pixel, the output scale times the pose's scale,
/// so a window that is scaled down has a proportionally smaller shadow.
pub fn shadow_params(
    frame: Rectangle<i32, Physical>,
    style: &ShadowStyle,
    px: f64,
    alpha: f32,
) -> Option<ShadowParams> {
    if frame.size.w <= 0 || frame.size.h <= 0 || alpha <= 0.0 || style.strength <= 0.0 {
        return None;
    }
    let sigma = style.sigma * px;
    let shift_y = style.offset_y * px;
    // Far enough that the gaussian has fallen to nothing at the area's edge,
    // in every direction, with the shift counted against it below.
    let reach = (REACH_SIGMAS * sigma + shift_y.abs()).ceil() as i32 + 2;
    let area = Rectangle::new(
        Point::from((frame.loc.x - reach, frame.loc.y - reach)),
        Size::from((frame.size.w + 2 * reach, frame.size.h + 2 * reach)),
    );
    Some(ShadowParams {
        area,
        rect: Rectangle::new(
            Point::from((reach as f64, reach as f64)),
            Size::from((frame.size.w as f64, frame.size.h as f64)),
        ),
        shift: (0.0, shift_y),
        radius: style.radius * px,
        sigma,
        strength: style.strength,
        alpha,
    })
}

// === Per-window state ===

/// What the damage tracker needs to see a still shadow as unchanged: a stable
/// id, and a commit counter that moves only when the parameters do.
struct ShadowTrack {
    id: Id,
    commit: CommitCounter,
    last: Option<ShadowParams>,
}

struct DepthSlot {
    focus: RefCell<Animation<f64>>,
    lift: RefCell<Animation<f64>>,
    /// 1 while the window casts its shadow, 0 once it is square (maximized
    /// or fullscreen): it fades out as the window goes square and back in as
    /// it leaves that state, so neither end pops.
    appear: RefCell<Animation<f64>>,
    /// `appear` has seen the window's squareness at least once, so a window
    /// born maximized starts with no shadow instead of fading one out.
    appear_seen: std::cell::Cell<bool>,
    shadow: RefCell<ShadowTrack>,
}

impl Default for DepthSlot {
    fn default() -> Self {
        Self {
            focus: RefCell::new(Animation::fixed(0.0)),
            lift: RefCell::new(Animation::fixed(0.0)),
            appear: RefCell::new(Animation::fixed(1.0)),
            appear_seen: std::cell::Cell::new(false),
            shadow: RefCell::new(ShadowTrack { id: Id::new(), commit: CommitCounter::default(), last: None }),
        }
    }
}

/// Head `anim` for `to` over `base` in the mode's terms: instantly when
/// animations are off, a short fade otherwise (reduced caps it, the scale
/// stretches it). A target it already has is left alone, so it does not
/// restart.
fn aim(anim: &mut Animation<f64>, now: Duration, to: f64, base: Duration, settings: &AnimationSettings) {
    if anim.to() == to {
        return;
    }
    match settings.duration(base) {
        Some(duration) => anim.retarget_with(now, to, Timing::curve(duration, STANDARD_CURVE)),
        None => *anim = Animation::fixed(to),
    }
}

impl WindowElement {
    fn depth_slot(&self) -> &DepthSlot {
        self.user_data().insert_if_missing(DepthSlot::default);
        self.user_data().get::<DepthSlot>().expect("just inserted")
    }

    /// The window gained or lost the keyboard: crossfade the header tint and
    /// the shadow depth.
    pub fn set_depth_focus(&self, focused: bool) {
        let slot = self.depth_slot();
        aim(
            &mut slot.focus.borrow_mut(),
            super::now(),
            if focused { 1.0 } else { 0.0 },
            FOCUS,
            &super::settings(),
        );
    }

    /// A move grab began (`true`) or ended: the shadow grows over `LIFT` and
    /// settles back.
    pub fn set_lifted(&self, lifted: bool) {
        aim(
            &mut self.depth_slot().lift.borrow_mut(),
            super::now(),
            if lifted { 1.0 } else { 0.0 },
            LIFT,
            &super::settings(),
        );
    }

    /// Focus and lift levels at `now`, each 0..=1.
    pub fn depth_levels(&self, now: Duration) -> (f64, f64) {
        match self.user_data().get::<DepthSlot>() {
            Some(slot) => (slot.focus.borrow().value(now), slot.lift.borrow().value(now)),
            None => (0.0, 0.0),
        }
    }

    /// Whether a focus or lift transition is still running at `now`.
    pub fn depth_animating(&self, now: Duration) -> bool {
        self.user_data()
            .get::<DepthSlot>()
            .is_some_and(|s| !s.focus.borrow().is_finished(now) || !s.lift.borrow().is_finished(now))
    }

    /// Whether the window is an app window that depth applies to at all.
    fn depth_app(&self) -> bool {
        !self.is_shell_window() && self.claimed_app_id().is_some_and(|id| is_bindable_app_id(&id))
    }

    /// The shadow this window casts at `now`, `None` for none. Also where the
    /// window's squareness is noticed, once per frame it is drawn.
    pub fn shadow_style(&self, now: Duration) -> Option<ShadowStyle> {
        let (focus, lift) = self.depth_levels(now);
        let square = self.is_square();
        // Squareness is noticed here, once per frame the window is drawn. The
        // shadow fades out as the window goes square and back in as it leaves
        // that state, following the window through its geometry transition
        // (render.rs gives it the drawn rect); once gone it is not drawn at all.
        let gone = {
            let slot = self.depth_slot();
            let mut appear = slot.appear.borrow_mut();
            if !slot.appear_seen.replace(true) && square {
                *appear = Animation::fixed(0.0);
            }
            aim(&mut appear, now, if square { 0.0 } else { 1.0 }, FOCUS, &super::settings());
            square && appear.value(now) <= 0.001
        };
        plan(&DepthInput {
            app: self.depth_app(),
            square: gone,
            ssd: self.is_ssd(),
            focus,
            lift,
        })
    }

    /// How much of its shadow the window shows, 0..=1 (see `DepthSlot::appear`).
    pub fn shadow_appear(&self, now: Duration) -> f64 {
        self.user_data().get::<DepthSlot>().map_or(1.0, |s| s.appear.borrow().value(now))
    }

    /// The radius, in logical pixels, to clip this window's content at its
    /// bottom corners to (0: don't).
    pub fn content_corner_radius(&self) -> f64 {
        if !self.depth_app() {
            return 0.0;
        }
        content_radius(self.is_ssd(), self.is_square())
    }

    /// The shadow element for `params`: the same id every frame, and a commit
    /// counter that moves when (and only when) the parameters change, so a
    /// window at rest costs no damage.
    pub fn shadow_element(&self, programs: &Programs, params: ShadowParams) -> ShadowElement {
        let slot = self.depth_slot();
        let mut track = slot.shadow.borrow_mut();
        if track.last != Some(params) {
            track.commit.increment();
            track.last = Some(params);
        }
        ShadowElement::new(programs.shadow.clone(), track.id.clone(), track.commit, params)
    }
}

thread_local! {
    /// The window that had the keyboard at the last focus change.
    static FOCUSED: RefCell<Option<WindowElement>> = const { RefCell::new(None) };
}

/// The keyboard focus moved to `window` (`None`: to something that is not an
/// app window, or to nothing). Called from `SeatHandler::focus_changed`.
pub fn focus_changed(window: Option<&WindowElement>) {
    FOCUSED.with(|focused| {
        let mut focused = focused.borrow_mut();
        if focused.as_ref() == window {
            return;
        }
        if let Some(old) = focused.take() {
            old.set_depth_focus(false);
        }
        if let Some(new) = window {
            new.set_depth_focus(true);
        }
        *focused = window.cloned();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::curve::Vec2;

    fn app() -> DepthInput {
        DepthInput { app: true, square: false, ssd: true, focus: 1.0, lift: 0.0 }
    }

    #[test]
    fn a_maximized_window_has_no_shadow_and_square_corners() {
        assert!(plan(&app()).is_some());
        assert_eq!(plan(&DepthInput { square: true, ..app() }), None);
        assert_eq!(content_radius(true, true), 0.0);
        assert_eq!(content_radius(true, false), FRAME_RADIUS);
    }

    #[test]
    fn the_shell_and_unidentified_windows_get_no_shadow() {
        assert_eq!(plan(&DepthInput { app: false, ..app() }), None);
    }

    #[test]
    fn only_a_frame_evo_draws_has_content_to_round() {
        assert_eq!(content_radius(false, false), 0.0);
        assert_eq!(plan(&DepthInput { ssd: false, ..app() }).unwrap().radius, CLIENT_RADIUS);
        assert_eq!(plan(&app()).unwrap().radius, FRAME_RADIUS);
    }

    #[test]
    fn focus_makes_the_shadow_larger_softer_and_darker() {
        let inactive = plan(&DepthInput { focus: 0.0, ..app() }).unwrap();
        let active = plan(&DepthInput { focus: 1.0, ..app() }).unwrap();
        assert!(active.sigma > inactive.sigma);
        assert!(active.offset_y > inactive.offset_y);
        assert!(active.strength > inactive.strength);
        let mid = plan(&DepthInput { focus: 0.5, ..app() }).unwrap();
        assert!(mid.sigma > inactive.sigma && mid.sigma < active.sigma);
    }

    #[test]
    fn lift_grows_the_shadow_beyond_the_active_one() {
        let rest = plan(&app()).unwrap();
        let lifted = plan(&DepthInput { lift: 1.0, ..app() }).unwrap();
        assert!(lifted.sigma > rest.sigma && lifted.offset_y > rest.offset_y && lifted.strength > rest.strength);
        // Out-of-range levels are clamped by the interpolation, not trusted.
        assert_eq!(plan(&DepthInput { focus: 7.0, ..app() }), plan(&app()));
    }

    #[test]
    fn the_shadow_area_surrounds_the_frame_by_its_reach() {
        let frame = Rectangle::new(Point::from((100, 80)), Size::from((400, 300)));
        let style = plan(&app()).unwrap();
        let p = shadow_params(frame, &style, 1.0, 1.0).unwrap();
        // Contains the frame, and reaches three sigma plus the shift in every direction.
        let need = (3.0 * style.sigma + style.offset_y).ceil() as i32;
        assert!(p.area.loc.x <= frame.loc.x - need);
        assert!(p.area.loc.y <= frame.loc.y - need);
        assert!(p.area.loc.x + p.area.size.w >= frame.loc.x + frame.size.w + need);
        assert!(p.area.loc.y + p.area.size.h >= frame.loc.y + frame.size.h + need);
        // The frame sits in the area where `rect` says it does.
        assert_eq!(p.area.loc.x + p.rect.loc.x as i32, frame.loc.x);
        assert_eq!(p.area.loc.y + p.rect.loc.y as i32, frame.loc.y);
        assert_eq!((p.rect.size.w, p.rect.size.h), (400.0, 300.0));
    }

    #[test]
    fn a_degenerate_frame_or_a_vanished_alpha_draws_nothing() {
        let style = plan(&app()).unwrap();
        let empty = Rectangle::new(Point::from((0, 0)), Size::from((0, 10)));
        assert_eq!(shadow_params(empty, &style, 1.0, 1.0), None);
        let frame = Rectangle::new(Point::from((0, 0)), Size::from((10, 10)));
        assert_eq!(shadow_params(frame, &style, 1.0, 0.0), None);
    }

    #[test]
    fn the_shadow_scales_with_the_pose_and_the_output() {
        let frame = Rectangle::new(Point::from((0, 0)), Size::from((200, 100)));
        let style = plan(&app()).unwrap();
        let one = shadow_params(frame, &style, 1.0, 1.0).unwrap();
        let two = shadow_params(frame, &style, 2.0, 1.0).unwrap();
        assert!((two.sigma - one.sigma * 2.0).abs() < 1e-9);
        assert!((two.radius - one.radius * 2.0).abs() < 1e-9);
        assert_eq!(two.shift.1, one.shift.1 * 2.0);
    }

    #[test]
    fn pose_frame_matches_scaling_about_the_origin_then_offsetting() {
        let frame = Rectangle::new(Point::from((100, 100)), Size::from((200, 100)));
        assert_eq!(
            pose_frame(frame, None, Point::from((0, 0)), 1.0),
            Rectangle::new(Point::from((100, 100)), Size::from((200, 100)))
        );
        // Scale 0.5 about the frame's own top-left: same corner, half the size.
        let pose = WindowTransform {
            origin: Point::from((100.0, 100.0)),
            scale: Vec2::splat(0.5),
            offset: Vec2::new(0.0, 10.0),
            alpha: 0.5,
        };
        assert_eq!(
            pose_frame(frame, Some(&pose), Point::from((0, 0)), 1.0),
            Rectangle::new(Point::from((100, 110)), Size::from((100, 50)))
        );
        // Output origin and scale: logical output-relative, then to pixels.
        assert_eq!(
            pose_frame(frame, Some(&pose), Point::from((50, 50)), 2.0),
            Rectangle::new(Point::from((100, 120)), Size::from((200, 100)))
        );
    }

    mod transitions {
        use super::*;
        use crate::anim::curve::Animation;

        const SETTINGS: AnimationSettings = AnimationSettings { mode: crate::anim::Mode::Full, scale: 1.0 };

        fn ms(n: u64) -> Duration {
            Duration::from_millis(n)
        }

        #[test]
        fn focus_runs_over_the_focus_token_and_lands_exactly() {
            let mut a = Animation::fixed(0.0);
            aim(&mut a, ms(1000), 1.0, FOCUS, &SETTINGS);
            assert_eq!(a.value(ms(1000)), 0.0);
            let mid = a.value(ms(1075));
            assert!(mid > 0.0 && mid < 1.0);
            assert!(!a.is_finished(ms(1149)));
            assert!(a.is_finished(ms(1150)));
            assert_eq!(a.value(ms(1150)), 1.0);
        }

        #[test]
        fn lift_is_shorter_than_focus() {
            let mut a = Animation::fixed(0.0);
            aim(&mut a, ms(0), 1.0, LIFT, &SETTINGS);
            assert!(a.is_finished(ms(120)));
        }

        #[test]
        fn retargeting_midway_turns_around_without_a_jump() {
            let mut a = Animation::fixed(0.0);
            aim(&mut a, ms(0), 1.0, FOCUS, &SETTINGS);
            let before = a.value(ms(60));
            aim(&mut a, ms(60), 0.0, FOCUS, &SETTINGS);
            assert!((a.value(ms(60)) - before).abs() < 1e-9);
            assert_eq!(a.value(ms(60 + 150)), 0.0);
        }

        #[test]
        fn aiming_at_the_target_it_already_has_does_not_restart() {
            let mut a = Animation::fixed(0.0);
            aim(&mut a, ms(0), 1.0, FOCUS, &SETTINGS);
            let at = a.value(ms(100));
            aim(&mut a, ms(100), 1.0, FOCUS, &SETTINGS);
            assert_eq!(a.value(ms(100)), at);
        }

        #[test]
        fn off_mode_is_instant() {
            let off = AnimationSettings { mode: crate::anim::Mode::Off, scale: 1.0 };
            let mut a = Animation::fixed(0.0);
            aim(&mut a, ms(0), 1.0, FOCUS, &off);
            assert_eq!(a.value(ms(0)), 1.0);
            assert!(a.is_finished(ms(0)));
            let zero_scale = AnimationSettings { mode: crate::anim::Mode::Full, scale: 0.0 };
            let mut b = Animation::fixed(1.0);
            aim(&mut b, ms(0), 0.0, LIFT, &zero_scale);
            assert_eq!(b.value(ms(0)), 0.0);
        }

        #[test]
        fn reduced_mode_keeps_the_fade_short() {
            let reduced = AnimationSettings { mode: crate::anim::Mode::Reduced, scale: 1.0 };
            let mut a = Animation::fixed(0.0);
            aim(&mut a, ms(0), 1.0, FOCUS, &reduced);
            assert!(a.is_finished(ms(150)));
            assert!(!a.is_finished(ms(100)));
        }
    }
}
