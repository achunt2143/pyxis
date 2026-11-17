//! Alt+Tab. Evo keeps the key and the recency order (an app has the
//! keyboard, so only Evo sees Alt+Tab); Bloom draws the switcher, in its
//! overview, from the `org.geckoOS.evo` signal `SwitcherChanged`.
//!
//! `Switcher` is the pure state machine (time is injected, windows are
//! generic) and `EvoState`'s methods below wire it to the keyboard, the
//! overlay and `focus_window`.
//!
//! Protocol, as the shell hears it:
//! - `open`: Alt is still held 150 ms after the first Tab, so the shell shows
//!   its overview with `selected` highlighted. Never sent for a quick tap.
//! - `step`: another Tab (or Shift+Tab backwards) while it is shown.
//! - `commit`: Alt released. Evo has already focused `selected` (restoring it
//!   if minimized). Sent for a quick tap too, *without* an `open` before it:
//!   a shell that was not showing anything ignores it.
//! - `cancel`: Escape while Alt is held. Focus stays where it was.
//!
//! Every signal carries `selected` and `order` (window ids, most recently
//! used first, so `order[0]` is where the walk started).
//!
//! With no overlay alive there is no shell to draw it, so Evo falls back to
//! the old direct cycling: each step focuses a window.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Alt released within this of the first Tab is a quick switch: no overview.
pub const QUICK_TAP: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Open,
    Step,
    Commit,
    Cancel,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Open => "open",
            Phase::Step => "step",
            Phase::Commit => "commit",
            Phase::Cancel => "cancel",
        }
    }
}

/// What the state machine wants done.
#[derive(Debug, Clone, PartialEq)]
pub enum Out<T> {
    /// Tell the shell.
    Signal { phase: Phase, selected: T, order: Vec<T> },
    /// Give this window the keyboard (restoring it if minimized).
    Focus(T),
}

/// An Alt+Tab in progress. `order` is frozen at the first press: each focus
/// change reshuffles the live history, which would move windows under the walk.
#[derive(Debug)]
pub struct Switcher<T> {
    order: Vec<T>,
    index: usize,
    started: Duration,
    /// A shell is drawing it (else each step focuses directly).
    shell: bool,
    shown: bool,
}

impl<T: Clone + PartialEq> Switcher<T> {
    /// Start a walk at `index` of `order`; the first Tab is a `step` on top
    /// of this. `shell`: whether an overlay is alive to draw it.
    pub fn begin(order: Vec<T>, index: usize, now: Duration, shell: bool) -> Self {
        let index = index.min(order.len().saturating_sub(1));
        Self { order, index, started: now, shell, shown: false }
    }

    pub fn selected(&self) -> Option<&T> {
        self.order.get(self.index)
    }

