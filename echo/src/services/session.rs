//! The session: whether the screen is locked, and the one place that decides
//! it may be unlocked.
//!
//! The rule is that Evo owns what is secret, Echo owns the decision and Bloom
//! owns the look (docs/internal/pyxis-glass-and-lock-plan.md). Evo draws
//! nothing but the lock page while locked; this service checks the password
//! and tells Evo when it may let go. The page can ask to lock, and can ask to
//! unlock, but only a right password (checked here, as root) ends it.
//!
//! **The password** is checked against `/etc/shadow` with `crypt_r(3)` from
//! libxcrypt, for the session user (`geckouser`), the way `pam_unix` would for
//! a local account; the build host has no PAM headers, and PAM can replace
//! this behind the same method later. A user with no password (`*`, `!` or an
//! empty field, as the image ships) unlocks with anything.
//!
//! **The rate limit** is here, not in the page, and survives an overlay
//! restart: 5 wrong tries, then a wait of 30 s that doubles with every further
//! wrong try up to 15 min, reset by a right password. A try during a wait is
//! refused and doesn't count. Changing a password with a wrong `current`
//! counts too (`accounts.rs` shares the `Limiter`). Counters are per user name
//! and kept in `/run/pyxis/auth-limits.json` (`PYXIS_AUTH_LIMITS` moves it;
//! root 0600, tmpfs), so a daemon restart doesn't reset them.
//!
//! **How Evo is told, with no identity on the bus.** Any client on the 0666
//! socket could emit a `LockChanged {locked: false}`, so Evo believes only a
//! mac. At start echo-daemon writes 32 random bytes (hex) to
//! `/run/pyxis/session.key`, root 0600; every `LockChanged` carries a `seq`
//! that only goes up and `mac` = HMAC-SHA256(key, "pyxis-lock:<0|1>:<seq>").
//! Evo locks on any change that says locked, and unlocks only on a mac it can
//! verify with the key file (which geckouser and pyxisapp can't read). A
//! stopgap until the bus knows who is calling (see CLAUDE.md, "The one thing
//! webOS got right").
//!
//! `PYXIS_SESSION_KEY` moves the key file, and `PYXIS_ACCOUNTS_ROOT` (see
//! accounts.rs) moves `/etc/shadow`, so tests and a dev echo-daemon don't
//! touch the host.

use std::ffi::{c_char, c_void, CStr, CString};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
#[cfg(test)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::accounts::{hash_is_set, Users};

pub const SESSION_IFACE: &str = "org.geckoOS.session";
pub const METHOD_LOCK: &str = "lock";
pub const METHOD_UNLOCK: &str = "unlock";
pub const METHOD_SWITCH_USER: &str = "switchUser";
pub const METHOD_GET_STATE: &str = "getState";
pub const METHOD_LOGOUT: &str = "logout";
pub const SIGNAL_LOCK_CHANGED: &str = "LockChanged";
pub const SIGNAL_UNLOCK_FAILED: &str = "UnlockFailed";

/// The default primary session user.
pub const SESSION_USER: &str = "geckouser";

static ACTIVE_SESSION_USER: std::sync::OnceLock<parking_lot::RwLock<String>> =
    std::sync::OnceLock::new();
static ACTIVE_LIMITER: std::sync::OnceLock<parking_lot::RwLock<Option<Arc<Limiter>>>> =
    std::sync::OnceLock::new();
static ACTIVE_SESSION: std::sync::OnceLock<parking_lot::RwLock<Option<std::sync::Weak<SessionService>>>> =
    std::sync::OnceLock::new();

fn global_active_user_slot() -> &'static parking_lot::RwLock<String> {
    ACTIVE_SESSION_USER.get_or_init(|| parking_lot::RwLock::new(SESSION_USER.to_string()))
}

fn global_limiter_slot() -> &'static parking_lot::RwLock<Option<Arc<Limiter>>> {
    ACTIVE_LIMITER.get_or_init(|| parking_lot::RwLock::new(None))
}

fn global_session_slot() -> &'static parking_lot::RwLock<Option<std::sync::Weak<SessionService>>> {
    ACTIVE_SESSION.get_or_init(|| parking_lot::RwLock::new(None))
}

fn shared_limiter() -> Arc<Limiter> {
    if let Some(existing) = global_limiter_slot().read().clone() {
        return existing;
    }
    let mut guard = global_limiter_slot().write();
    if let Some(existing) = guard.clone() {
        return existing;
    }
    let fresh = Arc::new(Limiter::new());
    *guard = Some(fresh.clone());
    fresh
}

pub fn current_session_user() -> String {
    if let Some(svc) = global_session_slot()
        .read()
        .as_ref()
        .and_then(|w| w.upgrade())
    {
        return svc.active_user();
    }
    global_active_user_slot().read().clone()
}

fn set_global_active_user(user: &str) {
    *global_active_user_slot().write() = user.to_string();
}

/// Structured failure detail returned when checking the active session user's
/// password fails or hits rate-limit cooldown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordCheckFailure {
    pub reason: &'static str,
    pub message: String,
    pub attempts_left: Option<u32>,
    pub retry_at_ms: Option<u64>,
}

/// Shared password verification + rate-limiting check against `/etc/shadow`
/// (or `Users`'s root) for `user`. An account with no password (`*`, `!` or
/// empty) is `Granted`: that is press-to-enter on the lock screen. Anything
/// that gates a privilege uses [`check_user_password_strict`] instead.
fn check_user_password(
    users: &Users,
    limiter: &Limiter,
    user: &str,
    password: &str,
    now_ms: u64,
) -> Result<Check> {
    check_user_password_mode(users, limiter, user, password, now_ms, false)
}

/// Like [`check_user_password`], but an account with no password is refused
/// (`reason: "no-password"`, "set a password first") rather than granted.
pub fn check_user_password_strict(
    users: &Users,
    limiter: &Limiter,
    user: &str,
    password: &str,
    now_ms: u64,
) -> Result<Check> {
    check_user_password_mode(users, limiter, user, password, now_ms, true)
}

