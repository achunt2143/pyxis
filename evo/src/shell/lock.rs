//! The lock screen's Evo half: whether the screen is locked, and the one place
//! that decides it may be unlocked. Design, ownership and the contracts both
//! sides build against: `docs/internal/pyxis-glass-and-lock-plan.md`.
//!
//! **Evo owns what is secret, Echo owns the decision, Bloom owns the look.** A
//! lock drawn by a web page must not be the only thing between a stranger and
//! the desktop, so Evo fails closed: while locked it draws only the overlay
//! (Bloom's lock page) over black and sends all input there or nowhere. If the
//! page crashes the screen stays black.
//!
//! # State
//!
//! `Core` is the whole state machine, pure and with an injected clock. It is
//! kept in a process-wide `Mutex` (like the overlay's region) because the
//! Echo broker thread feeds it (`LockChanged`, `evo.Lock`, `SetIdleLock`) and
//! the compositor thread acts on it. Two atomics mirror its phase for the hot
//! paths (every input event, every frame), which never take the lock.
//!
//! Phases: `Open`, `Pending`, `Locked`.
//!
//! - **Locking is immediate for input, one loop pass for the picture.** A
//!   request (Super+L, `evo.Lock`, idle, a `LockChanged {locked: true}`) moves
//!   `Open` to `Pending` at once: `input_locked()` is true from that instant,
//!   so a key or click arriving a millisecond later is already refused. The
//!   picture follows when the compositor thread's tick takes the snapshots for
//!   the Veil (`EvoState::lock_tick`, once per loop pass; Super+L runs it
//!   itself, so a key press has no wait at all) and makes the phase `Locked`.
//!   Until then Evo still draws the desktop, with input dead: not drawing it
//!   from the first frame would flash black before the Veil, since only the
//!   compositor thread can take the pictures. A tick that never comes would
//!   leave the desktop drawn, so after `PENDING_LIMIT` the frame is drawn
//!   locked whatever (`drawing_locked`).
//! - **Unlocking is only ever accepted from Echo, verified.** A
//!   `LockChanged {locked: false}` counts only with a mac that verifies
//!   against the session key and a `seq` above the last accepted
//!   (`Core::lock_changed`). It does not unlock at once: it queues, and the
//!   tick starts the Clear and opens the phase in the same pass, so no frame
//!   shows the desktop before its focus pull begins.
//!
//! # The key
//!
//! echo-daemon writes 32 random bytes, hex, to `/run/pyxis/session.key` (root
//! 0600; `PYXIS_SESSION_KEY` moves it, as it does for Echo). Every
//! `LockChanged` carries a `seq` and `mac` = HMAC-SHA256(key, `pyxis-lock:<0|1>:<seq>`).
//! The HMAC here is written the way `echo/src/services/session.rs` writes it,
//! over `sha2`, and the two contract vectors are tests on both sides. A failed
//! mac re-reads the file once, since echo-daemon may have restarted with a new
//! key. Locking needs no mac (locking is always safe), but a lock whose mac
//! does verify also raises the `seq` floor, so a captured older unlock can't
//! be replayed after a newer lock.
//!
//! This is a stopgap until the bus knows who is calling (CLAUDE.md, "The one
//! thing webOS got right").

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Mutex, MutexGuard,
    },
    time::Duration,
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use smithay::{
    desktop::space::SpaceElement,
    input::pointer::MotionEvent,
    utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER},
    wayland::seat::WaylandFocus,
};
use tracing::{debug, info, warn};

use super::WindowElement;
use crate::{
    anim::{
        self,
        bloom::{self, BloomFx},
        clear,
        ghosts::{self, Ghost, GhostSource},
        lumen,
        veil::{self, VeilFx},
        AnimKind, WindowAnim,
    },
    focus::KeyboardFocusTarget,
    state::Backend,
    EvoState,
};

pub const SESSION_IFACE: &str = "org.geckoOS.session";
pub const SIGNAL_LOCK_CHANGED: &str = "LockChanged";

/// Where echo-daemon puts the key on a device.
pub const KEY_PATH: &str = "/run/pyxis/session.key";

/// How long a lock may wait for the compositor thread's tick before the frame
/// is drawn locked anyway.
pub const PENDING_LIMIT: Duration = Duration::from_millis(100);

// ------------------------------------------------------------------- hmac

/// HMAC-SHA256 (RFC 2104) over `sha2`, block size 64. The same algorithm as
/// `echo/src/services/session.rs`, which the two sides must agree on byte for
/// byte; evo doesn't depend on echo's service code for this.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `a == b` without stopping at the first difference.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// The mac `LockChanged` carries: lowercase hex HMAC-SHA256 over
/// `pyxis-lock:<0|1>:<seq>` (1 is locked).
pub fn mac_for(key: &[u8; 32], locked: bool, seq: u64) -> String {
    to_hex(&hmac_sha256(key, format!("pyxis-lock:{}:{seq}", locked as u8).as_bytes()))
}

/// Whether `mac` is the right mac for `(locked, seq)` under `key`.
pub fn mac_matches(key: &[u8; 32], locked: bool, seq: u64, mac: &str) -> bool {
    constant_time_eq(mac_for(key, locked, seq).as_bytes(), mac.trim().to_ascii_lowercase().as_bytes())
}

/// The key file's text (64 hex characters) as bytes.
pub fn parse_key(text: &str) -> Option<[u8; 32]> {
    let hex = text.trim();
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(key)
}

/// Where this process reads the key: `PYXIS_SESSION_KEY`, else the device path.
pub fn key_path() -> PathBuf {
    std::env::var_os("PYXIS_SESSION_KEY").map(PathBuf::from).unwrap_or_else(|| KEY_PATH.into())
}

