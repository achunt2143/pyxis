//! Show desktop (Super+D) as Lumen draws it: Recede and Emerge. The poses are
//! `crate::anim::recede`; this is the part that knows windows and the Space.
//!
//! # The state model
//!
//! A show desktop minimizes every app window on screen, as before (placement
//! instant, focus to the shell, each window in the workspace as minimized),
//! but draws it as a **Recede**: front-most first, each one stagger
//! (`spec::SHOW_DESKTOP_STAGGER`, 45 ms) after the last, no `MinimizeLanded`
//! (nothing lands in an icon). Evo remembers the set it sent away: the
//! **desktop is shown**.
//!
//! - **Super+D again** while the desktop is shown brings back *exactly* that
//!   set with **Emerge**: back-most first, the same stagger, the front-most
//!   one focused (it was the focused one).
//! - The desktop stops being shown as soon as any app window is on screen
//!   again: a dock restore (which is a Rise, per window, as any restore), a
//!   window opening. Super+D then sends what is on screen away (a fresh
//!   Recede) rather than bringing the rest back. So no hook is needed in the
//!   restore or open paths: "shown" is judged when Super+D is pressed, from
//!   the remembered set (someone still minimized) and the screen (no app
//!   window on it).
//! - Bloom mirrors the same rule from the signals for its wallpaper drift
//!   (`ShortcutTriggered show-desktop` toggles; `WindowMinimized false` or
//!   `WindowCreated` ends it).
//!
//! Ghosts keep the stack's order as lane B's minimize does: they are made back
//! to front, each anchored to the window below it (`ghosts::add`). Emerge runs
//! on the windows themselves once they are back in the Space, mapped back to
//! front so the stack comes back as it was. Reduced: a fade (≤ 150 ms, no
//! stagger); off: instant.

use std::{cell::RefCell, sync::Mutex, time::Duration};

use serde_json::{json, Value};
use smithay::{
    desktop::space::SpaceElement,
    utils::{IsAlive, Rectangle},
};

use super::WindowElement;
use crate::{
    anim::{
        self,
        ghosts::{self, Ghost, GhostSource},
        lumen,
        recede::{self as poses, EmergeFrom, RecedeFx},
        AnimKind, WindowAnim,
    },
    shell::minimize::stagger_delays,
    state::Backend,
    EvoState,
};

/// How a minimize is drawn: a Droplet into the dock icon (any minimize), or
/// a Recede (show desktop). The `Duration` is when the drawing starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MinimizeStyle {
    Droplet(Duration),
    Recede(Duration),
}

/// How a restore is drawn: a Rise out of the dock icon, or an Emerge (a
/// second Super+D), starting the given time from now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreStyle {
    Rise,
    Emerge(Duration),
}

thread_local! {
    /// The windows the last show desktop sent away, front-most first.
    static RECEDED: RefCell<Vec<WindowElement>> = const { RefCell::new(Vec::new()) };
}

/// What the suites read (`GetLumenState.recede`), written by the compositor
/// thread and read by the Echo thread.
#[derive(Debug, Default, Clone)]
struct Report {
    shown: bool,
    recedes: u64,
    emerges: u64,
    /// Window ids in the order their animations start, last time each ran.
    recede_order: Vec<u32>,
    emerge_order: Vec<u32>,
}

static REPORT: Mutex<Option<Report>> = Mutex::new(None);

fn report(f: impl FnOnce(&mut Report)) {
    if let Ok(mut r) = REPORT.lock() {
        f(r.get_or_insert_with(Report::default));
    }
}

/// `{shown, recedes, emerges, recede_order, emerge_order}` for `GetLumenState`.
pub fn state_json() -> Value {
    let r = REPORT.lock().ok().and_then(|r| r.clone()).unwrap_or_default();
    json!({
        "shown": r.shown,
        "recedes": r.recedes,
        "emerges": r.emerges,
        "recede_order": r.recede_order,
        "emerge_order": r.emerge_order,
    })
}