fn check_user_password_mode(
    users: &Users,
    limiter: &Limiter,
    user: &str,
    password: &str,
    now_ms: u64,
    strict: bool,
) -> Result<Check> {
    users.require_real(user)?;
    match users.password_hash(user)? {
        Some(h) if hash_is_set(&h) => {
            Ok(match limiter.attempt(user, now_ms, || verify_password(&h, password)) {
                Attempt::Passed => Check::Granted,
                Attempt::Wrong { attempts_left, retry_at_ms } => {
                    Check::Refused { reason: "wrong", attempts_left, retry_at_ms }
                }
                Attempt::Wait { retry_at_ms } => Check::Refused {
                    reason: "wait",
                    attempts_left: 0,
                    retry_at_ms: Some(retry_at_ms),
                },
            })
        }
        _ if strict => Ok(Check::Refused { reason: "no-password", attempts_left: 0, retry_at_ms: None }),
        _ => Ok(Check::Granted),
    }
}

/// Strict: used for privileged access, so an account with no password fails
/// with `reason: "no-password"`.
fn verify_user_password_with_detailed(
    users: &Users,
    limiter: &Limiter,
    user: &str,
    password: &str,
    now_ms: u64,
) -> std::result::Result<String, PasswordCheckFailure> {
    match check_user_password_strict(users, limiter, user, password, now_ms) {
        Ok(Check::Granted) => Ok(user.to_string()),
        Ok(Check::Refused { reason: "no-password", .. }) => Err(PasswordCheckFailure {
            reason: "no-password",
            message: "set a password first".to_string(),
            attempts_left: None,
            retry_at_ms: None,
        }),
        Ok(Check::Refused { reason: "wait", attempts_left, retry_at_ms }) => Err(PasswordCheckFailure {
            reason: "wait",
            message: format!("Too many failed attempts; wait until {}ms", retry_at_ms.unwrap_or(0)),
            attempts_left: Some(attempts_left),
            retry_at_ms,
        }),
        Ok(Check::Refused { attempts_left, retry_at_ms, .. }) => {
            let message = if let Some(until) = retry_at_ms {
                format!(
                    "Incorrect password (too many attempts; retry after {}ms)",
                    until
                )
            } else {
                format!("Incorrect password ({} attempts left)", attempts_left)
            };
            Err(PasswordCheckFailure {
                reason: "wrong",
                message,
                attempts_left: Some(attempts_left),
                retry_at_ms,
            })
        }
        Err(e) => Err(PasswordCheckFailure {
            reason: "error",
            message: format!("{e:#}"),
            attempts_left: None,
            retry_at_ms: None,
        }),
    }
}

#[allow(dead_code)]
fn verify_user_password_with(
    users: &Users,
    limiter: &Limiter,
    user: &str,
    password: &str,
    now_ms: u64,
) -> std::result::Result<String, String> {
    verify_user_password_with_detailed(users, limiter, user, password, now_ms)
        .map_err(|f| f.message)
}

/// Verifies `password` against the currently active session user's shadow
/// entry, sharing the exact lock-screen rate limiter (`Limiter`) and active
/// session user state, returning structured cooldown fields (`attempts_left`,
/// `retry_at_ms`) on refusal.
pub fn verify_active_user_password_detailed(
    password: &str,
) -> std::result::Result<String, PasswordCheckFailure> {
    if let Some(svc) = global_session_slot()
        .read()
        .as_ref()
        .and_then(|w| w.upgrade())
    {
        let user = svc.active_user();
        return verify_user_password_with_detailed(&svc.users, &svc.limiter, &user, password, now_ms());
    }
    let user = current_session_user();
    let users = Users::system();
    let limiter = shared_limiter();
    verify_user_password_with_detailed(&users, &limiter, &user, password, now_ms())
}

/// Verifies `password` against the currently active session user's shadow
/// entry, sharing the exact lock-screen rate limiter (`Limiter`) and active
/// session user state. Returns `Ok(username)` on success or `Err(reason)` on
/// failure/lockout.
pub fn verify_active_user_password(password: &str) -> std::result::Result<String, String> {
    verify_active_user_password_detailed(password).map_err(|f| f.message)
}

/// Where the key lives on a device.
pub const KEY_PATH: &str = "/run/pyxis/session.key";

/// The settings key that makes `power.sleep` lock first.
pub const LOCK_ON_SLEEP_SETTING: &str = "pyxis.lock.onSleep";

/// Wrong tries before the first wait.
pub const FREE_TRIES: u32 = 5;
const FIRST_WAIT_MS: u64 = 30_000;
const MAX_WAIT_MS: u64 = 15 * 60_000;

/// Epoch milliseconds now.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ------------------------------------------------------------------ crypt

#[link(name = "crypt")]
extern "C" {
    /// libxcrypt's reentrant `crypt`: hashes `phrase` with the settings (and
    /// so the algorithm and salt) of `setting`, which may be a whole stored
    /// hash. `data` is a zeroed `struct crypt_data` (32768 bytes). On failure
    /// the answer is a short string starting with `*`.
    fn crypt_r(phrase: *const c_char, setting: *const c_char, data: *mut c_void) -> *mut c_char;
}

/// `sizeof(struct crypt_data)` in libxcrypt (crypt.h: "exactly 32768 bytes").
const CRYPT_DATA_SIZE: usize = 32768;

fn crypt(phrase: &str, setting: &str) -> Option<String> {
    let phrase = CString::new(phrase).ok()?;
    let setting = CString::new(setting).ok()?;
    // Zeroed, which is how crypt_r wants a new struct (its `initialized`).
    let mut data = vec![0u8; CRYPT_DATA_SIZE];
    // SAFETY: both strings are NUL-terminated and outlive the call; `data` is
    // a zeroed buffer of the struct's size, used by this call alone. A
    // non-null answer points into `data`.
    let hashed = unsafe {
        let out = crypt_r(phrase.as_ptr(), setting.as_ptr(), data.as_mut_ptr().cast());
        (!out.is_null()).then(|| CStr::from_ptr(out).to_string_lossy().into_owned())
    };
    // The struct keeps a copy of the phrase and of intermediate state.
    data.fill(0);
    std::hint::black_box(&data);
    hashed.filter(|h| !h.starts_with('*'))
}