static KEY: Mutex<Option<[u8; 32]>> = Mutex::new(None);

fn load_key() -> Option<[u8; 32]> {
    let path = key_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let key = parse_key(&text);
            if key.is_none() {
                warn!("lock: {} is not 64 hex characters, no unlock can verify", path.display());
            }
            key
        }
        Err(e) => {
            debug!("lock: can't read {}: {e}", path.display());
            None
        }
    }
}

/// Whether the mac verifies against the key file: the key read before if there
/// is one, and, if that fails, the file read again (echo-daemon may have
/// restarted with a new key). No key, or none that matches, is a refusal.
pub fn verify_mac(locked: bool, seq: u64, mac: &str) -> bool {
    let mut cached = KEY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(key) = cached.as_ref() {
        if mac_matches(key, locked, seq, mac) {
            return true;
        }
    }
    *cached = load_key();
    cached.as_ref().is_some_and(|key| mac_matches(key, locked, seq, mac))
}

// ------------------------------------------------------------------- core

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Open,
    Pending,
    Locked,
}

/// Where a lock request came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Super+L.
    Key,
    /// `evo.Lock`.
    Request,
    /// The idle timer.
    Idle,
    /// A `LockChanged {locked: true}` from Echo, who already knows.
    Echo,
}

impl Source {
    fn tells_echo(self) -> bool {
        self != Source::Echo
    }
}

/// What a `LockChanged` signal came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Locked (or already was).
    Locked,
    /// An unlock whose mac verified and whose seq was new.
    Unlocked,
    /// A bad mac: counted, ignored.
    BadMac,
    /// A good mac on a seq that is not above the last accepted: counted, ignored.
    Replay,
}

/// What the compositor thread has to do this pass.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tick {
    /// Begin the lock (take the Veil's pictures); the flag is whether to tell Echo.
    pub begin_lock: Option<bool>,
    /// Begin the unlock; the origin is `SetLockOrigin`'s, if one was set.
    pub begin_unlock: Option<Option<(f64, f64)>>,
}

#[derive(Debug)]
pub struct Core {
    phase: Phase,
    pending_since: Duration,
    /// A lock waits for the tick to start its Veil, and whether to tell Echo.
    lock_due: Option<bool>,
    /// An accepted unlock waits for the tick to start the Clear.
    unlock_due: bool,
    last_seq: u64,
    refused_macs: u64,
    /// `None`: never.
    idle_after: Option<Duration>,
    last_input: Duration,
    origin: Option<(f64, f64)>,
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}

impl Core {
    pub const fn new() -> Self {
        Self {
            phase: Phase::Open,
            pending_since: Duration::ZERO,
            lock_due: None,
            unlock_due: false,
            last_seq: 0,
            refused_macs: 0,
            idle_after: None,
            last_input: Duration::ZERO,
            origin: None,
        }
    }

    // Read by the tests; the loop reads the atomics instead.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Input is refused from the moment a lock is asked for.
    pub fn input_locked(&self) -> bool {
        self.phase != Phase::Open
    }