/// Which way Super+D goes, given the remembered set and the screen.
#[derive(Debug, PartialEq, Eq)]
pub enum Toggle {
    Recede,
    Emerge,
}

/// The rule of the module header: emerge only when nothing is on screen and
/// something of the remembered set is still away.
pub fn toggle(on_screen: usize, still_away: usize) -> Toggle {
    if on_screen == 0 && still_away > 0 {
        Toggle::Emerge
    } else {
        Toggle::Recede
    }
}

/// Back-most first: the reverse of the recede's order, with the same delays.
pub fn emerge_delays(count: usize, settings: &anim::AnimationSettings) -> Vec<Duration> {
    let mut d = stagger_delays(count, settings);
    d.reverse();
    d
}

impl<BackendData: Backend> EvoState<BackendData> {
    /// App windows on screen, front-most first: what a show desktop sends away.
    fn desktop_windows(&self) -> Vec<WindowElement> {
        self.space
            .elements()
            .rev()
            .filter(|w| {
                !w.is_shell_window()
                    && !w.is_overlay()
                    && self.workspace_manager.state(w).is_some()
                    && !self.workspace_manager.is_minimized(w)
            })
            .cloned()
            .collect()
    }

    /// Super+D: Recede what is on screen, or Emerge what the last one sent
    /// away (see the module header).
    pub fn show_desktop(&mut self) {
        let on_screen = self.desktop_windows();
        let away: Vec<WindowElement> = RECEDED.with(|r| {
            r.borrow()
                .iter()
                .filter(|w| w.alive() && self.workspace_manager.is_minimized(w))
                .cloned()
                .collect()
        });
        match toggle(on_screen.len(), away.len()) {
            Toggle::Emerge => self.emerge_desktop(away),
            Toggle::Recede => self.recede_desktop(on_screen),
        }
    }

    fn recede_desktop(&mut self, windows: Vec<WindowElement>) {
        let delays = stagger_delays(windows.len(), &anim::settings());
        let order: Vec<u32> = windows.iter().filter_map(|w| self.window_id_for(w)).collect();
        // Ghosts are drawn newest on top and leave the Space at once, so they
        // keep the stack's order only if they are made back to front. The
        // delays still go front-most first.
        for (window, delay) in windows.iter().zip(delays).rev() {
            self.minimize_window_as(window, MinimizeStyle::Recede(delay));
        }
        tracing::info!("show desktop: {} window(s) receded", windows.len());
        report(|r| {
            r.shown = !windows.is_empty();
            r.recedes += windows.len() as u64;
            r.recede_order = order;
        });
        RECEDED.with(|r| *r.borrow_mut() = windows);
    }

    fn emerge_desktop(&mut self, windows: Vec<WindowElement>) {
        let delays = emerge_delays(windows.len(), &anim::settings());
        // Back to front: each one mapped goes on top, so the stack comes
        // back as it was, and the back-most starts first.
        let mut order = Vec::new();
        for (window, delay) in windows.iter().zip(delays).rev() {
            self.unminimize_window_as(window, RestoreStyle::Emerge(delay));
            order.extend(self.window_id_for(window));
        }
        if let Some(front) = windows.first() {
            self.focus_window(front);
        }
        tracing::info!("show desktop: {} window(s) emerged", windows.len());
        report(|r| {
            r.shown = false;
            r.emerges += windows.len() as u64;
            r.emerge_order = order;
        });
        RECEDED.with(|r| r.borrow_mut().clear());
    }

    /// The output `window` is on (else the first).
    fn recede_output(&self, window: &WindowElement) -> Option<smithay::output::Output> {
        self.space
            .outputs_for_element(window)
            .first()
            .cloned()
            .or_else(|| self.space.outputs().next().cloned())
    }

