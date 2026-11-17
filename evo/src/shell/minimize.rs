//! Minimize, restore and show-desktop animations: where windows go, and when
//! the animations start. The poses and springs are `crate::anim::minimize`.
//!
//! - **Placement stays instant.** `minimize_window` takes a window out of the
//!   Space at once, as before, and focus and input move on immediately. What
//!   the user sees is a *ghost*: the same live window, drawn at its old z-slot
//!   springing into its dock icon (`anim::ghosts::Live`). Restore is the
//!   reverse and runs on the window itself, as soon as it is back in the
//!   Space, so it is drawn at its real z-position.
//! - **Targets.** Bloom's dock reports each running app's icon rect
//!   (`org.geckoOS.evo SetMinimizeTargets`, output-logical pixels, relative to
//!   the output's origin). A window with no target goes to the bottom centre
//!   of its output. Targets are forgotten when the overlay window is destroyed,
//!   like the work area, so a dead Bloom can't aim windows at icons nobody
//!   draws.
//! - **Interrupting.** Restoring a window whose minimize ghost is still
//!   running takes that ghost's animation and turns it round (from its pose
//!   and velocity); minimizing a window that is mid-open or mid-restore does
//!   the same from its own. Nothing jumps and nothing waits.
//! - **Show desktop** (Super+D) minimizes every app window front-most first,
//!   each starting `SHOW_DESKTOP_STAGGER` after the last, drawn as Lumen's
//!   Recede, and a second Super+D brings the same set back (Emerge): see
//!   `shell/recede.rs`, which uses `stagger_delays` from here. Evo does this
//!   itself because only it knows the stacking order; Bloom hears the same
//!   `ShortcutTriggered` signal and no longer moves windows.
//!
//! The target table is process-wide, written by the Echo thread and read by
//! the compositor thread at minimize time, like the overlay's region. It is
//! only a lookup, so nothing on the compositor thread has to apply it.
//!
//! - **Lumen.** A minimize is drawn as a Droplet and a restore as a Rise
//!   (`anim::droplet`, through `anim::warp`) from a snapshot taken when it
//!   begins. Each `SetMinimizeTargets` entry may carry the app's `accent`
//!   (`"#rrggbb"`), the orb's colour; without one the picture's average is
//!   made into one.
//! - **`MinimizeLanded {app_id, window_id, phase, timestamp}`**: `land` when a
//!   minimize reaches 90% of its run (the orb touching the icon; at once when
//!   nothing animates), `rise` when a restore starts. Bloom ripples the icon.
//!   The time is worked out from the spring when the minimize starts and the
//!   signal sent from the event loop (`minimize_landed_tick`), so it fires the
//!   same whether or not Lumen draws.

use std::{cell::RefCell, collections::HashMap, sync::RwLock, time::Duration};

use serde_json::Value;
use smithay::{
    desktop::space::SpaceElement,
    utils::{IsAlive, Logical, Rectangle},
};

use super::WindowElement;
use crate::{
    anim::{
        self,
        droplet,
        ghosts::{self, Ghost, GhostSource},
        lumen,
        minimize::{self, Toward},
        spec,
        warp::Warp,
        AnimKind, TransformAnim, WindowAnim,
    },
    state::Backend,
    EvoState,
};

/// At most this many targets are accepted in one call.
pub const MAX_TARGETS: usize = 256;

/// The size of the stand-in icon a window with no target shrinks into.
pub const FALLBACK_ICON: i32 = 48;

type Targets = HashMap<String, Rectangle<i32, Logical>>;

static TARGETS: RwLock<Option<Targets>> = RwLock::new(None);

/// Replace the whole table.
pub fn set_targets(targets: Targets) {
    if let Ok(mut t) = TARGETS.write() {
        *t = Some(targets);
    }
}

/// Forget every target (the overlay is gone).
pub fn reset_targets() {
    if let Ok(mut t) = TARGETS.write() {
        *t = None;
    }
    set_accents(Accents::new());
}