    /// Whether a frame at `now` is drawn locked: yes once locked, and yes for
    /// a lock that has waited too long for its tick.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn drawing_locked(&self, now: Duration) -> bool {
        match self.phase {
            Phase::Locked => true,
            Phase::Pending => now.saturating_sub(self.pending_since) > PENDING_LIMIT,
            Phase::Open => false,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn refused_macs(&self) -> u64 {
        self.refused_macs
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn idle_after(&self) -> Option<Duration> {
        self.idle_after
    }

    pub fn idle_for(&self, now: Duration) -> Duration {
        now.saturating_sub(self.last_input)
    }

    pub fn note_input(&mut self, now: Duration) {
        self.last_input = now;
    }

    /// `SetIdleLock {seconds}`: 0 is never.
    pub fn set_idle_lock(&mut self, seconds: u64) {
        self.idle_after = (seconds > 0).then(|| Duration::from_secs(seconds));
    }

    pub fn set_origin(&mut self, x: f64, y: f64) {
        self.origin = Some((x, y));
    }

    /// Ask for the lock. Returns whether this began one (it does nothing
    /// when already locked or locking). A lock arriving while an accepted
    /// unlock still waits for its tick wins: the unlock is dropped.
    pub fn lock(&mut self, now: Duration, source: Source) -> bool {
        match self.phase {
            Phase::Open => {
                self.phase = Phase::Pending;
                self.pending_since = now;
                self.lock_due = Some(source.tells_echo());
                true
            }
            Phase::Pending => false,
            Phase::Locked => {
                self.unlock_due = false;
                false
            }
        }
    }

    /// A `LockChanged` from Echo. `mac_ok` is whether its mac verified.
    pub fn lock_changed(&mut self, now: Duration, locked: bool, seq: u64, mac_ok: bool) -> Verdict {
        if locked {
            // Locking is always safe, with or without a mac. One that verifies
            // also says everything before it is stale.
            if mac_ok && seq > self.last_seq {
                self.last_seq = seq;
            }
            self.lock(now, Source::Echo);
            return Verdict::Locked;
        }
        if !mac_ok {
            self.refused_macs += 1;
            return Verdict::BadMac;
        }
        if seq <= self.last_seq {
            self.refused_macs += 1;
            return Verdict::Replay;
        }
        self.last_seq = seq;
        match self.phase {
            Phase::Locked => self.unlock_due = true,
            // The picture was never locked: nothing to clear, just stop.
            Phase::Pending => {
                self.phase = Phase::Open;
                self.lock_due = None;
                self.last_input = now;
            }
            Phase::Open => {}
        }
        Verdict::Unlocked
    }

    /// One pass of the compositor thread's loop: the idle timer, then what is
    /// due. Moves `Pending` to `Locked` and an accepted unlock to `Open`.
    pub fn tick(&mut self, now: Duration) -> Tick {
        if self.phase == Phase::Open {
            if let Some(after) = self.idle_after {
                if self.idle_for(now) >= after {
                    self.lock(now, Source::Idle);
                }
            }
        }
        let mut out = Tick::default();
        if self.phase == Phase::Pending {
            if let Some(tell) = self.lock_due.take() {
                out.begin_lock = Some(tell);
            }
            self.phase = Phase::Locked;
        }
        if self.phase == Phase::Locked && self.unlock_due {
            self.unlock_due = false;
            self.phase = Phase::Open;
            // Whoever unlocked has just been using the machine.
            self.last_input = now;
            out.begin_unlock = Some(self.origin);
        }
        out
    }
}

// ----------------------------------------------------------------- globals

static CORE: Mutex<Core> = Mutex::new(Core::new());
const OPEN: u8 = 0;
const PENDING: u8 = 1;
const LOCKED: u8 = 2;
static PHASE: AtomicU8 = AtomicU8::new(OPEN);
/// When the pending lock began, in microseconds of the animation clock.
static PENDING_AT_US: AtomicU64 = AtomicU64::new(0);
/// Whether an overlay window exists, as the compositor thread last saw it.
static OVERLAY_PRESENT: AtomicBool = AtomicBool::new(false);

fn core() -> MutexGuard<'static, Core> {
    CORE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Publish `core`'s phase for the lock-free readers.
fn publish(core: &Core) {
    PENDING_AT_US.store(core.pending_since.as_micros() as u64, Ordering::Release);
    let phase = match core.phase {
        Phase::Open => OPEN,
        Phase::Pending => PENDING,
        Phase::Locked => LOCKED,
    };
    PHASE.store(phase, Ordering::Release);
}

/// Input is refused: a lock was asked for. The one test every input path makes.
pub fn input_locked() -> bool {
    PHASE.load(Ordering::Acquire) != OPEN
}

/// The frame is drawn locked: only the overlay, over black.
pub fn drawing_locked() -> bool {
    match PHASE.load(Ordering::Acquire) {
        LOCKED => true,
        PENDING => {
            let since = Duration::from_micros(PENDING_AT_US.load(Ordering::Acquire));
            anim::now().saturating_sub(since) > PENDING_LIMIT
        }
        _ => false,
    }
}

/// Ask for the lock. Returns whether it began one.
pub fn request_lock(source: Source) -> bool {
    let mut core = core();
    let began = core.lock(anim::now(), source);
    publish(&core);
    if began {
        info!("lock: requested ({source:?})");
    }
    began
}

/// Something the user did (a key, a pointer, a touch): the idle timer starts over.
pub fn note_input() {
    core().note_input(anim::now());
}

/// `SetIdleLock {seconds}`.
pub fn set_idle_lock(seconds: u64) {
    core().set_idle_lock(seconds);
    info!("lock: idle lock after {}", if seconds == 0 { "never".to_string() } else { format!("{seconds} s") });
}

/// `SetLockOrigin {x, y}`: where the Clear focuses from, output-logical px.
pub fn set_origin(x: f64, y: f64) {
    core().set_origin(x, y);
}

/// Whether the compositor thread found an overlay window this pass.
pub fn note_overlay(present: bool) {
    OVERLAY_PRESENT.store(present, Ordering::Relaxed);
}

/// Handle a `LockChanged` signal's body. Returns a line for the log.
pub fn on_lock_changed(body: &Value) -> String {
    let (Some(locked), Some(seq), Some(mac)) = (
        body.get("locked").and_then(Value::as_bool),
        body.get("seq").and_then(Value::as_u64),
        body.get("mac").and_then(Value::as_str),
    ) else {
        // Not a signal of Echo's making. A locked one needs no mac, but this
        // doesn't even say which way it goes, so it is ignored.
        core().refused_macs += 1;
        return "lock: a LockChanged with no locked/seq/mac, ignored".to_string();
    };
    // Only an unlock (and a lock, for its floor) needs the key; a lock with a
    // bad mac still locks, so the file is read for it only when it is cheap.
    let mac_ok = verify_mac(locked, seq, mac);
    let mut core = core();
    let verdict = core.lock_changed(anim::now(), locked, seq, mac_ok);
    publish(&core);
    match verdict {
        Verdict::Locked => format!("lock: LockChanged locked (seq {seq}, mac {})", if mac_ok { "ok" } else { "unverified" }),
        Verdict::Unlocked => format!("lock: LockChanged unlock accepted (seq {seq})"),
        Verdict::BadMac => format!("lock: refused an unlock with a bad mac (seq {seq}); {} refused so far", core.refused_macs),
        Verdict::Replay => format!("lock: refused an unlock replaying seq {seq} (last accepted {})", core.last_seq),
    }
}

/// One pass of the loop: what the compositor thread has to do.
pub fn tick() -> Tick {
    let mut core = core();
    let out = core.tick(anim::now());
    publish(&core);
    out
}

/// `GetLockState`'s answer (echo-cli only).
pub fn state_json() -> Value {
    let core = core();
    let now = anim::now();
    let stats = anim::stats();
    let running = |kind: AnimKind| stats.started(kind) > stats.completed(kind);
    json!({
        "locked": core.input_locked(),
        "phase": match core.phase { Phase::Open => "open", Phase::Pending => "pending", Phase::Locked => "locked" },
        "overlay": OVERLAY_PRESENT.load(Ordering::Relaxed),
        "last_seq": core.last_seq,
        "idle_lock_after": core.idle_after.map_or(0, |d| d.as_secs()),
        "idle_seconds": core.idle_for(now).as_secs(),
        "veil": running(AnimKind::Veil),
        "clear": running(AnimKind::Clear),
        "frames": {
            "veil": lumen::frames(lumen::Effect::Veil),
            "clear": lumen::frames(lumen::Effect::Clear),
        },
        "refused_macs": core.refused_macs,
    })
}

/// `{x, y}` from `SetLockOrigin`'s body.
pub fn parse_origin(body: &Value) -> Result<(f64, f64), String> {
    let n = |name: &str| {
        body.get(name)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && v.abs() <= 1e6)
            .ok_or_else(|| format!("{name} must be a finite number"))
    };
    Ok((n("x")?, n("y")?))
}

/// `seconds` from `SetIdleLock`'s body: whole seconds, 0 is never.
pub fn parse_idle_seconds(body: &Value) -> Result<u64, String> {
    let s = body
        .get("seconds")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 86_400.0 * 7.0)
        .ok_or_else(|| "seconds must be a number from 0 (never) to a week".to_string())?;
    Ok(s.round() as u64)
}