/// Whether `password` hashes to the stored shadow field `hash`. A locked
/// account's hash (`!` in front) or one libcrypt doesn't know can't match,
/// so both refuse. The comparison takes the same time wherever the two differ.
pub fn verify_password(hash: &str, password: &str) -> bool {
    match crypt(password, hash) {
        Some(candidate) => constant_time_eq(candidate.as_bytes(), hash.as_bytes()),
        None => false,
    }
}

/// `a == b` without stopping at the first difference. A length mismatch
/// returns at once: lengths aren't secret here (the hash's is public).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// A SHA-512 crypt hash of `password` with a fresh 16-char salt from `/dev/urandom`.
pub fn hash_password(password: &str) -> Result<String> {
    const SALT_CHARS: &[u8] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut raw = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut raw))
        .context("can't read /dev/urandom for password salt")?;
    let mut salt = String::with_capacity(16);
    for b in raw {
        salt.push(SALT_CHARS[(b as usize) % SALT_CHARS.len()] as char);
    }
    let setting = format!("$6${salt}$");
    crypt(password, &setting).ok_or_else(|| anyhow!("libcrypt failed to hash password"))
}

/// A SHA-512 crypt hash of `password` with a fixed salt, for tests.
#[cfg(test)]
pub(crate) fn test_hash(password: &str) -> String {
    crypt(password, "$6$pyxistestsalt$").expect("libcrypt can hash")
}

// ------------------------------------------------------------------- hmac

/// HMAC-SHA256 (RFC 2104) over `sha2`, block size 64.
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

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The 32 random bytes Echo and Evo share through a root-only file.
#[derive(Clone)]
pub struct SessionKey([u8; 32]);

impl SessionKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; 32];
        fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .context("can't read /dev/urandom for the session key")?;
        Ok(Self(bytes))
    }

    /// Write the key as 64 hex characters (no newline) to `path`, root 0600,
    /// by temp file and rename so Evo never reads half of it. Makes the
    /// directory (0755) if it is missing.
    pub fn write_file(&self, path: &Path) -> Result<()> {
        let dir = path.parent().ok_or_else(|| anyhow!("{} has no directory", path.display()))?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(dir)
            .with_context(|| format!("can't create {}", dir.display()))?;
        let name = path.file_name().ok_or_else(|| anyhow!("{} has no file name", path.display()))?;
        let tmp = dir.join(format!(".{}.tmp", name.to_string_lossy()));
        let _ = fs::remove_file(&tmp);
        let written = (|| -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
            file.write_all(to_hex(&self.0).as_bytes())?;
            file.sync_all()?;
            fs::rename(&tmp, path)
        })();
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(anyhow!("can't write {}: {e}", path.display()));
        }
        Ok(())
    }

    /// `LockChanged`'s mac: HMAC-SHA256 over `pyxis-lock:<0|1>:<seq>`, lowercase hex.
    pub fn mac(&self, locked: bool, seq: u64) -> String {
        to_hex(&hmac_sha256(&self.0, format!("pyxis-lock:{}:{seq}", locked as u8).as_bytes()))
    }
}

/// Where this daemon writes the key: `PYXIS_SESSION_KEY`, else `/run/pyxis/session.key`.
pub fn key_path() -> PathBuf {
    std::env::var_os("PYXIS_SESSION_KEY").map(PathBuf::from).unwrap_or_else(|| KEY_PATH.into())
}

// --------------------------------------------------------------- limiter

#[derive(Debug, Default, Clone, PartialEq)]
struct LimiterState {
    wrong: u32,
    retry_at_ms: u64,
}

/// Where the limiter's counters live on a device (`/run` is tmpfs: a reboot
/// clears them, a daemon restart does not).
pub const AUTH_LIMITS_PATH: &str = "/run/pyxis/auth-limits.json";

/// `PYXIS_AUTH_LIMITS` moves the limiter file, else `/run/pyxis/auth-limits.json`.
pub fn auth_limits_path() -> PathBuf {
    std::env::var_os("PYXIS_AUTH_LIMITS").map(PathBuf::from).unwrap_or_else(|| AUTH_LIMITS_PATH.into())
}

/// How a try (an unlock, or a `current` in `setPassword`) went.
#[derive(Debug, Clone, PartialEq)]
pub enum Attempt {
    Passed,
    /// Wrong, and counted. `retry_at_ms` is set once the tries are used up.
    Wrong { attempts_left: u32, retry_at_ms: Option<u64> },
    /// Refused unchecked, because a wait is running; not counted.
    Wait { retry_at_ms: u64 },
}

/// The wrong-password counters, held in the daemon, one per user name, and
/// written to `path` (root 0600, atomically) after every change.
#[derive(Default)]
pub struct Limiter {
    /// Held for the whole of a try, check included, so a hundred parallel
    /// guesses are a hundred in a row (each one a wait, after the fifth), not a
    /// hundred at once against a counter that hasn't moved.
    gate: Mutex<()>,
    state: Mutex<std::collections::HashMap<String, LimiterState>>,
    path: Option<PathBuf>,
}

impl Limiter {
    /// The daemon's limiter: persisted at `auth_limits_path()`. Under
    /// `cfg(test)` it is in memory unless `PYXIS_AUTH_LIMITS` is set, so the
    /// unit tests never touch `/run`.
    pub fn new() -> Self {
        if cfg!(test) && std::env::var_os("PYXIS_AUTH_LIMITS").is_none() {
            Self::default()
        } else {
            Self::with_path(auth_limits_path())
        }
    }