    /// The picture a Recede or Emerge is drawn from, taken now; `None` (the
    /// live window is drawn under the same motion, unblurred) in reduced mode,
    /// with Lumen off or failed, or with no snapshot.
    fn recede_fx(&mut self, window: &WindowElement, settings: &anim::AnimationSettings) -> Option<Box<RecedeFx>> {
        if !settings.motion() || !lumen::usable() {
            return None;
        }
        let output = self.recede_output(window)?;
        let began = std::time::Instant::now();
        let snapshot = self
            .backend_data
            .snapshot_window(window, output.current_scale().fractional_scale())?;
        tracing::info!(
            "lumen recede: snapshot of {}x{} took {:.1} ms",
            snapshot.size.w,
            snapshot.size.h,
            began.elapsed().as_secs_f64() * 1000.0
        );
        Some(Box::new(RecedeFx::new(snapshot)))
    }

    /// Draw `window` receding, starting `delay` from now. Call while it is
    /// still in the Space. Does nothing (it vanishes) when it may not animate.
    pub(crate) fn begin_recede_animation(&mut self, window: &WindowElement, delay: Duration) {
        if !window.has_opened() || !self.animation_eligible(window) || !self.is_window_mapped(window) {
            return;
        }
        let Some(location) = self.space.element_location(window) else {
            return;
        };
        let geometry = SpaceElement::geometry(window);
        let frame = Rectangle::new(location, geometry.size);
        let settings = anim::settings();
        // An open or restore still running is dropped: the recede starts from
        // the window as it is meant to be (they are short, and a show desktop
        // is a deliberate gesture).
        window.cancel_animation();
        let Some(animation) = poses::recede(&settings, anim::now() + delay) else {
            return;
        };
        let fx = self.recede_fx(window, &settings);
        let anchor = self.space.elements().take_while(|w| *w != window).last().cloned();
        ghosts::add(Ghost {
            source: GhostSource::Live(window.clone()),
            owner: Some(window.clone()),
            anim: WindowAnim::new(AnimKind::Minimize, animation).with_recede(fx),
            anchor,
            frame,
            render_origin: frame.loc - geometry.loc,
        });
    }

    /// Draw `window` emerging, starting `delay` from now. Call once it is back
    /// in the Space. `ghost` is its Recede if that was still running: the
    /// Emerge starts from its pose, on its picture.
    pub(crate) fn begin_emerge_animation(&mut self, window: &WindowElement, ghost: Option<Ghost>, delay: Duration) {
        if !window.has_opened() || !self.animation_eligible(window) || !self.is_window_mapped(window) {
            return;
        }
        let settings = anim::settings();
        let now = anim::now();
        let (from, carried) = match ghost {
            Some(mut g) => {
                let from = EmergeFrom::sample(&g.anim.anim, now);
                (from, g.anim.recede.take())
            }
            None => (EmergeFrom::receded(), None),
        };
        let Some(animation) = poses::emerge(&settings, now + delay, from) else {
            return;
        };
        let fx = match carried.filter(|_| settings.motion()) {
            Some(fx) => Some(fx),
            None => self.recede_fx(window, &settings),
        };
        window.start_animation(WindowAnim::new(AnimKind::Restore, animation).with_recede(fx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn super_d_emerges_only_when_the_screen_is_clear_and_someone_is_away() {
        assert_eq!(toggle(3, 0), Toggle::Recede, "first press");
        assert_eq!(toggle(0, 3), Toggle::Emerge, "second press");
        assert_eq!(toggle(1, 2), Toggle::Recede, "a dock restore ended it");
        assert_eq!(toggle(0, 0), Toggle::Recede, "nothing to bring back");
    }

    #[test]
    fn emerge_goes_back_most_first_with_the_same_stagger() {
        let full = anim::AnimationSettings::default();
        assert_eq!(
            emerge_delays(3, &full),
            vec![Duration::from_millis(90), Duration::from_millis(45), Duration::ZERO],
            "indexed front-most first: the back-most (last) starts at once"
        );
        let reduced = anim::AnimationSettings { mode: anim::Mode::Reduced, scale: 1.0 };
        assert!(emerge_delays(3, &reduced).iter().all(|d| d.is_zero()));
    }
}