// ----------------------------------------------------------- the compositor

impl<BackendData: Backend> EvoState<BackendData> {
    /// Super+L and `evo.Lock`'s compositor-thread entry: lock now, without
    /// waiting for the next loop pass, so a key press has no black or bare
    /// desktop frame before the Veil.
    pub fn lock_screen(&mut self, source: Source) {
        request_lock(source);
        self.lock_tick();
    }

    /// Once per loop pass (`process_pending_window_actions`): the idle timer,
    /// starting a lock's Veil or an unlock's Clear, and, while locked,
    /// holding the keyboard on the overlay.
    pub fn lock_tick(&mut self) {
        let overlay_alive = self.space.elements().any(|w| w.is_overlay() && w.alive());
        note_overlay(overlay_alive);
        let tick = tick();
        if let Some(tell_echo) = tick.begin_lock {
            self.begin_lock(tell_echo);
        }
        if let Some(origin) = tick.begin_unlock {
            self.begin_unlock(origin);
        }
        if input_locked() {
            self.hold_lock();
        }
    }

    /// Everything in flight ends, the Veil starts, and Echo is told.
    fn begin_lock(&mut self, tell_echo: bool) {
        info!("lock: locked");
        self.switcher_cancel();
        self.release_grabs();
        // Nothing animated is drawn while locked, so an animation that was
        // running would never be drawn to its end and so never counted
        // completed. End them all.
        for window in self.space.elements().cloned().collect::<Vec<_>>() {
            window.cancel_animation();
            window.cancel_geometry();
        }
        ghosts::clear();
        self.start_veil();
        self.hold_lock();
        self.fix_locked_pointer();
        #[cfg(feature = "echo-ipc")]
        if tell_echo {
            if let Err(e) = self.echo_service.read().session_lock_remote() {
                warn!("lock: could not tell Echo: {e:#}");
            }
        }
        #[cfg(not(feature = "echo-ipc"))]
        let _ = tell_echo;
    }

    /// A ghost of each app window that was on screen, blurring and fading
    /// under the lock page (`anim::veil`). Back to front, so they keep their
    /// stack order.
    fn start_veil(&mut self) {
        let settings = anim::settings();
        let now = anim::now();
        let windows = self.desktop_windows();
        for window in windows.iter().rev() {
            if !self.animation_eligible(window) {
                continue;
            }
            let Some(location) = self.space.element_location(window) else {
                continue;
            };
            let Some(pose) = veil::transform(&settings, now) else {
                // Off: instant. No ghost, so not a pixel of any window.
                continue;
            };
            let geometry = SpaceElement::geometry(window);
            let fx = self.veil_fx(window, &settings);
            ghosts::add(Ghost {
                // The live window is the plain fallback (the same fade, no
                // blur) when the picture can't be drawn.
                source: GhostSource::Live(window.clone()),
                owner: Some(window.clone()),
                anim: WindowAnim::new(AnimKind::Veil, pose).with_veil(fx),
                anchor: None,
                frame: Rectangle::new(location, geometry.size),
                render_origin: location - geometry.loc,
            });
        }
    }

    /// The picture a Veil draws, taken now. `None` (the live window fades
    /// plain) in reduced mode, with Lumen off or failed, or with no snapshot.
    fn veil_fx(&mut self, window: &WindowElement, settings: &anim::AnimationSettings) -> Option<Box<VeilFx>> {
        if !settings.motion() || !lumen::usable() {
            return None;
        }
        let output = self
            .space
            .outputs_for_element(window)
            .first()
            .cloned()
            .or_else(|| self.space.outputs().next().cloned())?;
        let began = std::time::Instant::now();
        let snapshot = self
            .backend_data
            .snapshot_window(window, output.current_scale().fractional_scale())?;
        debug!(
            "lumen veil: snapshot of {}x{} took {:.1} ms",
            snapshot.size.w,
            snapshot.size.h,
            began.elapsed().as_secs_f64() * 1000.0
        );
        Some(Box::new(VeilFx::new(snapshot, window.picture_radii())))
    }