    pub fn order(&self) -> &[T] {
        &self.order
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    fn signal(&self, phase: Phase) -> Option<Out<T>> {
        self.selected().map(|s| Out::Signal { phase, selected: s.clone(), order: self.order.clone() })
    }

    /// Drop windows that died mid-walk, keeping the selection where it was.
    pub fn prune(&mut self, alive: impl Fn(&T) -> bool) {
        let selected = self.selected().cloned();
        self.order.retain(|w| alive(w));
        self.index = match selected.and_then(|s| self.order.iter().position(|w| *w == s)) {
            Some(i) => i,
            None => self.index.min(self.order.len().saturating_sub(1)),
        };
    }

    /// Tab (or Shift+Tab, `reverse`).
    pub fn step(&mut self, reverse: bool) -> Vec<Out<T>> {
        if self.order.is_empty() {
            return Vec::new();
        }
        self.index = super::placement::cycle_step(self.order.len(), self.index, reverse);
        if !self.shell {
            return self.selected().cloned().map(Out::Focus).into_iter().collect();
        }
        if self.shown {
            return self.signal(Phase::Step).into_iter().collect();
        }
        Vec::new()
    }

    /// Time passing. Alt still held `QUICK_TAP` after the first Tab: open.
    pub fn tick(&mut self, now: Duration) -> Vec<Out<T>> {
        if !self.shell || self.shown || now.saturating_sub(self.started) < QUICK_TAP {
            return Vec::new();
        }
        self.shown = true;
        self.signal(Phase::Open).into_iter().collect()
    }

    /// Alt released: the selection wins.
    pub fn release(self) -> Vec<Out<T>> {
        if !self.shell {
            return Vec::new(); // every step already focused its window
        }
        let mut out: Vec<Out<T>> = self.selected().cloned().map(Out::Focus).into_iter().collect();
        out.extend(self.signal(Phase::Commit));
        out
    }

    /// Escape: leave everything as it was. Direct stepping has already moved
    /// focus, so that mode can't undo it, and says nothing.
    pub fn cancel(self) -> Vec<Out<T>> {
        if !self.shell {
            return Vec::new();
        }
        self.signal(Phase::Cancel).into_iter().collect()
    }

    /// Something else picked `window` while the walk was on (a card clicked
    /// in the overview): that is the commit, with the window already focused.
    pub fn choose(mut self, window: &T) -> Vec<Out<T>> {
        if !self.shell {
            return Vec::new();
        }
        if let Some(i) = self.order.iter().position(|w| w == window) {
            self.index = i;
        }
        self.signal(Phase::Commit).into_iter().collect()
    }
}

/// The last thing the switcher did, for `GetSwitcherState`: the Echo thread
/// can't see `EvoState`, and suites need to read it with `echo-cli`.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// A walk is on.
    pub active: bool,
    /// The shell has been told to show it (`open` sent, no `commit`/`cancel` since).
    pub shown: bool,
    pub selected: Option<u32>,
    pub order: Vec<u32>,
    pub last_phase: Option<&'static str>,
    /// How many of each signal went out, in `open`, `step`, `commit`,
    /// `cancel` order.
    pub counts: [u64; 4],
    /// The window the last `commit` chose.
    pub last_commit: Option<u32>,
}

static SNAPSHOT: Mutex<Option<Snapshot>> = Mutex::new(None);

fn with_snapshot<R>(f: impl FnOnce(&mut Snapshot) -> R) -> R {
    let mut guard = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Snapshot::default))
}

pub fn snapshot() -> Snapshot {
    with_snapshot(|s| s.clone())
}

fn record_walk(walk: Option<&Switcher<u32>>) {
    with_snapshot(|s| {
        s.active = walk.is_some();
        s.shown = walk.is_some_and(|w| w.shown());
        s.selected = walk.and_then(|w| w.selected().copied());
        s.order = walk.map(|w| w.order().to_vec()).unwrap_or_default();
    });
}

fn record_signal(phase: Phase, selected: u32) {
    with_snapshot(|s| {
        s.last_phase = Some(phase.as_str());
        s.counts[phase as usize] += 1;
        match phase {
            Phase::Open => s.shown = true,
            Phase::Commit => {
                s.shown = false;
                s.last_commit = Some(selected);
            }
            Phase::Cancel => s.shown = false,
            Phase::Step => {}
        }
    });
}

fn epoch() -> Duration {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed()
}

use smithay::utils::IsAlive;

use crate::shell::{element::WindowElement, placement::StackRole};
use crate::state::{Backend, EvoState};

impl<BackendData: Backend> EvoState<BackendData> {
    /// Whether an Alt+Tab walk is on.
    pub fn switcher_active(&self) -> bool {
        self.alt_tab.is_some()
    }

    fn overlay_alive(&self) -> bool {
        self.space.elements().any(|w| w.is_overlay() && w.alive())
    }

    /// Alt+Tab / Shift+Alt+Tab.
    pub fn cycle_windows(&mut self, reverse: bool) {
        self.cycle_windows_matching(reverse, false);
    }

    /// Alt+Grave / Alt+Shift+Grave: cycle windows belonging to the focused
    /// window's `app_id`.
    pub fn cycle_app_windows(&mut self, reverse: bool) {
        self.cycle_windows_matching(reverse, true);
    }