/// Apps' accent colours, from the same call as the targets.
pub type Accents = HashMap<String, [f32; 3]>;

static ACCENTS: RwLock<Option<Accents>> = RwLock::new(None);

/// Replace the whole accent table.
pub fn set_accents(accents: Accents) {
    if let Ok(mut a) = ACCENTS.write() {
        *a = Some(accents);
    }
}

/// `app_id`'s accent, if Bloom sent one.
pub fn accent_for(app_id: Option<&str>) -> Option<[f32; 3]> {
    let id = app_id?;
    ACCENTS.read().ok()?.as_ref()?.get(id).copied()
}

/// The optional `accent: "#rrggbb"` of each `SetMinimizeTargets` entry. An
/// entry without one, or with one that doesn't parse, has none (the picture's
/// average is used): a bad colour never costs an app its target.
pub fn parse_accents(body: &Value) -> Accents {
    let mut out = Accents::new();
    for entry in body.get("targets").and_then(Value::as_array).into_iter().flatten().take(MAX_TARGETS) {
        let id = entry.get("app_id").and_then(Value::as_str).filter(|s| !s.is_empty());
        let accent = entry.get("accent").and_then(Value::as_str).and_then(lumen::parse_hex);
        if let (Some(id), Some(accent)) = (id, accent) {
            out.insert(id.to_string(), accent);
        }
    }
    out
}

/// `MinimizeLanded`'s phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landed {
    Land,
    Rise,
}

impl Landed {
    pub fn as_str(self) -> &'static str {
        match self {
            Landed::Land => "land",
            Landed::Rise => "rise",
        }
    }
}

struct Landing {
    window: WindowElement,
    due: Duration,
    phase: Landed,
}

thread_local! {
    /// Signals waiting for their moment, sent by `minimize_landed_tick`.
    static LANDINGS: RefCell<Vec<Landing>> = const { RefCell::new(Vec::new()) };
}

static LANDED_COUNTS: [std::sync::atomic::AtomicU64; 2] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 2];

/// How many `land` and `rise` signals have gone out (for `GetLumenState`).
pub fn landed_counts() -> (u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (LANDED_COUNTS[0].load(Relaxed), LANDED_COUNTS[1].load(Relaxed))
}

fn queue_landing(window: &WindowElement, due: Duration, phase: Landed) {
    LANDINGS.with(|l| {
        let mut l = l.borrow_mut();
        // A window has one trip in flight: a restore drops the land it had
        // pending, a minimize the rise.
        l.retain(|x| &x.window != window);
        l.push(Landing { window: window.clone(), due, phase });
    });
}

/// When `anim`, a run toward the icon, reaches the land point: its progress
/// (read off the spring as the Droplet reads it) at `droplet::LAND`, or, for a
/// fade with no motion (reduced), its alpha down to 10%. Scanned from `now`.
pub fn land_time(
    anim: &TransformAnim,
    now: Duration,
    frame: Rectangle<i32, Logical>,
    target: Rectangle<i32, Logical>,
) -> Duration {
    let end_scale = minimize::plan(frame, target).scale.y;
    let end = [
        anim.scale.start() + anim.scale.duration(),
        anim.offset.start() + anim.offset.duration(),
        anim.alpha.start() + anim.alpha.duration(),
    ]
    .into_iter()
    .max()
    .unwrap_or(now)
    .max(now);
    let step = Duration::from_millis(2);
    let mut t = now;
    while t < end {
        // A fade (fixed scale) never makes progress; its alpha says.
        let p = droplet::progress(anim.scale.value(t).y, end_scale);
        if p >= droplet::LAND || anim.alpha.value(t) <= 0.1 {
            return t;
        }
        t += step;
    }
    end
}

/// The table as it stands.
pub fn targets() -> Targets {
    TARGETS.read().ok().and_then(|t| t.clone()).unwrap_or_default()
}