    /// The Clear: the lock is over, so the desktop and every window come into
    /// focus about the field (`anim::clear`). Drawn on the live surfaces
    /// through Bloom's shader.
    fn begin_unlock(&mut self, origin: Option<(f64, f64)>) {
        info!("lock: unlocked");
        // A Veil still running ends where it stands (the page it fades under
        // is gone).
        ghosts::clear();
        // Ensure any work area or output changes received while locked are
        // applied before the Clear reveal starts and published on the next sync.
        self.refit_to_work_area();
        #[cfg(feature = "echo-ipc")]
        self.mark_toplevel_sync_dirty();
        let settings = anim::settings();
        let now = anim::now();
        let origin = origin.map(Point::from).unwrap_or_else(|| self.output_centre());
        let shader = bloom::available();
        for window in self.space.elements().cloned().collect::<Vec<_>>() {
            // The overlay is the lock page itself; a window that has not
            // opened yet will open with its own animation.
            if window.is_overlay() || !(window.is_shell() || window.has_opened()) {
                continue;
            }
            let Some(pose) = clear::transform(&settings, now, origin) else {
                continue;
            };
            let fx = if shader { BloomFx::clear(&settings, now, window.frame_radii()) } else { None };
            window.start_animation(WindowAnim::new(AnimKind::Clear, pose).with_bloom(fx));
        }
        // The keyboard was held on the overlay (or on nobody, if the shell
        // died) through the lock: hand it to the front app, as a panel closing
        // would. Not gated on the overlay's keyboard note: that is only kept
        // for focus the overlay asked for, and a lock's focus was forced, so
        // on device the gate skipped the hand-back and no window had the
        // keyboard after an unlock.
        let leaving = self
            .space
            .elements()
            .find(|w| w.is_overlay() && w.alive())
            .or_else(|| self.space.elements().find(|w| w.is_shell() && w.alive()))
            .cloned();
        match leaving {
            Some(leaving) => self.refocus_after_loss(&leaving),
            None => {
                // No shell at all: the front-most app that is on screen.
                let front = self
                    .space
                    .elements()
                    .rev()
                    .find(|w| w.alive() && !w.is_shell_window() && !self.workspace_manager.is_minimized(w))
                    .cloned();
                if let Some(window) = front {
                    self.focus_window(&window);
                }
            }
        }
        self.refresh_pointer_focus();
    }

    /// The output's centre, output-logical: where Clear focuses from when
    /// Bloom never said.
    fn output_centre(&self) -> Point<f64, Logical> {
        self.space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
            .map(|g| Point::from((g.loc.x as f64 + g.size.w as f64 / 2.0, g.loc.y as f64 + g.size.h as f64 / 2.0)))
            .unwrap_or_default()
    }

    /// Every pass while locked: the keyboard stays on the overlay, finished
    /// window animations are dropped (nothing draws them to sweep them), and
    /// ghosts that are not a Veil are gone.
    fn hold_lock(&mut self) {
        self.enforce_lock_focus();
        self.fix_locked_pointer();
        ghosts::retain_kind(AnimKind::Veil);
        let now = anim::now();
        for window in self.space.elements() {
            window.animation_transform(now, Rectangle::default());
        }
    }

    /// While locked the keyboard is the overlay's, whatever asked for it: a
    /// new window mapping, xdg-activation, a click, the overlay dying and a
    /// new one mapping. With no overlay, nobody has it.
    pub(crate) fn enforce_lock_focus(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let want: Option<KeyboardFocusTarget> = self
            .space
            .elements()
            .find(|w| w.is_overlay() && w.alive())
            .cloned()
            .map(Into::into);
        if keyboard.current_focus() != want {
            keyboard.set_focus(self, want, SERIAL_COUNTER.next_serial());
        }
    }

    /// While locked the pointer can only be over the overlay. An app that had
    /// it must be told it left now, not at the next motion, or a click or a
    /// scroll in between would still reach it.
    pub(crate) fn fix_locked_pointer(&mut self) {
        if !input_locked() {
            return;
        }
        self.refresh_pointer_focus();
    }

    /// Re-aim the pointer's focus at what is under it now, if that differs
    /// from what has it.
    pub(crate) fn refresh_pointer_focus(&mut self) {
        let pointer = self.pointer.clone();
        let location = pointer.current_location();
        let under = self.surface_under(location);
        let want = under.as_ref().and_then(|(t, _)| t.wl_surface().map(|s| s.into_owned()));
        let have = pointer.current_focus().and_then(|t| t.wl_surface().map(|s| s.into_owned()));
        if want == have {
            return;
        }
        pointer.motion(
            self,
            under,
            &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time: 0 },
        );
        pointer.frame(self);
    }

    /// A drag, a resize, a popup grab, a touch in progress: all end as the
    /// lock begins, so nothing an app grabbed outlives it.
    fn release_grabs(&mut self) {
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.unset_grab(self, serial, 0);
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.unset_grab(self);
        }
        if let Some(touch) = self.seat.get_touch() {
            touch.cancel(self);
            touch.unset_grab(self);
        }
    }

    /// What the overlay has under `pos` while locked: its surface wherever it
    /// is, with no input region to consult (the whole screen is the lock
    /// page's). With no overlay, nothing.
    pub(crate) fn locked_surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(crate::focus::PointerFocusTarget, Point<f64, Logical>)> {
        let overlay = self.space.elements().find(|w| w.is_overlay() && w.alive())?;
        let render_loc = self.space.element_location(overlay)? - overlay.geometry().loc;
        let (surface, loc) = overlay.surface_under(pos - render_loc.to_f64(), smithay::desktop::WindowSurfaceType::ALL)?;
        Some((surface, (loc + render_loc).to_f64()))
    }
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
pub(crate) static GLOBAL_TEST: Mutex<()> = Mutex::new(());

/// Put the process-wide lock state back as a test found it.
#[cfg(test)]
pub(crate) fn test_reset() {
    let mut c = core();
    *c = Core::new();
    publish(&c);
    *KEY.lock().unwrap_or_else(|e| e.into_inner()) = None;
    std::env::remove_var("PYXIS_SESSION_KEY");
}

