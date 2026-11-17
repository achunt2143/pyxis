//! Busy windows: `xdg_wm_base.ping`, the pong, and the spinner.
//!
//! An app's client is pinged when one of its windows takes the keyboard or is
//! clicked, and every `PING_EVERY` otherwise. A ping not answered within
//! `PONG_WITHIN` makes the client busy until its next pong, and every window
//! of a busy client draws the remochi spinner in its three control slots
//! (`ssd.rs`). Close keeps working while busy: it is what you reach for.
//!
//! The state is per client (a ping is to a client, not a window), kept in the
//! smithay `ShellClient`'s data map, so it goes when the client does. The
//! state machine (`PingState`) and the spinner's frames (`morph`) are pure,
//! with injected time, and tested here.

use std::{sync::Mutex, time::Duration};

use smithay::{
    desktop::WindowSurface,
    utils::SERIAL_COUNTER,
    wayland::shell::{xdg::ShellClient, PingError},
};

use super::WindowElement;
use crate::{state::Backend, EvoState};

/// How often a client is pinged when nothing else prompts it.
pub const PING_EVERY: Duration = Duration::from_secs(5);
/// How long a client has to answer before it is busy.
pub const PONG_WITHIN: Duration = Duration::from_secs(2);
/// The spinner's loop: circle, square, triangle.
pub const SPIN_PERIOD: Duration = Duration::from_millis(1100);

/// One client's pings, on `crate::anim::now`'s clock.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PingState {
    /// When the unanswered ping went out.
    pending: Option<Duration>,
    /// When the last ping went out, answered or not.
    last_sent: Option<Duration>,
}

impl PingState {
    /// Whether to ping now. `eager` (focus, a click) pings at once; otherwise
    /// every `PING_EVERY`. Never while one is out: smithay allows one at a
    /// time, and the pending one already measures the wait.
    pub fn wants_ping(&self, now: Duration, eager: bool) -> bool {
        if self.pending.is_some() {
            return false;
        }
        eager || self.last_sent.is_none_or(|t| now.saturating_sub(t) >= PING_EVERY)
    }

    pub fn sent(&mut self, now: Duration) {
        self.pending = Some(now);
        self.last_sent = Some(now);
    }

    pub fn pong(&mut self) {
        self.pending = None;
    }

    /// Busy: a ping has gone unanswered for `PONG_WITHIN` or longer.
    pub fn busy(&self, now: Duration) -> bool {
        self.pending.is_some_and(|t| now.saturating_sub(t) >= PONG_WITHIN)
    }
}

/// The state in a client's data map.
#[derive(Default)]
struct ClientPing(Mutex<PingState>);

fn with_state<T>(client: &ShellClient, f: impl FnOnce(&mut PingState) -> T) -> Option<T> {
    client
        .with_data(|data| {
            data.insert_if_missing_threadsafe(ClientPing::default);
            let slot = data.get::<ClientPing>()?;
            let mut state = slot.0.lock().ok()?;
            Some(f(&mut state))
        })
        .ok()
        .flatten()
}

impl WindowElement {
    /// The xdg client behind this window (none for X11).
    fn shell_client(&self) -> Option<ShellClient> {
        match self.0.underlying_surface() {
            WindowSurface::Wayland(toplevel) => Some(toplevel.client()),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(_) => None,
        }
    }

    /// Whether this window's client has stopped answering pings. Never for
    /// the shell, the overlay or an X11 window.
    pub fn is_busy(&self) -> bool {
        if self.is_shell_window() {
            return false;
        }
        let now = crate::anim::now();
        self.shell_client().and_then(|c| with_state(&c, |s| s.busy(now))).unwrap_or(false)
    }
}

impl<B: Backend> EvoState<B> {
    /// Ping `window`'s client if it is due (or at once, `eager`). Only app
    /// windows: the shell is not judged.
    pub fn ping_window(&mut self, window: &WindowElement, eager: bool) {
        if window.is_shell_window() {
            return;
        }
        let Some(client) = window.shell_client() else { return };
        if !client.alive() {
            return;
        }
        let now = crate::anim::now();
        if with_state(&client, |s| s.wants_ping(now, eager)) != Some(true) {
            return;
        }
        match client.send_ping(SERIAL_COUNTER.next_serial()) {
            Ok(()) => {
                with_state(&client, |s| s.sent(now));
            }
            // Someone else's ping is out (none of ours is, or we would not be
            // here): time the wait from now.
            Err(PingError::PingAlreadyPending(_)) => {
                with_state(&client, |s| s.sent(now));
            }
            Err(_) => {}
        }
    }

    /// Every loop pass: ping each app window's client that is due.
    pub fn busy_tick(&mut self) {
        let windows: Vec<WindowElement> = self.windows_by_id.values().cloned().collect();
        for window in windows {
            self.ping_window(&window, false);
        }
    }

    /// A client answered (`XdgShellHandler::client_pong`).
    pub fn note_pong(&mut self, client: &ShellClient) {
        let was_busy = with_state(client, |s| {
            let busy = s.busy(crate::anim::now());
            s.pong();
            busy
        });
        if was_busy == Some(true) {
            tracing::info!("busy: a client answered its ping again");
        }
    }
}