/// Parse `SetMinimizeTargets`' body: `{"targets": [{app_id, x, y, w, h}, …]}`.
/// An entry with no area is dropped (nothing to aim at); a later entry for
/// the same app replaces an earlier one.
pub fn parse_targets(body: &Value) -> Result<Targets, String> {
    let list = body
        .get("targets")
        .and_then(Value::as_array)
        .ok_or_else(|| "targets must be an array of {app_id, x, y, w, h}".to_string())?;
    if list.len() > MAX_TARGETS {
        return Err(format!("at most {MAX_TARGETS} targets, got {}", list.len()));
    }
    let mut out = Targets::new();
    for (i, entry) in list.iter().enumerate() {
        let app_id = entry
            .get("app_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("targets[{i}].app_id must be a non-empty string"))?;
        let field = |name: &str| -> Result<i32, String> {
            let n = entry
                .get(name)
                .and_then(Value::as_f64)
                .ok_or_else(|| format!("targets[{i}].{name} must be a number"))?;
            if !n.is_finite() || n.abs() > 1e6 {
                return Err(format!("targets[{i}].{name} is out of range"));
            }
            Ok(n.round() as i32)
        };
        let (x, y, w, h) = (field("x")?, field("y")?, field("w")?, field("h")?);
        if w > 0 && h > 0 {
            out.insert(app_id.to_string(), Rectangle::new((x, y).into(), (w, h).into()));
        }
    }
    Ok(out)
}

/// Where a window of `app_id` minimizes to on an output whose geometry is
/// `output`: its reported icon rect (relative to the output's origin), else a
/// stand-in icon at the output's bottom centre. Output-logical (global)
/// coordinates.
pub fn resolve_target(
    targets: &Targets,
    app_id: Option<&str>,
    output: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    if let Some(rect) = app_id.and_then(|id| targets.get(id)) {
        return Rectangle::new(output.loc + rect.loc, rect.size);
    }
    let size = FALLBACK_ICON;
    Rectangle::new(
        (
            output.loc.x + (output.size.w - size) / 2,
            output.loc.y + output.size.h - size,
        )
            .into(),
        (size, size).into(),
    )
}

/// Front-most first, each starting one stagger after the last. Reduced mode
/// fades everything together (there is no motion to stagger).
pub fn stagger_delays(count: usize, settings: &anim::AnimationSettings) -> Vec<Duration> {
    let step = if settings.motion() {
        spec::SHOW_DESKTOP_STAGGER.mul_f64(settings.scale)
    } else {
        Duration::ZERO
    };
    (0..count).map(|i| step * i as u32).collect()
}

impl<BackendData: Backend> EvoState<BackendData> {
    /// The output `window` is on (else the first), for its target and scale.
    fn window_output(&self, window: &WindowElement) -> Option<smithay::output::Output> {
        self.space
            .outputs_for_element(window)
            .first()
            .cloned()
            .or_else(|| self.space.outputs().next().cloned())
    }

    /// Where `window` minimizes to, output-logical.
    fn minimize_target(&self, window: &WindowElement) -> Option<Rectangle<i32, Logical>> {
        let output = self.window_output(window)?;
        let geo = self.space.output_geometry(&output)?;
        Some(resolve_target(&targets(), window.app_id().as_deref(), geo))
    }

    /// The picture a Droplet or Rise draws `window` from, taken now: `None`
    /// (the minimize or restore is then drawn as genie-lite) when reduced mode
    /// has no motion, Lumen is switched off or its program already failed, or
    /// the backend can't snapshot.
    fn warp_for(
        &mut self,
        window: &WindowElement,
        target: Rectangle<i32, Logical>,
        settings: &anim::AnimationSettings,
    ) -> Option<Box<Warp>> {
        if !settings.motion() || !lumen::usable() {
            return None;
        }
        let output = self.window_output(window)?;
        let began = std::time::Instant::now();
        let snapshot = self
            .backend_data
            .snapshot_window(window, output.current_scale().fractional_scale())?;
        tracing::info!(
            "lumen droplet: snapshot of {}x{} took {:.1} ms",
            snapshot.size.w,
            snapshot.size.h,
            began.elapsed().as_secs_f64() * 1000.0
        );
        Some(Box::new(Warp::new(
            snapshot,
            target,
            accent_for(window.app_id().as_deref()),
            window.picture_radii(),
        )))
    }

    /// The frame `window` is drawn at now, `None` if it is not in the Space.
    fn animated_frame(&self, window: &WindowElement) -> Option<(Rectangle<i32, Logical>, Rectangle<i32, Logical>)> {
        let location = self.space.element_location(window)?;
        let geometry = SpaceElement::geometry(window);
        Some((Rectangle::new(location, geometry.size), geometry))
    }

    /// Draw `window` leaving for its dock icon, starting `delay` from now.
    /// Call while it is still in the Space, before it is unmapped. Does
    /// nothing (the window simply vanishes) when it may not animate, was never
    /// shown, or settings say off.
    pub(crate) fn begin_minimize_animation(&mut self, window: &WindowElement, delay: Duration) {
        if !window.has_opened() || !self.animation_eligible(window) || !self.is_window_mapped(window) {
            return;
        }
        // Landed at once unless an animation below says when.
        queue_landing(window, anim::now() + delay, Landed::Land);
        let Some((frame, geometry)) = self.animated_frame(window) else {
            return;
        };
        let Some(target) = self.minimize_target(window) else {
            return;
        };
        let settings = anim::settings();
        let now = anim::now();
        let start = now + delay;
        // Whatever the window is animating (an open, a restore) is where this
        // one starts from, and a restore's warp picture carries over.
        let (prev, prev_warp) = match window.with_animation(|a| (a.anim.clone(), a.warp.take())) {
            Some((anim, warp)) => (Some(anim), warp),
            None => (None, None),
        };
        let animation = match &prev {
            Some(prev) => minimize::retarget(prev, now, start, Toward::Icon, &settings, frame, target),
            None => minimize::minimize(&settings, start, frame, target),
        };
        window.cancel_animation();
        let Some(animation) = animation else {
            return;
        };
        queue_landing(window, land_time(&animation, now, frame, target), Landed::Land);
        let warp = match prev_warp.filter(|_| settings.motion()) {
            Some(mut warp) => {
                warp.target = target;
                Some(warp)
            }
            None => self.warp_for(window, target, &settings),
        };
        let anchor = self
            .space
            .elements()
            .take_while(|w| *w != window)
            .last()
            .cloned();
        // Ghosts already waiting on this window (it was their anchor) now sit
        // above its own ghost; see `ghosts::add`.
        ghosts::add(Ghost {
            source: GhostSource::Live(window.clone()),
            owner: Some(window.clone()),
            anim: WindowAnim::new(AnimKind::Minimize, animation).with_warp(warp),
            anchor,
            frame,
            render_origin: frame.loc - geometry.loc,
        });
    }

    /// Draw `window` coming back from its dock icon. Call once it is mapped
    /// into the Space again. `ghost` is its minimize ghost if that was still
    /// running: the restore starts from its pose and velocity.
    pub(crate) fn begin_restore_animation(&mut self, window: &WindowElement, mut ghost: Option<Ghost>) {
        // A window mapped into exclusive fullscreen is drawn by a path that
        // does not animate.
        let fullscreen = self
            .space
            .outputs()
            .any(|o| {
                o.user_data()
                    .get::<super::FullscreenSurface>()
                    .and_then(|slot| slot.get())
                    .is_some_and(|held| &held == window)
            });
        if !window.has_opened() || !self.animation_eligible(window) {
            return;
        }
        queue_landing(window, anim::now(), Landed::Rise);
        if fullscreen || !self.is_window_mapped(window) {
            return;
        }
        let Some((frame, _)) = self.animated_frame(window) else {
            return;
        };
        let Some(target) = self.minimize_target(window) else {
            return;
        };
        let settings = anim::settings();
        let now = anim::now();
        let animation = match &ghost {
            Some(g) => minimize::retarget(&g.anim.anim, now, now, Toward::Frame, &settings, frame, target),
            None => minimize::restore(&settings, now, frame, target),
        };
        // The minimize that was still running hands over its picture, so the
        // warp turns round on the same pixels.
        let carried = ghost.as_mut().and_then(|g| g.anim.warp.take()).filter(|_| settings.motion());
        // The ghost is finished with: dropping it completes its minimize.
        drop(ghost);
        if let Some(animation) = animation {
            let warp = match carried {
                Some(mut warp) => {
                    warp.target = target;
                    Some(warp)
                }
                None => self.warp_for(window, target, &settings),
            };
            window.start_animation(WindowAnim::new(AnimKind::Restore, animation).with_warp(warp));
        }
    }

    /// Send the `MinimizeLanded` signals that are due (per event-loop pass).
    /// A window that died in the meantime is dropped silently.
    pub fn minimize_landed_tick(&mut self) {
        let now = anim::now();
        let due: Vec<Landing> = LANDINGS.with(|l| {
            let mut l = l.borrow_mut();
            let (due, wait): (Vec<_>, Vec<_>) = l.drain(..).partition(|x| x.due <= now);
            *l = wait;
            due
        });
        for landing in due {
            if !landing.window.alive() {
                continue;
            }
            LANDED_COUNTS[landing.phase as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            #[cfg(feature = "echo-ipc")]
            if let Some(id) = self.window_id_for(&landing.window) {
                let app_id = landing.window.app_id().unwrap_or_default();
                let _ = self.echo_service.read().emit_minimize_landed(&app_id, id, landing.phase.as_str());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn a_reported_icon_is_the_target_relative_to_its_output() {
        let mut t = Targets::new();
        t.insert("com.pyxis.files".into(), rect(600, 740, 48, 48));
        let out = rect(0, 0, 1280, 800);
        assert_eq!(resolve_target(&t, Some("com.pyxis.files"), out), rect(600, 740, 48, 48));
        // On a second output the same rect is offset by its origin.
        let right = rect(1280, 0, 1280, 800);
        assert_eq!(resolve_target(&t, Some("com.pyxis.files"), right), rect(1880, 740, 48, 48));
    }

    #[test]
    fn an_unknown_app_goes_to_the_bottom_centre_of_its_output() {
        let t = Targets::new();
        let out = rect(0, 0, 1280, 800);
        let want = rect(616, 752, FALLBACK_ICON, FALLBACK_ICON);
        assert_eq!(resolve_target(&t, Some("nobody"), out), want);
        assert_eq!(resolve_target(&t, None, out), want);
        let right = rect(1280, 100, 1000, 600);
        assert_eq!(resolve_target(&t, None, right), rect(1280 + 476, 100 + 552, 48, 48));
    }

    #[test]
    fn targets_parse_and_drop_empty_or_invalid_entries() {
        let body = json!({"targets": [
            {"app_id": "a", "x": 10.4, "y": 20, "w": 48, "h": 48},
            {"app_id": "b", "x": 0, "y": 0, "w": 0, "h": 48},
            {"app_id": "a", "x": 99, "y": 20, "w": 40, "h": 40},
        ]});
        let t = parse_targets(&body).unwrap();
        assert_eq!(t.len(), 1, "an empty rect is dropped");
        assert_eq!(t["a"], rect(99, 20, 40, 40), "a later entry replaces an earlier one");
        assert!(parse_targets(&json!({"targets": []})).unwrap().is_empty());
        assert!(parse_targets(&json!({})).is_err());
        assert!(parse_targets(&json!({"targets": [{"app_id": "", "x": 0, "y": 0, "w": 1, "h": 1}]})).is_err());
        assert!(parse_targets(&json!({"targets": [{"app_id": "a", "x": "no", "y": 0, "w": 1, "h": 1}]})).is_err());
        let many: Vec<Value> = (0..=MAX_TARGETS)
            .map(|i| json!({"app_id": format!("a{i}"), "x": 0, "y": 0, "w": 1, "h": 1}))
            .collect();
        assert!(parse_targets(&json!({ "targets": many })).is_err());
    }

    #[test]
    fn the_table_is_replaced_whole_and_reset() {
        // One test owns the global, so nothing else races it.
        let mut t = Targets::new();
        t.insert("a".into(), rect(1, 2, 3, 4));
        set_targets(t);
        assert_eq!(targets().len(), 1);
        set_targets(Targets::new());
        assert!(targets().is_empty());
        let mut t = Targets::new();
        t.insert("a".into(), rect(1, 2, 3, 4));
        set_targets(t);
        reset_targets();
        assert!(targets().is_empty());
    }

    #[test]
    fn show_desktop_staggers_front_most_first_by_45ms_scaled() {
        let full = anim::AnimationSettings::default();
        assert_eq!(
            stagger_delays(3, &full),
            vec![Duration::ZERO, Duration::from_millis(45), Duration::from_millis(90)]
        );
        let slow = anim::AnimationSettings { mode: anim::Mode::Full, scale: 10.0 };
        assert_eq!(stagger_delays(2, &slow)[1], Duration::from_millis(450));
        let reduced = anim::AnimationSettings { mode: anim::Mode::Reduced, scale: 1.0 };
        assert!(stagger_delays(3, &reduced).iter().all(|d| d.is_zero()));
    }

    #[test]
    fn accents_parse_leniently() {
        let body = json!({"targets": [
            {"app_id": "a", "x": 0, "y": 0, "w": 48, "h": 48, "accent": "#ff0000"},
            {"app_id": "b", "x": 0, "y": 0, "w": 48, "h": 48, "accent": "red"},
            {"app_id": "c", "x": 0, "y": 0, "w": 48, "h": 48},
        ]});
        let a = parse_accents(&body);
        assert_eq!(a.len(), 1);
        assert_eq!(a["a"], [1.0, 0.0, 0.0]);
        // A bad accent never costs an app its target.
        assert_eq!(parse_targets(&body).unwrap().len(), 3);
        assert!(parse_accents(&json!({})).is_empty());
    }

    #[test]
    fn the_land_point_is_90_percent_of_the_run_or_a_faded_reduced_one() {
        let frame = rect(200, 80, 800, 600);
        let icon = rect(900, 740, 48, 48);
        let full = anim::AnimationSettings::default();
        let ms = Duration::from_millis;
        let a = minimize::minimize(&full, ms(1000), frame, icon).unwrap();
        let t = land_time(&a, ms(1000), frame, icon);
        let p = droplet::progress(a.scale.value(t).y, minimize::plan(frame, icon).scale.y);
        assert!(p >= droplet::LAND && p < droplet::LAND + 0.02, "lands at p={p}");
        assert!(t > ms(1100) && t < ms(1400), "{t:?}");
        // A stagger delays it.
        let late = minimize::minimize(&full, ms(1100), frame, icon).unwrap();
        assert!(land_time(&late, ms(1000), frame, icon) > t);
        // Reduced: a fade, landed when 90% faded.
        let reduced = anim::AnimationSettings { mode: anim::Mode::Reduced, scale: 1.0 };
        let r = minimize::minimize(&reduced, ms(0), frame, icon).unwrap();
        let tr = land_time(&r, ms(0), frame, icon);
        assert!(r.alpha.value(tr) <= 0.1 && tr <= ms(150), "{tr:?}");
    }
}