#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use self::tests::{test_lock_guard, TestLockGuard};

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn contract_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        for (i, b) in key.iter_mut().enumerate() {
            *b = i as u8;
        }
        key
    }

    #[test]
    fn the_contracts_mac_vectors() {
        let key = contract_key();
        assert_eq!(
            mac_for(&key, false, 1_700_000_000_000),
            "50465067a085d8970c228bb83ed55ebb8011f06989f7240bb1eb3ed7d5a1ccee"
        );
        assert_eq!(
            mac_for(&key, true, 1_700_000_000_001),
            "3c117518d88f28b34392c9b3775cec378fd836d6a816685289f38d74b6fe21c0"
        );
    }

    #[test]
    fn hmac_matches_rfc_4231_case_2_and_a_long_key() {
        // RFC 4231 test case 2.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(to_hex(&mac), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        // Test case 6: a key longer than the block is hashed first.
        let mac = hmac_sha256(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(to_hex(&mac), "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54");
    }

    #[test]
    fn a_mac_matches_only_its_own_state_and_seq() {
        let key = contract_key();
        let mac = mac_for(&key, false, 100);
        assert!(mac_matches(&key, false, 100, &mac));
        assert!(mac_matches(&key, false, 100, &mac.to_uppercase()), "hex case doesn't matter");
        assert!(!mac_matches(&key, true, 100, &mac), "an unlock's mac is not a lock's");
        assert!(!mac_matches(&key, false, 101, &mac));
        assert!(!mac_matches(&key, false, 100, ""));
        assert!(!mac_matches(&key, false, 100, &mac[..63]));
        let mut other = contract_key();
        other[0] ^= 1;
        assert!(!mac_matches(&other, false, 100, &mac), "another key");
    }

    #[test]
    fn the_key_file_is_64_hex_characters() {
        let key = parse_key("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\n").unwrap();
        assert_eq!(key, contract_key());
        assert!(parse_key("00").is_none());
        assert!(parse_key(&"zz".repeat(32)).is_none());
        assert!(parse_key(&"0".repeat(65)).is_none());
        assert!(parse_key("").is_none());
    }

    // ---- the state machine ----

    #[test]
    fn a_lock_request_refuses_input_at_once_and_the_tick_makes_it_locked() {
        let mut c = Core::new();
        assert!(!c.input_locked());
        assert!(c.lock(ms(10), Source::Key));
        assert!(c.input_locked(), "input is dead from the request");
        assert_eq!(c.phase(), Phase::Pending);
        assert!(!c.drawing_locked(ms(50)), "the picture waits for the tick");
        let t = c.tick(ms(16));
        assert_eq!(t.begin_lock, Some(true), "Evo's own lock tells Echo");
        assert_eq!(c.phase(), Phase::Locked);
        assert!(c.drawing_locked(ms(16)));
        // Nothing more to begin on the next pass.
        assert_eq!(c.tick(ms(32)), Tick::default());
    }

    #[test]
    fn a_tick_that_never_comes_still_draws_locked_after_the_limit() {
        let mut c = Core::new();
        c.lock(ms(1000), Source::Request);
        assert!(!c.drawing_locked(ms(1000) + PENDING_LIMIT));
        assert!(c.drawing_locked(ms(1000) + PENDING_LIMIT + ms(1)));
    }

    #[test]
    fn every_source_locks_and_only_echoes_do_not_call_echo_back() {
        for (source, tells) in [
            (Source::Key, true),
            (Source::Request, true),
            (Source::Idle, true),
            (Source::Echo, false),
        ] {
            let mut c = Core::new();
            assert!(c.lock(ms(0), source));
            assert_eq!(c.tick(ms(1)).begin_lock, Some(tells), "{source:?}");
        }
    }

    #[test]
    fn locking_twice_is_one_lock_and_super_l_while_locked_does_nothing() {
        let mut c = Core::new();
        assert!(c.lock(ms(0), Source::Key));
        assert!(!c.lock(ms(1), Source::Key), "already pending");
        c.tick(ms(2));
        assert!(!c.lock(ms(3), Source::Key), "already locked");
        assert_eq!(c.tick(ms(4)), Tick::default(), "no second Veil");
    }

    #[test]
    fn a_lock_signal_from_echo_locks_with_or_without_a_mac() {
        for mac_ok in [true, false] {
            let mut c = Core::new();
            assert_eq!(c.lock_changed(ms(0), true, 500, mac_ok), Verdict::Locked);
            assert!(c.input_locked());
            assert_eq!(c.refused_macs(), 0, "a lock is never refused");
        }
    }

    #[test]
    fn a_verified_unlock_with_a_new_seq_unlocks_on_the_next_tick() {
        let mut c = Core::new();
        c.lock(ms(0), Source::Key);
        c.tick(ms(1));
        assert_eq!(c.lock_changed(ms(100), false, 1000, true), Verdict::Unlocked);
        assert!(c.input_locked(), "still locked until the tick starts the Clear");
        assert!(c.drawing_locked(ms(100)));
        c.set_origin(640.0, 700.0);
        let t = c.tick(ms(116));
        assert_eq!(t.begin_unlock, Some(Some((640.0, 700.0))));
        assert!(!c.input_locked());
        assert_eq!(c.phase(), Phase::Open);
        assert_eq!(c.last_seq(), 1000);
        assert_eq!(c.tick(ms(132)), Tick::default());
    }

    #[test]
    fn an_unlock_without_an_origin_says_so() {
        let mut c = Core::new();
        c.lock(ms(0), Source::Key);
        c.tick(ms(1));
        c.lock_changed(ms(2), false, 5, true);
        assert_eq!(c.tick(ms(3)).begin_unlock, Some(None), "the compositor uses the centre");
    }

    #[test]
    fn a_forged_unlock_is_refused_counted_and_changes_nothing() {
        let mut c = Core::new();
        c.lock(ms(0), Source::Key);
        c.tick(ms(1));
        assert_eq!(c.lock_changed(ms(2), false, 9_999_999_999_999, false), Verdict::BadMac);
        assert_eq!(c.lock_changed(ms(3), false, 9_999_999_999_998, false), Verdict::BadMac);
        assert_eq!(c.refused_macs(), 2);
        assert!(c.input_locked());
        assert_eq!(c.last_seq(), 0, "a forgery moves nothing, the seq floor included");
        assert_eq!(c.tick(ms(4)), Tick::default());
        // The real one still works afterwards.
        assert_eq!(c.lock_changed(ms(5), false, 10, true), Verdict::Unlocked);
    }

    #[test]
    fn a_replayed_unlock_is_refused_even_with_a_good_mac() {
        let mut c = Core::new();
        c.lock(ms(0), Source::Key);
        c.tick(ms(1));
        assert_eq!(c.lock_changed(ms(2), false, 100, true), Verdict::Unlocked);
        c.tick(ms(3));
        // Locked again; the old unlock is captured and replayed.
        c.lock(ms(4), Source::Key);
        c.tick(ms(5));
        assert_eq!(c.lock_changed(ms(6), false, 100, true), Verdict::Replay, "same seq");
        assert_eq!(c.lock_changed(ms(7), false, 50, true), Verdict::Replay, "older seq");
        assert_eq!(c.refused_macs(), 2);
        assert!(c.input_locked());
        assert_eq!(c.lock_changed(ms(8), false, 101, true), Verdict::Unlocked, "a newer one is fine");
    }

    #[test]
    fn a_verified_lock_makes_any_older_unlock_stale() {
        // An unlock Echo sent while Evo was away, kept by someone, then the
        // screen locks (seq 300) and the old unlock (seq 200) is replayed.
        let mut c = Core::new();
        c.lock_changed(ms(0), true, 300, true);
        c.tick(ms(1));
        assert_eq!(c.lock_changed(ms(2), false, 200, true), Verdict::Replay);
        assert!(c.input_locked());
        // An unverified lock cannot raise the floor (a forger could lock the
        // seq space shut otherwise).
        let mut c = Core::new();
        c.lock_changed(ms(0), true, u64::MAX, false);
        c.tick(ms(1));
        assert_eq!(c.last_seq(), 0);
        assert_eq!(c.lock_changed(ms(2), false, 7, true), Verdict::Unlocked);
    }

    #[test]
    fn the_last_seq_is_kept_across_locks() {
        let mut c = Core::new();
        for (i, seq) in [100u64, 200, 300].into_iter().enumerate() {
            c.lock(ms(i as u64 * 10), Source::Key);
            c.tick(ms(i as u64 * 10 + 1));
            assert_eq!(c.lock_changed(ms(i as u64 * 10 + 2), false, seq, true), Verdict::Unlocked);
            c.tick(ms(i as u64 * 10 + 3));
            assert_eq!(c.last_seq(), seq);
        }
    }

    #[test]
    fn an_unlock_before_the_tick_cancels_the_lock_without_a_clear() {
        let mut c = Core::new();
        c.lock(ms(0), Source::Request);
        assert_eq!(c.lock_changed(ms(1), false, 10, true), Verdict::Unlocked);
        assert!(!c.input_locked());
        assert_eq!(c.tick(ms(2)), Tick::default(), "never locked, so nothing to veil or clear");
    }

    #[test]
    fn a_lock_after_an_accepted_unlock_but_before_its_tick_keeps_the_screen_locked() {
        let mut c = Core::new();
        c.lock(ms(0), Source::Key);
        c.tick(ms(1));
        c.lock_changed(ms(2), false, 10, true);
        c.lock(ms(3), Source::Key);
        assert_eq!(c.tick(ms(4)), Tick::default());
        assert!(c.input_locked());
    }

    #[test]
    fn an_unlock_while_open_only_moves_the_seq() {
        let mut c = Core::new();
        assert_eq!(c.lock_changed(ms(0), false, 40, true), Verdict::Unlocked);
        assert!(!c.input_locked());
        assert_eq!(c.last_seq(), 40);
        assert_eq!(c.tick(ms(1)), Tick::default());
    }

    // ---- idle ----

    #[test]
    fn idle_locks_after_the_time_with_no_input() {
        let mut c = Core::new();
        c.set_idle_lock(5);
        c.note_input(ms(1000));
        assert_eq!(c.tick(ms(5900)), Tick::default());
        assert!(!c.input_locked());
        let t = c.tick(ms(6000));
        assert_eq!(t.begin_lock, Some(true), "idle is Evo's own lock, so it tells Echo");
        assert!(c.input_locked());
    }

    #[test]
    fn input_restarts_the_idle_timer() {
        let mut c = Core::new();
        c.set_idle_lock(5);
        c.note_input(ms(0));
        c.tick(ms(4000));
        c.note_input(ms(4500));
        c.tick(ms(8000));
        assert!(!c.input_locked(), "only 3.5 s since the key");
        c.tick(ms(9500));
        assert!(c.input_locked());
    }

    #[test]
    fn zero_means_never_and_it_does_not_idle_while_locked_or_after_an_unlock() {
        let mut c = Core::new();
        c.set_idle_lock(5);
        c.set_idle_lock(0);
        assert_eq!(c.idle_after(), None);
        c.tick(ms(1_000_000));
        assert!(!c.input_locked());
        c.set_idle_lock(5);
        c.tick(ms(1_000_001));
        assert!(c.input_locked());
        // Unlock: the clock starts over from the unlock, so it doesn't relock at once.
        c.lock_changed(ms(1_000_002), false, 9, true);
        let t = c.tick(ms(1_000_003));
        assert!(t.begin_unlock.is_some());
        assert_eq!(c.tick(ms(1_000_004)), Tick::default());
        assert!(!c.input_locked());
        c.tick(ms(1_000_003) + ms(5000));
        assert!(c.input_locked(), "and idles again 5 s later");
    }

    #[test]
    fn idle_seconds_counts_from_the_last_input() {
        let mut c = Core::new();
        c.note_input(ms(2000));
        assert_eq!(c.idle_for(ms(9500)).as_secs(), 7);
        assert_eq!(c.idle_for(ms(1000)).as_secs(), 0, "a clock behind the input is not negative");
    }

    // ---- the bodies the Echo thread parses ----

    #[test]
    fn origin_and_idle_bodies_are_parsed_strictly() {
        assert_eq!(parse_origin(&json!({"x": 640, "y": 700.5})), Ok((640.0, 700.5)));
        assert!(parse_origin(&json!({"x": 1})).is_err());
        assert!(parse_origin(&json!({"x": "a", "y": 1})).is_err());
        assert!(parse_origin(&json!({"x": 1e12, "y": 1})).is_err());
        assert_eq!(parse_idle_seconds(&json!({"seconds": 5})), Ok(5));
        assert_eq!(parse_idle_seconds(&json!({"seconds": 0})), Ok(0));
        assert!(parse_idle_seconds(&json!({"seconds": -1})).is_err());
        assert!(parse_idle_seconds(&json!({})).is_err());
        assert!(parse_idle_seconds(&json!({"seconds": 1e12})).is_err());
    }

    // ---- the globals (one test, since they are process-wide) ----

    /// Serialize process-wide lock tests and guarantee reset on drop.
    #[cfg(test)]
    pub(crate) struct TestLockGuard(#[allow(dead_code)] MutexGuard<'static, ()>);

    #[cfg(test)]
    impl Drop for TestLockGuard {
        fn drop(&mut self) {
            test_reset();
        }
    }

    #[cfg(test)]
    pub(crate) fn test_lock_guard() -> TestLockGuard {
        let guard = GLOBAL_TEST.lock().unwrap_or_else(|e| e.into_inner());
        test_reset();
        TestLockGuard(guard)
    }

    /// Resets the process-wide state when a test ends, however it ends.
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            test_reset();
        }
    }

    #[test]
    fn the_signal_path_end_to_end_with_a_key_file() {
        let _g = GLOBAL_TEST.lock().unwrap_or_else(|e| e.into_inner());
        let _reset = Reset;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.key");
        let key = contract_key();
        std::fs::write(&path, to_hex(&key)).unwrap();
        std::env::set_var("PYXIS_SESSION_KEY", &path);

        assert!(!input_locked());
        // Echo locks.
        let lock = json!({"locked": true, "seq": 1000, "mac": mac_for(&key, true, 1000)});
        assert!(on_lock_changed(&lock).contains("locked"));
        assert!(input_locked());
        tick();
        assert!(drawing_locked());

        // A forged unlock: wrong mac.
        let forged = json!({"locked": false, "seq": 5000, "mac": "00".repeat(32)});
        assert!(on_lock_changed(&forged).contains("bad mac"));
        // A forged unlock that is missing fields.
        on_lock_changed(&json!({"locked": false}));
        assert!(input_locked());
        assert_eq!(state_json()["refused_macs"], 2);

        // The right one. Not yet open until the tick.
        let good = json!({"locked": false, "seq": 2000, "mac": mac_for(&key, false, 2000)});
        assert!(on_lock_changed(&good).contains("accepted"));
        assert!(input_locked());
        let t = tick();
        assert!(t.begin_unlock.is_some());
        assert!(!input_locked());
        assert!(!drawing_locked());
        assert_eq!(state_json()["last_seq"], 2000);

        // Locked again, replaying that one is refused.
        request_lock(Source::Key);
        tick();
        assert!(on_lock_changed(&good).contains("replaying"));
        assert!(input_locked());
        assert_eq!(state_json()["refused_macs"], 3);
    }

    #[test]
    fn a_new_key_after_an_echo_restart_is_read_when_a_mac_fails() {
        let _g = GLOBAL_TEST.lock().unwrap_or_else(|e| e.into_inner());
        let _reset = Reset;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.key");
        let old = contract_key();
        let mut new = contract_key();
        new[31] ^= 0xff;
        std::fs::write(&path, to_hex(&old)).unwrap();
        std::env::set_var("PYXIS_SESSION_KEY", &path);
        assert!(verify_mac(false, 1, &mac_for(&old, false, 1)), "reads and caches the key");
        // echo-daemon restarts with a new key.
        std::fs::write(&path, to_hex(&new)).unwrap();
        assert!(verify_mac(false, 2, &mac_for(&new, false, 2)), "a failed mac re-reads the file");
        assert!(!verify_mac(false, 3, &mac_for(&old, false, 3)), "and the old key is no good now");
    }

    #[test]
    fn no_key_file_means_no_unlock_ever() {
        let _g = GLOBAL_TEST.lock().unwrap_or_else(|e| e.into_inner());
        let _reset = Reset;
        std::env::set_var("PYXIS_SESSION_KEY", "/nonexistent/pyxis/session.key");
        assert!(!verify_mac(false, 1, &mac_for(&contract_key(), false, 1)));
        assert!(!verify_mac(false, 1, &"0".repeat(64)));
    }
}