    fn cycle_windows_matching(&mut self, reverse: bool, same_app_only: bool) {
        self.focus_history.retain(|w| w.alive());
        if self.alt_tab.is_none() {
            let focused = self
                .seat
                .get_keyboard()
                .and_then(|k| k.current_focus())
                .and_then(|f| smithay::wayland::seat::WaylandFocus::wl_surface(&f).map(|s| s.into_owned()))
                .and_then(|s| self.tracked_window_for_surface(&s));
            let required_app_id = if same_app_only {
                let Some(app_id) = focused
                    .as_ref()
                    .filter(|w| w.stack_role() == StackRole::App)
                    .and_then(|w| w.app_id())
                else {
                    return;
                };
                Some(app_id)
            } else {
                None
            };

            let live: Vec<WindowElement> = self
                .workspace_manager
                .active_workspace()
                .windows()
                .iter()
                .filter(|w| {
                    w.alive()
                        && w.stack_role() == StackRole::App
                        && self.window_id_for(w).is_some()
                        && required_app_id
                            .as_ref()
                            .is_none_or(|id| w.app_id().as_deref() == Some(id.as_str()))
                })
                .cloned()
                .collect();
            let order = super::placement::cycle_order(&self.focus_history, &live);
            if order.len() < 2 {
                return;
            }
            // With the desktop focused, the first step lands on the most
            // recent app (or the oldest, going backwards), not the second.
            let index = match focused {
                Some(f) if f == order[0] => 0,
                _ if reverse => 0,
                _ => order.len() - 1,
            };
            let ids: Vec<u32> = order.iter().filter_map(|w| self.window_id_for(w)).collect();
            self.alt_tab = Some(Switcher::begin(ids, index, epoch(), self.overlay_alive()));
        }
        self.prune_switcher();
        let Some(walk) = self.alt_tab.as_mut() else { return };
        let out = walk.step(reverse);
        self.after_switcher(out);
    }

    /// Per event-loop iteration: opens the shell's view once Alt has been
    /// held past a quick tap.
    pub fn switcher_tick(&mut self) {
        let Some(walk) = self.alt_tab.as_mut() else { return };
        let out = walk.tick(epoch());
        self.after_switcher(out);
    }

    /// Alt let go.
    pub fn switcher_release(&mut self) {
        self.prune_switcher();
        if let Some(walk) = self.alt_tab.take() {
            let out = walk.release();
            self.after_switcher(out);
        }
        record_walk(None);
    }

    /// Escape while Alt is held.
    pub fn switcher_cancel(&mut self) {
        if let Some(walk) = self.alt_tab.take() {
            let out = walk.cancel();
            self.after_switcher(out);
        }
        record_walk(None);
    }

    /// A window was focused from outside the keyboard (a card clicked in the
    /// switcher, the dock): that is the commit, so Alt's release must not
    /// override it.
    pub fn switcher_chose(&mut self, window: &WindowElement) {
        let Some(id) = self.window_id_for(window) else { return };
        if let Some(walk) = self.alt_tab.take() {
            let out = walk.choose(&id);
            self.after_switcher(out);
        }
        record_walk(None);
    }

    fn prune_switcher(&mut self) {
        let alive: std::collections::HashSet<u32> = self
            .windows_by_id
            .iter()
            .filter(|(_, w)| w.alive())
            .map(|(&id, _)| id)
            .collect();
        if let Some(walk) = self.alt_tab.as_mut() {
            walk.prune(|id| alive.contains(id));
        }
    }