// === The spinner ===

/// The three shapes, in the spinner's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Circle,
    Square,
    Triangle,
}

/// One shape of a frame: how opaque, how big (1 = its control's size) and
/// how turned (degrees, negative is anticlockwise).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Morph {
    pub shape: Shape,
    pub alpha: f64,
    pub scale: f64,
    pub turn: f64,
}

/// What slot `slot` (0 the left, minimize; 2 the right, close) shows at
/// `phase` (0..1 of `SPIN_PERIOD`). Each slot runs a third ahead of the one
/// on its left, so the three always show three different shapes. Within its
/// third a shape appears whole, shrinks to 0.8 turning 12° over 78% of it,
/// then fades out shrinking to 0.6 at 20°: the mockup's busy row.
pub fn morph(slot: usize, phase: f64) -> Morph {
    let k = (phase + slot as f64 / 3.0).rem_euclid(1.0) * 3.0;
    let shape = match k as usize {
        0 => Shape::Circle,
        1 => Shape::Square,
        _ => Shape::Triangle,
    };
    let u = k.fract();
    const HOLD: f64 = 0.78;
    if u < HOLD {
        let t = u / HOLD;
        Morph { shape, alpha: 1.0, scale: 1.0 - 0.2 * t, turn: -12.0 * t }
    } else {
        let t = (u - HOLD) / (1.0 - HOLD);
        Morph { shape, alpha: 1.0 - t, scale: 0.8 - 0.2 * t, turn: -12.0 - 8.0 * t }
    }
}

/// The phase of the spinner at `now`, 0..1.
pub fn phase(now: Duration) -> f64 {
    (now.as_secs_f64() / SPIN_PERIOD.as_secs_f64()).fract()
}

/// Reduced motion: each slot keeps its own shape and blinks its colour on
/// for a third of the loop, a third out of step with its neighbour.
pub fn blink_on(slot: usize, phase: f64) -> bool {
    ((phase + slot as f64 / 3.0).rem_euclid(1.0) * 3.0) < 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: f64) -> Duration {
        Duration::from_secs_f64(n)
    }

    #[test]
    fn busy_only_after_a_missed_pong_and_not_before() {
        let mut p = PingState::default();
        assert!(p.wants_ping(s(0.0), false), "never pinged: due");
        p.sent(s(10.0));
        assert!(!p.busy(s(10.0)));
        assert!(!p.busy(s(11.9)), "still inside the two seconds");
        assert!(p.busy(s(12.0)));
        assert!(p.busy(s(60.0)), "busy until it answers");
    }

    #[test]
    fn a_pong_clears_busy() {
        let mut p = PingState::default();
        p.sent(s(10.0));
        assert!(p.busy(s(13.0)));
        p.pong();
        assert!(!p.busy(s(13.0)));
        assert!(!p.busy(s(100.0)));
    }

    #[test]
    fn pings_go_every_five_seconds_or_at_once_on_focus_but_one_at_a_time() {
        let mut p = PingState::default();
        p.sent(s(10.0));
        assert!(!p.wants_ping(s(20.0), true), "one is out: not even eagerly");
        p.pong();
        assert!(!p.wants_ping(s(14.0), false));
        assert!(p.wants_ping(s(15.0), false));
        assert!(p.wants_ping(s(11.0), true), "focus or a click: at once");
    }

    #[test]
    fn the_three_slots_show_three_shapes_a_third_apart() {
        for i in 0..100 {
            let ph = i as f64 / 100.0;
            let shapes: Vec<Shape> = (0..3).map(|slot| morph(slot, ph).shape).collect();
            assert_ne!(shapes[0], shapes[1]);
            assert_ne!(shapes[1], shapes[2]);
            assert_ne!(shapes[0], shapes[2]);
        }
        // Circle, then square, then triangle in one slot over the loop.
        assert_eq!(morph(0, 0.0).shape, Shape::Circle);
        assert_eq!(morph(0, 0.34).shape, Shape::Square);
        assert_eq!(morph(0, 0.67).shape, Shape::Triangle);
        // The next slot runs a third ahead.
        assert_eq!(morph(1, 0.0).shape, Shape::Square);
    }

    #[test]
    fn a_shape_appears_whole_and_leaves_small_turned_and_faded() {
        let start = morph(0, 0.0);
        assert_eq!((start.alpha, start.scale, start.turn), (1.0, 1.0, 0.0));
        let late = morph(0, 0.3);
        assert!(late.alpha < 0.5 && late.scale < 0.8 && late.turn < -12.0);
        for i in 0..300 {
            let m = morph(2, i as f64 / 300.0);
            assert!((0.0..=1.0).contains(&m.alpha) && (0.6..=1.0).contains(&m.scale));
        }
    }

    #[test]
    fn reduced_blinks_each_slot_for_a_third() {
        let on: usize = (0..300).filter(|i| blink_on(1, *i as f64 / 300.0)).count();
        assert_eq!(on, 100);
        assert_ne!(blink_on(0, 0.1), blink_on(1, 0.1));
    }
}