    /// A limiter persisted at `path`, loading what is there (a missing or
    /// unreadable file starts empty).
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let state = fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.as_object().cloned())
            .map(|m| {
                m.into_iter()
                    .filter_map(|(user, v)| {
                        Some((
                            user,
                            LimiterState {
                                wrong: v.get("attempts")?.as_u64()?.min(u32::MAX as u64) as u32,
                                retry_at_ms: v.get("retry_at")?.as_u64()?,
                            },
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { gate: Mutex::new(()), state: Mutex::new(state), path: Some(path) }
    }

    fn persist(&self, state: &std::collections::HashMap<String, LimiterState>) {
        let Some(path) = &self.path else { return };
        let body: serde_json::Map<String, Value> = state
            .iter()
            .filter(|(_, s)| **s != LimiterState::default())
            .map(|(u, s)| (u.clone(), json!({ "attempts": s.wrong, "retry_at": s.retry_at_ms })))
            .collect();
        if let Err(e) = write_private_atomic(path, Value::Object(body).to_string().as_bytes()) {
            tracing::warn!("session: can't save {}: {e:#}", path.display());
        }
    }

    /// Run one try for `user`: refuse if waiting, else `check` and count the result.
    pub fn attempt(&self, user: &str, now_ms: u64, check: impl FnOnce() -> bool) -> Attempt {
        let _gate = self.gate.lock();
        {
            let state = self.state.lock();
            if let Some(s) = state.get(user) {
                if s.retry_at_ms > now_ms {
                    return Attempt::Wait { retry_at_ms: s.retry_at_ms };
                }
            }
        }
        let passed = check();
        let mut all = self.state.lock();
        if passed {
            let had = all.remove(user).is_some();
            if had {
                self.persist(&all);
            }
            return Attempt::Passed;
        }
        let state = all.entry(user.to_string()).or_default();
        state.wrong += 1;
        let out = if state.wrong < FREE_TRIES {
            Attempt::Wrong { attempts_left: FREE_TRIES - state.wrong, retry_at_ms: None }
        } else {
            let doublings = (state.wrong - FREE_TRIES).min(16);
            let wait = (FIRST_WAIT_MS << doublings).min(MAX_WAIT_MS);
            state.retry_at_ms = now_ms + wait;
            Attempt::Wrong { attempts_left: 0, retry_at_ms: Some(state.retry_at_ms) }
        };
        self.persist(&all);
        out
    }

    /// `(attempts_left, retry_at_ms)` for `user` as `getState` reports them.
    /// While a wait runs there are none left. Once it is over, the tries are
    /// used up and each further wrong one costs a longer wait, so there is one.
    pub fn status(&self, user: &str, now_ms: u64) -> (u32, Option<u64>) {
        let all = self.state.lock();
        let state = all.get(user).cloned().unwrap_or_default();
        if state.retry_at_ms > now_ms {
            (0, Some(state.retry_at_ms))
        } else if state.wrong >= FREE_TRIES {
            (1, None)
        } else {
            (FREE_TRIES - state.wrong, None)
        }
    }
}

/// Write `bytes` to `path` as root-only 0600 through a temp file and a rename.
fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let result = (|| -> Result<()> {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

// --------------------------------------------------------------- service

/// What the lock state became, as `LockChanged` carries it.
#[derive(Debug, Clone, PartialEq)]
pub struct LockChange {
    pub locked: bool,
    pub seq: u64,
    pub mac: String,
}

impl LockChange {
    pub fn signal_body(&self) -> Value {
        json!({ "locked": self.locked, "seq": self.seq, "mac": self.mac })
    }
}

/// `unlock`'s verdict on a password. It changes nothing by itself: a
/// `Granted` is followed by `set_locked(false)`.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    Granted,
    /// `ok: false`: `reason` is `wrong` (counted) or `wait` (refused unchecked).
    Refused { reason: &'static str, attempts_left: u32, retry_at_ms: Option<u64> },
}

impl Check {
    pub fn reply(&self) -> Value {
        match self {
            Check::Granted => json!({ "ok": true }),
            Check::Refused { reason, attempts_left, retry_at_ms } => {
                json!({ "ok": false, "reason": reason, "attempts_left": attempts_left, "retry_at_ms": retry_at_ms })
            }
        }
    }

    /// `UnlockFailed`'s body, for a wrong try (not for a wait).
    pub fn failed_signal(&self) -> Option<Value> {
        match self {
            Check::Refused { reason: "wrong", attempts_left, retry_at_ms } => {
                Some(json!({ "attempts_left": attempts_left, "retry_at_ms": retry_at_ms }))
            }
            _ => None,
        }
    }
}

struct LockState {
    locked: bool,
    last_seq: u64,
    user: String,
}

pub struct SessionService {
    users: Users,
    limiter: Arc<Limiter>,
    key: SessionKey,
    state: Mutex<LockState>,
    /// Held across a state change and the signal that announces it, so two
    /// changes reach Evo in the order their `seq`s say (an unlock's late
    /// signal must not follow a newer lock's: Evo would take it as newer).
    emit_gate: tokio::sync::Mutex<()>,
}

impl SessionService {
    /// For echo-daemon: the device's users and limiter, and a fresh key
    /// written to `key_path()`. A key that can't be written is logged, not
    /// fatal (a dev daemon as an ordinary user has no /run/pyxis): the macs
    /// are then signed with a key nobody can read, and Evo never unlocks.
    pub fn start(users: Users, limiter: Arc<Limiter>) -> Self {
        *global_limiter_slot().write() = Some(limiter.clone());
        let key = SessionKey::generate().unwrap_or_else(|e| {
            tracing::error!("session: {e:#}; using a key nobody has");
            SessionKey::from_bytes([0; 32])
        });
        let path = key_path();
        match key.write_file(&path) {
            Ok(()) => tracing::info!("session: key written to {}", path.display()),
            Err(e) => tracing::warn!("session: {e:#}; Evo will not be able to verify unlocks"),
        }
        Self::new(users, limiter, key)
    }

    /// Registers `session` as the active daemon `SessionService` so
    /// [`verify_active_user_password`] shares its exact active user, user
    /// database, and rate limiter.
    pub fn register_active(session: &Arc<Self>) {
        *global_limiter_slot().write() = Some(session.limiter.clone());
        *global_session_slot().write() = Some(Arc::downgrade(session));
    }

    pub fn new(users: Users, limiter: Arc<Limiter>, key: SessionKey) -> Self {
        Self {
            users,
            limiter,
            key,
            state: Mutex::new(LockState {
                locked: false,
                last_seq: 0,
                user: SESSION_USER.to_string(),
            }),
            emit_gate: tokio::sync::Mutex::new(()),
        }
    }

    /// Take this around `set_locked` and the emit of its change.
    pub async fn change_gate(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.emit_gate.lock().await
    }

    pub fn limiter(&self) -> &Arc<Limiter> {
        &self.limiter
    }

    pub fn is_locked(&self) -> bool {
        self.state.lock().locked
    }

    pub fn active_user(&self) -> String {
        self.state.lock().user.clone()
    }

    /// Lock the session and revoke any active privilege grants.
    pub fn lock(&self) -> LockChange {
        let change = self.set_locked(true);
        if self.users.root() == Path::new("/") {
            crate::services::privilege::revoke_all("session_locked");
        }
        change
    }

    pub fn set_locked(&self, locked: bool) -> LockChange {
        self.set_locked_at(locked, now_ms())
    }

    /// Log out the active session user: revokes all temporary privilege grants
    /// and locks the session back to the lock / user-picker screen.
    pub fn logout(&self) -> LockChange {
        self.set_locked_at(true, now_ms())
    }

    /// Set the state and sign it. `seq` is `max(now, last + 1)`, so it only
    /// goes up, clock steps back included. Called for every lock and every
    /// right password, changed or not: Evo can be locked while this daemon
    /// (restarted) thinks it is not, and must still hear the unlock.
    pub fn set_locked_at(&self, locked: bool, now_ms: u64) -> LockChange {
        let change = {
            let mut state = self.state.lock();
            let seq = now_ms.max(state.last_seq + 1);
            state.last_seq = seq;
            state.locked = locked;
            LockChange { locked, seq, mac: self.key.mac(locked, seq) }
        };
        if locked && self.users.root() == Path::new("/") {
            crate::services::privilege::revoke_all("session_locked");
        }
        change
    }

    /// Switch the active session user to `user` and lock the session so the
    /// lock screen prompts for `user`'s credentials.
    pub fn switch_user(&self, user: &str) -> Result<LockChange> {
        self.switch_user_at(user, now_ms())
    }

    pub fn switch_user_at(&self, user: &str, now_ms: u64) -> Result<LockChange> {
        self.users.require_real(user)?;
        let change = {
            let mut state = self.state.lock();
            let seq = now_ms.max(state.last_seq + 1);
            state.last_seq = seq;
            state.locked = true;
            state.user = user.to_string();
            LockChange {
                locked: true,
                seq,
                mac: self.key.mac(true, seq),
            }
        };
        if self.users.root() == Path::new("/") {
            set_global_active_user(user);
            crate::services::privilege::revoke_all("session_locked");
        }
        Ok(change)
    }

    /// Check `password` for the active session user. Blocking (crypt is slow on
    /// purpose). A user with no password passes with anything and isn't counted.
    pub fn check_password(&self, password: &str) -> Result<Check> {
        self.check_password_at(password, now_ms())
    }

    pub fn check_password_at(&self, password: &str, now_ms: u64) -> Result<Check> {
        self.check_password_for_at(None, password, now_ms)
    }

    /// Check `password` for `user` (or the current session user when `None`).
    /// On `Check::Granted`, switches the active session user to `user`.
    pub fn check_password_for(&self, user: Option<&str>, password: &str) -> Result<Check> {
        self.check_password_for_at(user, password, now_ms())
    }

    pub fn check_password_for_at(
        &self,
        user: Option<&str>,
        password: &str,
        now_ms: u64,
    ) -> Result<Check> {
        let target = match user.map(str::trim).filter(|u| !u.is_empty()) {
            Some(u) => u.to_string(),
            None => self.active_user(),
        };
        let verdict = check_user_password(&self.users, &self.limiter, &target, password, now_ms)?;
        if verdict == Check::Granted {
            self.state.lock().user = target.clone();
            if self.users.root() == Path::new("/") {
                set_global_active_user(&target);
            }
        }
        Ok(verdict)
    }

    /// `getState`'s answer.
    pub fn state(&self) -> Result<Value> {
        self.state_at(now_ms())
    }

    pub fn state_at(&self, now_ms: u64) -> Result<Value> {
        let user = self.active_user();
        let profile = self.users.profile(&user);
        let (attempts_left, retry_at_ms) = self.limiter.status(&user, now_ms);
        Ok(json!({
            "locked": self.is_locked(),
            "user": user,
            "display_name": profile.display_name,
            "accent": profile.accent,
            "has_password": self.users.has_password(&user)?,
            "attempts_left": attempts_left,
            "retry_at_ms": retry_at_ms,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// RFC 2104 against the contract's vectors: key bytes 0..31.
    fn contract_key() -> SessionKey {
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = i as u8;
        }
        SessionKey::from_bytes(bytes)
    }

    #[test]
    fn the_contracts_mac_vectors() {
        let key = contract_key();
        assert_eq!(
            key.mac(false, 1_700_000_000_000),
            "50465067a085d8970c228bb83ed55ebb8011f06989f7240bb1eb3ed7d5a1ccee"
        );
        assert_eq!(
            key.mac(true, 1_700_000_000_001),
            "3c117518d88f28b34392c9b3775cec378fd836d6a816685289f38d74b6fe21c0"
        );
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 1: 20 bytes of 0x0b, "Hi There".
        assert_eq!(
            to_hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // Test case 6: a key longer than the block (131 bytes of 0xaa).
        assert_eq!(
            to_hex(&hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn constant_time_eq_is_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn the_key_file_is_64_hex_chars_root_only_and_replaced_whole() {
        let dir = tempfile::tempdir().unwrap();
        // The directory doesn't exist yet: it is made.
        let path = dir.path().join("run/pyxis/session.key");
        let key = SessionKey::generate().unwrap();
        key.write_file(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.len(), 64);
        assert!(text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_eq!(text, to_hex(&key.0));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o755);

        // A second write replaces it, and leaves no temp file.
        let again = SessionKey::generate().unwrap();
        again.write_file(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), to_hex(&again.0));
        assert_ne!(again.0, key.0);
        let names: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["session.key"]);
    }

    #[test]
    fn crypt_verifies_a_real_hash_and_only_that() {
        let hash = test_hash("correct horse");
        assert!(hash.starts_with("$6$"), "{hash}");
        assert!(verify_password(&hash, "correct horse"));
        assert!(!verify_password(&hash, "correct horsf"));
        assert!(!verify_password(&hash, ""));
        // NUL can't be in a C string: refuses rather than truncating.
        assert!(!verify_password(&hash, "correct horse\0x"));
        // A locked account, or a field libcrypt doesn't know, matches nothing.
        assert!(!verify_password(&format!("!{hash}"), "correct horse"));
        assert!(!verify_password("$zz$nonsense", "correct horse"));
        assert!(!verify_password("*", "correct horse"));
    }

    #[test]
    fn the_limiter_gives_five_tries_then_waits_that_double_to_fifteen_minutes() {
        const U: &str = "geckouser";
        let limiter = Limiter::new();
        let wrong = |t: u64| limiter.attempt(U, t, || false);
        assert_eq!(limiter.status(U, 0), (5, None));
        for left in (1..=4).rev() {
            assert_eq!(wrong(1_000), Attempt::Wrong { attempts_left: left, retry_at_ms: None });
        }
        assert_eq!(wrong(1_000), Attempt::Wrong { attempts_left: 0, retry_at_ms: Some(31_000) });
        assert_eq!(limiter.status(U, 1_000), (0, Some(31_000)));

        // A try during the wait is refused, not run, not counted.
        let mut ran = false;
        assert_eq!(limiter.attempt(U, 30_999, || { ran = true; true }), Attempt::Wait { retry_at_ms: 31_000 });
        assert!(!ran);
        assert_eq!(limiter.status(U, 30_999), (0, Some(31_000)));

        // Past the wait there is one try; each further wrong one doubles it.
        assert_eq!(limiter.status(U, 31_000), (1, None));
        let mut now = 31_000;
        for wait in [60_000, 120_000, 240_000, 480_000, 900_000, 900_000, 900_000] {
            let r = wrong(now);
            assert_eq!(r, Attempt::Wrong { attempts_left: 0, retry_at_ms: Some(now + wait) });
            now += wait;
        }

        // A right one resets everything.
        assert_eq!(limiter.attempt(U, now, || true), Attempt::Passed);
        assert_eq!(limiter.status(U, now), (5, None));
        assert_eq!(wrong(now), Attempt::Wrong { attempts_left: 4, retry_at_ms: None });
    }

    #[test]
    fn a_right_password_part_way_resets_the_count() {
        const U: &str = "geckouser";
        let limiter = Limiter::new();
        for _ in 0..4 {
            limiter.attempt(U, 0, || false);
        }
        assert_eq!(limiter.status(U, 0), (1, None));
        assert_eq!(limiter.attempt(U, 0, || true), Attempt::Passed);
        assert_eq!(limiter.status(U, 0), (5, None));
    }

    #[test]
    fn parallel_guesses_are_taken_one_at_a_time() {
        let limiter = Arc::new(Limiter::new());
        let ran = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let threads: Vec<_> = (0..40)
            .map(|_| {
                let (limiter, ran) = (limiter.clone(), ran.clone());
                std::thread::spawn(move || {
                    limiter.attempt("geckouser", 1_000, || {
                        ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        false
                    })
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        // Five checked, then a wait refused the other thirty-five unchecked.
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), FREE_TRIES);
    }

    fn service(shadow: &str) -> (tempfile::TempDir, SessionService) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("etc")).unwrap();
        fs::write(
            dir.path().join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/bash\ngeckouser:x:1000:1000:Gecko User:/home/geckouser:/bin/bash\n",
        )
        .unwrap();
        fs::write(dir.path().join("etc/shadow"), shadow).unwrap();
        let svc = SessionService::new(Users::with_root(dir.path()), Arc::new(Limiter::new()), contract_key());
        (dir, svc)
    }

    #[test]
    fn with_no_password_set_anything_unlocks_and_nothing_is_counted() {
        for field in ["*", "!", ""] {
            let (_dir, svc) = service(&format!("geckouser:{field}:20000:0:99999:7:::\n"));
            assert_eq!(svc.check_password_at("", 0).unwrap(), Check::Granted);
            assert_eq!(svc.check_password_at("whatever", 0).unwrap(), Check::Granted);
            let state = svc.state_at(0).unwrap();
            assert_eq!(state["has_password"], false);
            assert_eq!(state["attempts_left"], 5);
        }
        // A shadow with no line for the user: nothing to protect either.
        let (_dir, svc) = service("root:*:1::::::\n");
        assert_eq!(svc.check_password_at("x", 0).unwrap(), Check::Granted);
    }

    #[test]
    fn an_unreadable_shadow_refuses_rather_than_unlocking() {
        let dir = tempfile::tempdir().unwrap();
        let svc = SessionService::new(Users::with_root(dir.path()), Arc::new(Limiter::new()), contract_key());
        assert!(svc.check_password_at("x", 0).is_err());
        assert!(svc.state_at(0).is_err());
    }

    #[test]
    fn a_password_unlocks_after_wrong_ones_count_and_the_sixth_waits() {
        let hash = test_hash("sesame");
        let (_dir, svc) = service(&format!("geckouser:{hash}:20000:0:99999:7:::\n"));

        let refused = |left, at| Check::Refused { reason: "wrong", attempts_left: left, retry_at_ms: at };
        assert_eq!(svc.check_password_at("nope", 1_000).unwrap(), refused(4, None));
        assert_eq!(svc.state_at(1_000).unwrap()["attempts_left"], 4);
        for left in [3, 2, 1] {
            assert_eq!(svc.check_password_at("nope", 1_000).unwrap(), refused(left, None));
        }
        let fifth = svc.check_password_at("nope", 1_000).unwrap();
        assert_eq!(fifth, refused(0, Some(31_000)));
        assert_eq!(fifth.failed_signal().unwrap(), json!({"attempts_left": 0, "retry_at_ms": 31_000}));

        // The right password during the wait is refused, with no signal.
        let wait = svc.check_password_at("sesame", 2_000).unwrap();
        assert_eq!(wait, Check::Refused { reason: "wait", attempts_left: 0, retry_at_ms: Some(31_000) });
        assert!(wait.failed_signal().is_none());
        assert_eq!(wait.reply(), json!({"ok": false, "reason": "wait", "attempts_left": 0, "retry_at_ms": 31_000}));
        let state = svc.state_at(2_000).unwrap();
        assert_eq!((state["attempts_left"].clone(), state["retry_at_ms"].clone()), (json!(0), json!(31_000)));
        assert_eq!(state["has_password"], true);

        // After it, the right one passes and the count is gone.
        assert_eq!(svc.check_password_at("sesame", 31_500).unwrap(), Check::Granted);
        assert_eq!(svc.check_password_at("sesame", 31_500).unwrap().reply(), json!({"ok": true}));
        let state = svc.state_at(31_500).unwrap();
        assert_eq!((state["attempts_left"].clone(), state["retry_at_ms"].clone()), (json!(5), Value::Null));
    }

    #[test]
    fn seq_only_goes_up_and_the_mac_signs_state_and_seq() {
        let (_dir, svc) = service("geckouser:*:1::::::\n");
        assert!(!svc.is_locked());
        let a = svc.set_locked_at(true, 1_700_000_000_000);
        assert_eq!(a.seq, 1_700_000_000_000);
        assert!(svc.is_locked());
        assert_eq!(a.mac, contract_key().mac(true, a.seq));
        // The same millisecond, and a clock that went back: still above.
        let b = svc.set_locked_at(false, 1_700_000_000_000);
        assert_eq!(b.seq, 1_700_000_000_001);
        assert!(!svc.is_locked());
        assert_eq!(b.mac, contract_key().mac(false, b.seq));
        let c = svc.set_locked_at(true, 5);
        assert_eq!(c.seq, 1_700_000_000_002);
        assert_eq!(
            c.signal_body(),
            json!({"locked": true, "seq": 1_700_000_000_002u64, "mac": contract_key().mac(true, 1_700_000_000_002)})
        );
        // Not the other state's mac, and not another seq's.
        assert_ne!(c.mac, contract_key().mac(false, c.seq));
        assert_ne!(c.mac, contract_key().mac(true, c.seq + 1));
    }

    #[test]
    fn get_state_reports_the_session_user_and_their_look() {
        let (_dir, svc) = service("geckouser:*:1::::::\n");
        let state = svc.state_at(0).unwrap();
        assert_eq!(
            state,
            json!({
                "locked": false,
                "user": "geckouser",
                "display_name": "Gecko User",
                "accent": super::super::accounts::DEFAULT_ACCENT,
                "has_password": false,
                "attempts_left": 5,
                "retry_at_ms": null,
            })
        );
        svc.set_locked_at(true, 1);
        assert_eq!(svc.state_at(0).unwrap()["locked"], true);
    }

    #[test]
    fn switch_user_and_unlock_for_user_switch_the_active_session_user() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("etc")).unwrap();
        fs::write(
            dir.path().join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/bash\n\
             geckouser:x:1000:1000:Gecko User:/home/geckouser:/bin/bash\n\
             ada:x:1002:1002:Ada Lovelace:/home/ada:/bin/bash\n",
        )
        .unwrap();
        let ada_hash = test_hash("analytical");
        fs::write(
            dir.path().join("etc/shadow"),
            format!("geckouser:*:20000:0:99999:7:::\nada:{ada_hash}:20000:0:99999:7:::\n"),
        )
        .unwrap();
        let svc = SessionService::new(
            Users::with_root(dir.path()),
            Arc::new(Limiter::new()),
            contract_key(),
        );
        assert_eq!(svc.active_user(), "geckouser");

        // Switching to a real user locks the session and targets that user.
        let change = svc.switch_user_at("ada", 1_000).unwrap();
        assert!(change.locked);
        assert_eq!(svc.active_user(), "ada");
        let state = svc.state_at(1_000).unwrap();
        assert_eq!(state["user"], "ada");
        assert_eq!(state["display_name"], "Ada Lovelace");
        assert_eq!(state["has_password"], true);

        // Wrong password for Ada is refused; right password unlocks as Ada.
        assert!(matches!(
            svc.check_password_for_at(Some("ada"), "wrong", 1_001).unwrap(),
            Check::Refused { reason: "wrong", .. }
        ));
        assert_eq!(
            svc.check_password_for_at(Some("ada"), "analytical", 1_002).unwrap(),
            Check::Granted
        );
        assert_eq!(svc.active_user(), "ada");

        // Unlocking directly as geckouser from the lock screen switches active_user back.
        assert_eq!(
            svc.check_password_for_at(Some("geckouser"), "", 1_003).unwrap(),
            Check::Granted
        );
        assert_eq!(svc.active_user(), "geckouser");

        // Non-existent user is refused.
        assert!(svc.switch_user_at("ghost", 1_004).is_err());
        assert!(svc.check_password_for_at(Some("ghost"), "", 1_004).is_err());
    }

    #[test]
    fn verify_user_password_shares_rate_limiter_with_lock_screen() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("etc")).unwrap();
        fs::write(
            dir.path().join("etc/passwd"),
            "geckouser:x:1000:1000:Gecko User:/home/geckouser:/bin/bash\n",
        )
        .unwrap();
        let hash = test_hash("sesame");
        fs::write(
            dir.path().join("etc/shadow"),
            format!("geckouser:{hash}:20000:0:99999:7:::\n"),
        )
        .unwrap();
        let users = Users::with_root(dir.path());
        let limiter = Arc::new(Limiter::new());
        let svc = SessionService::new(users.clone(), limiter.clone(), contract_key());

        // Burn 4 attempts through lock screen, 5th through verify_user_password_with.
        for _ in 0..4 {
            assert!(matches!(
                svc.check_password_at("wrong", 1_000).unwrap(),
                Check::Refused { reason: "wrong", .. }
            ));
        }
        let err = verify_user_password_with(&users, &limiter, "geckouser", "wrong", 1_000)
            .unwrap_err();
        assert!(err.contains("too many attempts"));
        // Even the right password is locked out until retry window passes.
        let wait = verify_user_password_with(&users, &limiter, "geckouser", "sesame", 2_000)
            .unwrap_err();
        assert!(wait.contains("wait"));
        // Succeeds after cooldown.
        assert_eq!(
            verify_user_password_with(&users, &limiter, "geckouser", "sesame", 31_500).unwrap(),
            "geckouser"
        );
    }

    #[test]
    fn logout_locks_an_unlocked_session_and_advances_seq() {
        let dir = tempfile::tempdir().unwrap();
        let users = Users::with_root(dir.path());
        let limiter = Arc::new(Limiter::new());
        let svc = SessionService::new(users, limiter, contract_key());

        let unlocked = svc.set_locked_at(false, 10_000);
        assert!(!unlocked.locked);
        assert!(!svc.is_locked());

        let logged_out = svc.logout();
        assert!(logged_out.locked);
        assert!(svc.is_locked());
        assert!(logged_out.seq > unlocked.seq);
    }

    #[test]
    fn verify_user_password_detailed_reports_attempts_left_and_retry_at_ms() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("etc")).unwrap();
        fs::write(
            dir.path().join("etc/passwd"),
            "geckouser:x:1000:1000:Gecko User:/home/geckouser:/bin/bash\n",
        )
        .unwrap();
        let hash = test_hash("sesame");
        fs::write(
            dir.path().join("etc/shadow"),
            format!("geckouser:{hash}:20000:0:99999:7:::\n"),
        )
        .unwrap();
        let users = Users::with_root(dir.path());
        let limiter = Arc::new(Limiter::new());

        for expected_left in [4, 3, 2, 1] {
            let err = verify_user_password_with_detailed(&users, &limiter, "geckouser", "bad", 1_000)
                .unwrap_err();
            assert_eq!(err.reason, "wrong");
            assert_eq!(err.attempts_left, Some(expected_left));
            assert_eq!(err.retry_at_ms, None);
        }

        let fifth = verify_user_password_with_detailed(&users, &limiter, "geckouser", "bad", 1_000)
            .unwrap_err();
        assert_eq!(fifth.reason, "wrong");
        assert_eq!(fifth.attempts_left, Some(0));
        assert_eq!(fifth.retry_at_ms, Some(1_000 + FIRST_WAIT_MS));

        let waiting =
            verify_user_password_with_detailed(&users, &limiter, "geckouser", "sesame", 5_000)
                .unwrap_err();
        assert_eq!(waiting.reason, "wait");
        assert_eq!(waiting.attempts_left, Some(0));
        assert_eq!(waiting.retry_at_ms, Some(1_000 + FIRST_WAIT_MS));
    }

    #[test]
    fn strict_check_refuses_no_password_accounts_but_unlock_still_grants() {
        for shadow in ["geckouser:*:1::::::\n", "geckouser:!:1::::::\n", "geckouser::1::::::\n"] {
            let (_dir, svc) = service(shadow);
            assert_eq!(svc.check_password_at("x", 0).unwrap(), Check::Granted);
            let strict = check_user_password_strict(&svc.users, &svc.limiter, "geckouser", "x", 0).unwrap();
            assert_eq!(strict, Check::Refused { reason: "no-password", attempts_left: 0, retry_at_ms: None });
            let f = verify_user_password_with_detailed(&svc.users, &svc.limiter, "geckouser", "x", 0).unwrap_err();
            assert_eq!(f.reason, "no-password");
            assert_eq!(f.message, "set a password first");
            assert_eq!(svc.limiter.status("geckouser", 0), (5, None), "nothing is counted");
        }
        let (_dir, svc) = service(&format!("geckouser:{}:1::::::\n", test_hash("pw")));
        assert_eq!(
            check_user_password_strict(&svc.users, &svc.limiter, "geckouser", "pw", 0).unwrap(),
            Check::Granted
        );
    }

    #[test]
    fn the_limiter_counts_per_user() {
        let limiter = Limiter::new();
        for _ in 0..5 {
            limiter.attempt("ada", 1_000, || false);
        }
        assert_eq!(limiter.status("ada", 1_000), (0, Some(31_000)));
        assert_eq!(limiter.status("geckouser", 1_000), (5, None));
        assert_eq!(limiter.attempt("geckouser", 1_000, || true), Attempt::Passed);
        assert_eq!(limiter.attempt("ada", 1_001, || true), Attempt::Wait { retry_at_ms: 31_000 });
    }

    #[test]
    fn the_limiter_survives_a_restart_through_a_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run/auth-limits.json");
        let a = Limiter::with_path(&path);
        for _ in 0..5 {
            a.attempt("geckouser", 1_000, || false);
        }
        a.attempt("ada", 1_000, || false);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let b = Limiter::with_path(&path);
        assert_eq!(b.status("geckouser", 2_000), (0, Some(31_000)));
        assert_eq!(b.status("ada", 2_000), (4, None));
        assert_eq!(b.attempt("geckouser", 2_000, || true), Attempt::Wait { retry_at_ms: 31_000 });
        // A right password clears the entry from the file too.
        assert_eq!(b.attempt("ada", 2_000, || true), Attempt::Passed);
        let c = Limiter::with_path(&path);
        assert_eq!(c.status("ada", 2_000), (5, None));
        assert_eq!(c.status("geckouser", 2_000), (0, Some(31_000)));
        // No stray temp files; a corrupt file starts empty.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        fs::write(&path, "not json").unwrap();
        assert_eq!(Limiter::with_path(&path).status("geckouser", 0), (5, None));
    }
}