    fn after_switcher(&mut self, out: Vec<Out<u32>>) {
        for o in out {
            match o {
                Out::Focus(id) => {
                    if let Some(window) = self.windows_by_id.get(&id).cloned() {
                        self.focus_window(&window);
                    }
                }
                Out::Signal { phase, selected, order } => {
                    record_signal(phase, selected);
                    #[cfg(feature = "echo-ipc")]
                    let _ = self.echo_service.read().emit_switcher_changed(phase.as_str(), selected, &order);
                    #[cfg(not(feature = "echo-ipc"))]
                    let _ = order;
                }
            }
        }
        record_walk(self.alt_tab.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn signal(phase: Phase, selected: u32, order: &[u32]) -> Out<u32> {
        Out::Signal { phase, selected, order: order.to_vec() }
    }

    #[test]
    fn held_alt_opens_after_the_quick_tap_window_then_steps_and_commits() {
        let order = vec![1, 2, 3];
        let mut s = Switcher::begin(order.clone(), 0, ms(1000), true);
        assert!(s.step(false).is_empty(), "the first Tab says nothing yet");
        assert_eq!(s.selected(), Some(&2));
        assert!(s.tick(ms(1149)).is_empty(), "still a quick tap");
        assert_eq!(s.tick(ms(1150)), vec![signal(Phase::Open, 2, &order)]);
        assert!(s.tick(ms(1400)).is_empty(), "open is sent once");
        assert_eq!(s.step(false), vec![signal(Phase::Step, 3, &order)]);
        assert_eq!(s.step(true), vec![signal(Phase::Step, 2, &order)]);
        assert_eq!(s.release(), vec![Out::Focus(2), signal(Phase::Commit, 2, &order)]);
    }

    #[test]
    fn a_quick_tap_commits_without_the_overview_ever_opening() {
        let order = vec![1, 2];
        let mut s = Switcher::begin(order.clone(), 0, ms(0), true);
        s.step(false);
        assert!(s.tick(ms(100)).is_empty());
        assert!(!s.shown());
        assert_eq!(s.release(), vec![Out::Focus(2), signal(Phase::Commit, 2, &order)]);
    }

    #[test]
    fn steps_before_the_overview_shows_are_silent_but_count() {
        let order = vec![1, 2, 3];
        let mut s = Switcher::begin(order.clone(), 0, ms(0), true);
        s.step(false);
        s.step(false);
        assert_eq!(s.release(), vec![Out::Focus(3), signal(Phase::Commit, 3, &order)]);
    }

    #[test]
    fn shift_goes_backwards_and_wraps() {
        let mut s = Switcher::begin(vec![1, 2, 3], 0, ms(0), true);
        s.step(true);
        assert_eq!(s.selected(), Some(&3));
    }

    #[test]
    fn cancel_leaves_focus_alone() {
        let order = vec![1, 2];
        let mut s = Switcher::begin(order.clone(), 0, ms(0), true);
        s.step(false);
        s.tick(ms(200));
        assert_eq!(s.cancel(), vec![signal(Phase::Cancel, 2, &order)]);
    }

    #[test]
    fn a_chosen_window_is_the_commit() {
        let order = vec![1, 2, 3];
        let mut s = Switcher::begin(order.clone(), 0, ms(0), true);
        s.step(false);
        s.tick(ms(200));
        assert_eq!(s.choose(&3), vec![signal(Phase::Commit, 3, &order)]);
    }

    #[test]
    fn with_no_shell_every_step_focuses_and_nothing_is_said() {
        let mut s = Switcher::begin(vec![1, 2, 3], 0, ms(0), false);
        assert_eq!(s.step(false), vec![Out::Focus(2)]);
        assert_eq!(s.step(false), vec![Out::Focus(3)]);
        assert!(s.tick(ms(5000)).is_empty(), "no overview without a shell");
        assert!(s.clone_cancel().is_empty());
        let s = Switcher::begin(vec![1, 2], 0, ms(0), false);
        assert!(s.release().is_empty());
    }

    #[test]
    fn a_window_that_dies_mid_walk_leaves_the_selection_on_a_live_one() {
        let mut s = Switcher::begin(vec![1, 2, 3], 0, ms(0), true);
        s.step(false);
        s.prune(|w| *w != 2);
        assert_eq!(s.order(), &[1, 3]);
        assert_eq!(s.selected(), Some(&3));
        s.prune(|w| *w != 1);
        assert_eq!(s.selected(), Some(&3));
        s.prune(|_| false);
        assert!(s.selected().is_none());
        assert!(s.step(false).is_empty());
    }

    impl<T: Clone + PartialEq> Switcher<T> {
        fn clone_cancel(&self) -> Vec<Out<T>> {
            Switcher { order: self.order.clone(), index: self.index, started: self.started, shell: self.shell, shown: self.shown }
                .cancel()
        }
    }
}
