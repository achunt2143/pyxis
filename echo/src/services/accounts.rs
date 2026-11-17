//! Accounts: the device's real users, how the lock screen and Settings show
//! them, and changing a password.
//!
//! A "real" user is one in `/etc/passwd` with uid 1000 or more, minus
//! `pyxisapp` (the user every app runs as, not anyone's account) and uid 65534
//! (`nobody`, which the uid test alone would let in). What each one looks
//! like (display name, accent, avatar) is a small JSON file per user under
//! `/var/lib/pyxis/accounts/`, written atomically; a user with no file reads
//! as their GECOS name and Pyxis's default accent.
//!
//! The password itself is the system's: `/etc/shadow` is read here (echo-daemon
//! is root) and written only by `chpasswd`, which we run and wait on ourselves
//! (never `waitpid(-1)`: see the AppHost reaper). A wrong `current` counts
//! against the same limiter as a wrong unlock (`session::Limiter`), so the
//! password can't be guessed through Settings instead of the lock screen.
//!
//! `PYXIS_ACCOUNTS_ROOT` puts `etc/passwd`, `etc/shadow` and
//! `var/lib/pyxis/accounts/` under another directory, and `PYXIS_CHPASSWD`
//! names another program for `chpasswd`, so tests and a dev echo-daemon never
//! touch the host's users.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::session::{hash_password, now_ms, verify_password, Attempt, Limiter, SESSION_USER};

pub const ACCOUNTS_IFACE: &str = "org.geckoOS.accounts";
pub const METHOD_LIST: &str = "list";
pub const METHOD_CREATE: &str = "create";
pub const METHOD_DELETE: &str = "delete";
pub const METHOD_SET_PASSWORD: &str = "setPassword";
pub const METHOD_SET_PROFILE: &str = "setProfile";

/// The user every Nova-launched app runs as (`APP_UID` 1001 in nova.rs); never
/// listed.
const APP_USER: &str = "pyxisapp";
const FIRST_REAL_UID: u32 = 1000;
/// `nobody`: above the first real uid, and not a person.
const NOBODY_UID: u32 = 65534;

/// The accent a user has until they pick one: the remochi spinner's blue.
pub const DEFAULT_ACCENT: &str = "#69cdff";

const NAME_MAX_CHARS: usize = 64;
/// An avatar is a small data URL. This is the whole string, base64 included.
const AVATAR_MAX_BYTES: usize = 700_000;
const AVATAR_PREFIXES: &[&str] =
    &["data:image/png;base64,", "data:image/jpeg;base64,", "data:image/webp;base64,"];
/// libxcrypt refuses a passphrase of 512 bytes with its NUL, and no one types
/// more. Over it, `chpasswd` would write a hash no one can match.
const PASSWORD_MAX_BYTES: usize = 511;

const DEFAULT_CHPASSWD: &str = "/usr/sbin/chpasswd";

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// One `/etc/passwd` line, the parts we use.
#[derive(Debug, Clone, PartialEq)]
pub struct PasswdEntry {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    /// The GECOS field's first comma-separated part, often the full name.
    pub full_name: String,
    pub home: String,
}

/// What a user looks like, with defaults filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub display_name: String,
    pub accent: String,
    pub avatar: Option<String>,
}

/// What is stored: only what the user chose.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
struct Stored {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    accent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    avatar: Option<String>,
}

/// The user database: passwd, shadow and the profile directory, under one
/// root (`/` on a device).
#[derive(Debug, Clone)]
pub struct Users {
    root: PathBuf,
}

impl Users {
    /// The device's own tree, or `PYXIS_ACCOUNTS_ROOT`'s.
    pub fn system() -> Self {
        Self::with_root(std::env::var_os("PYXIS_ACCOUNTS_ROOT").map(PathBuf::from).unwrap_or_else(|| "/".into()))
    }

    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn passwd_path(&self) -> PathBuf {
        self.root.join("etc/passwd")
    }

    pub fn shadow_path(&self) -> PathBuf {
        self.root.join("etc/shadow")
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.root.join("var/lib/pyxis/accounts")
    }

    fn passwd(&self) -> Result<Vec<PasswdEntry>> {
        let text = fs::read_to_string(self.passwd_path())
            .with_context(|| format!("can't read {}", self.passwd_path().display()))?;
        Ok(text.lines().filter_map(parse_passwd_line).collect())
    }

    /// The users the lock screen and Settings offer.
    pub fn real_users(&self) -> Result<Vec<PasswdEntry>> {
        Ok(self.passwd()?.into_iter().filter(is_real_user).collect())
    }

    /// `user`'s entry if it is a real user, else an error naming why a call
    /// about them is refused. This is also what keeps a name from becoming a
    /// path: only a name `/etc/passwd` lists, in a shape no path can take,
    /// gets this far.
    pub fn require_real(&self, user: &str) -> Result<PasswdEntry> {
        if !is_username(user) {
            return Err(anyhow!("not a user name: {user:?}"));
        }
        self.real_users()?
            .into_iter()
            .find(|e| e.name == user)
            .ok_or_else(|| anyhow!("no such user: {user}"))
    }

    /// `user`'s hash field in `/etc/shadow`. `None` when the file has no line
    /// for them (a user that doesn't exist has nothing to protect); an
    /// unreadable file is an error, which refuses rather than unlocks.
    pub fn password_hash(&self, user: &str) -> Result<Option<String>> {
        let text = fs::read_to_string(self.shadow_path())
            .with_context(|| format!("can't read {}", self.shadow_path().display()))?;
        Ok(text.lines().find_map(|line| {
            let mut fields = line.split(':');
            (fields.next() == Some(user)).then(|| fields.next().unwrap_or("").to_string())
        }))
    }

    /// Whether `user` has a password: no, for a missing line or a hash field
    /// that is empty, `*` or `!` (no password was ever set). A locked
    /// account (`!` in front of a real hash) has one, and nothing matches it.
    pub fn has_password(&self, user: &str) -> Result<bool> {
        Ok(self.password_hash(user)?.map(|h| hash_is_set(&h)).unwrap_or(false))
    }

    /// `user`'s profile with defaults filled in. Works for a name `/etc/passwd`
    /// doesn't list too (the display name is then the name itself).
    pub fn profile(&self, user: &str) -> Profile {
        let stored = self.stored(user);
        let full_name = self
            .passwd()
            .ok()
            .and_then(|all| all.into_iter().find(|e| e.name == user))
            .map(|e| e.full_name)
            .filter(|n| !n.is_empty());
        Profile {
            display_name: stored.display_name.or(full_name).unwrap_or_else(|| user.to_string()),
            accent: stored.accent.unwrap_or_else(|| DEFAULT_ACCENT.to_string()),
            avatar: stored.avatar,
        }
    }

    fn profile_path(&self, user: &str) -> PathBuf {
        self.profiles_dir().join(format!("{user}.json"))
    }

    /// A missing or unreadable file is a user who chose nothing.
    fn stored(&self, user: &str) -> Stored {
        if !is_username(user) {
            return Stored::default();
        }
        match fs::read(self.profile_path(user)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("accounts: {} is not a profile ({e}); using defaults", self.profile_path(user).display());
                Stored::default()
            }),
            Err(_) => Stored::default(),
        }
    }

    fn store(&self, user: &str, stored: &Stored) -> Result<()> {
        let dir = self.profiles_dir();
        fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;
        let bytes = serde_json::to_vec_pretty(stored)?;
        let tmp = dir.join(format!(".{user}.json.{}.{}", std::process::id(), TEMP_SEQ.fetch_add(1, Ordering::Relaxed)));
        let written = (|| -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, self.profile_path(user))
        })();
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(anyhow!("can't write {}: {e}", self.profile_path(user).display()));
        }
        Ok(())
    }
}

fn parse_passwd_line(line: &str) -> Option<PasswdEntry> {
    let mut f = line.split(':');
    let name = f.next().filter(|n| !n.is_empty())?;
    let _password = f.next()?;
    let uid: u32 = f.next()?.parse().ok()?;
    let gid: u32 = f.next()?.parse().unwrap_or(uid);
    let gecos = f.next().unwrap_or("");
    let home = f
        .next()
        .filter(|h| !h.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("/home/{name}"));
    Some(PasswdEntry {
        name: name.to_string(),
        uid,
        gid,
        full_name: gecos.split(',').next().unwrap_or("").trim().to_string(),
        home,
    })
}

fn is_real_user(e: &PasswdEntry) -> bool {
    e.uid >= FIRST_REAL_UID
        && e.uid < super::nova::APP_UID_BASE
        && e.uid != super::nova::APP_UID
        && e.uid != NOBODY_UID
        && e.name != APP_USER
}

/// The shape of a Debian user name (`NAME_REGEX` in adduser): no slash, dot
/// or colon, so it is safe in a path and in `chpasswd`'s `user:password`.
fn is_username(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && name.len() <= 32
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// A shadow hash field that means "a password is set".
pub fn hash_is_set(hash: &str) -> bool {
    !matches!(hash, "" | "*" | "!" | "!!")
}

pub struct AccountsService {
    users: Users,
    limiter: Arc<Limiter>,
    chpasswd: PathBuf,
    /// One profile read-modify-write at a time, so two `setProfile`s for
    /// different fields don't lose one.
    profile_lock: Mutex<()>,
}

impl AccountsService {
    pub fn system(limiter: Arc<Limiter>) -> Self {
        let chpasswd = std::env::var_os("PYXIS_CHPASSWD").map(PathBuf::from).unwrap_or_else(|| DEFAULT_CHPASSWD.into());
        Self::new(Users::system(), limiter, chpasswd)
    }

    pub fn new(users: Users, limiter: Arc<Limiter>, chpasswd: impl Into<PathBuf>) -> Self {
        Self { users, limiter, chpasswd: chpasswd.into(), profile_lock: Mutex::new(()) }
    }

    /// `list`: `{users: [{user, uid, display_name, accent, avatar, has_password}]}`.
    pub fn list(&self) -> Result<Value> {
        let mut users = Vec::new();
        for entry in self.users.real_users()? {
            let profile = self.users.profile(&entry.name);
            users.push(json!({
                "user": entry.name,
                "uid": entry.uid,
                "display_name": profile.display_name,
                "accent": profile.accent,
                "avatar": profile.avatar,
                "has_password": self.users.has_password(&entry.name)?,
            }));
        }
        Ok(json!({ "users": users }))
    }

    /// `setProfile {user, display_name?, accent?, avatar?}`. A field left out
    /// is kept, one set to null goes back to its default.
    pub fn set_profile(&self, body: &Value) -> Result<Value> {
        let user = str_field(body, "user")?.ok_or_else(|| anyhow!("setProfile needs a string `user`"))?;
        self.users.require_real(&user)?;

        let _guard = self.profile_lock.lock();
        let mut stored = self.users.stored(&user);
        if let Some(v) = body.get("display_name") {
            stored.display_name = clean(v, "display_name", valid_display_name)?;
        }
        if let Some(v) = body.get("accent") {
            stored.accent = clean(v, "accent", valid_accent)?.map(|a| a.to_ascii_lowercase());
        }
        if let Some(v) = body.get("avatar") {
            stored.avatar = clean(v, "avatar", valid_avatar)?;
        }
        self.users.store(&user, &stored)?;
        Ok(json!({ "ok": true }))
    }

    /// `create {user, display_name?, accent?, avatar?, password?}` ->
    /// `{ok: true, user: {user, uid, display_name, accent, avatar, has_password}}`.
    /// Allocates the next available human UID in `1000..10000` (skipping 1001),
    /// writes `/etc/passwd`, `/etc/shadow` (and `/etc/group` if present),
    /// creates `<root>/home/<user>` mode 0700, and persists the profile JSON.
    pub fn create(&self, body: &Value) -> Result<Value> {
        let user = str_field(body, "user")?
            .ok_or_else(|| anyhow!("create needs a string `user`"))?
            .trim()
            .to_string();
        if !is_username(&user) {
            return Err(anyhow!("not a valid user name: {user:?}"));
        }
        if matches!(user.as_str(), "root" | "nobody" | APP_USER) {
            return Err(anyhow!("reserved user name: {user}"));
        }

        let display_name = match body.get("display_name") {
            Some(v) => clean(v, "display_name", valid_display_name)?,
            None => None,
        };
        let accent = match body.get("accent") {
            Some(v) => clean(v, "accent", valid_accent)?.map(|a| a.to_ascii_lowercase()),
            None => None,
        };
        let avatar = match body.get("avatar") {
            Some(v) => clean(v, "avatar", valid_avatar)?,
            None => None,
        };
        let password = str_field(body, "password")?.filter(|p| !p.is_empty());
        if let Some(ref pw) = password {
            if pw.len() > PASSWORD_MAX_BYTES {
                return Err(anyhow!(
                    "the password is too long (at most {PASSWORD_MAX_BYTES} bytes)"
                ));
            }
            if pw.contains(['\n', '\r', '\0']) {
                return Err(anyhow!("the password can't contain a line break or NUL"));
            }
        }

        let _guard = self.profile_lock.lock();
        let existing = self.users.passwd()?;
        if existing.iter().any(|e| e.name == user) {
            return Err(anyhow!("user already exists: {user}"));
        }

        let used_uids: std::collections::HashSet<u32> =
            existing.iter().map(|e| e.uid).collect();
        let uid = (FIRST_REAL_UID..super::nova::APP_UID_BASE)
            .find(|&u| u != super::nova::APP_UID && u != NOBODY_UID && !used_uids.contains(&u))
            .ok_or_else(|| anyhow!("no available user IDs remaining"))?;

        let gecos = display_name
            .as_deref()
            .unwrap_or(&user)
            .replace([':', '\n', '\r'], " ");
        let passwd_line = format!("{user}:x:{uid}:{uid}:{gecos}:/home/{user}:/bin/bash\n");
        append_line_atomic(&self.users.passwd_path(), &passwd_line, 0o644)?;

        let group_path = self.users.root().join("etc/group");
        if group_path.exists() {
            let group_line = format!("{user}:x:{uid}:\n");
            let _ = append_line_atomic(&group_path, &group_line, 0o644);
        }

        let shadow_hash = match password {
            Some(ref pw) => hash_password(pw)?,
            None => "*".to_string(),
        };
        let shadow_line = format!("{user}:{shadow_hash}:20000:0:99999:7:::\n");
        append_line_atomic(&self.users.shadow_path(), &shadow_line, 0o640)?;

        let home_dir = self.users.root().join("home").join(&user);
        fs::create_dir_all(&home_dir)
            .with_context(|| format!("can't create {}", home_dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&home_dir, fs::Permissions::from_mode(0o700));
            if rustix::process::geteuid().is_root() {
                let _ = std::os::unix::fs::chown(&home_dir, Some(uid), Some(uid));
            }
        }

        if display_name.is_some() || accent.is_some() || avatar.is_some() {
            self.users
                .store(&user, &Stored { display_name, accent, avatar })?;
        }

        let profile = self.users.profile(&user);
        Ok(json!({
            "ok": true,
            "user": {
                "user": user,
                "uid": uid,
                "display_name": profile.display_name,
                "accent": profile.accent,
                "avatar": profile.avatar,
                "has_password": self.users.has_password(&user)?,
            }
        }))
    }

    /// `delete {user}` -> `{ok: true, user}`. Refuses to delete `geckouser`,
    /// `pyxisapp`, or the currently active session user.
    pub fn delete(&self, body: &Value, active_user: &str) -> Result<Value> {
        let user = str_field(body, "user")?
            .ok_or_else(|| anyhow!("delete needs a string `user`"))?;
        self.users.require_real(&user)?;
        if user == SESSION_USER {
            return Err(anyhow!("cannot delete the primary user account ({SESSION_USER})"));
        }
        if user == active_user {
            return Err(anyhow!("cannot delete the active session user ({active_user})"));
        }

        let _guard = self.profile_lock.lock();
        remove_user_line_atomic(&self.users.passwd_path(), &user, 0o644)?;
        remove_user_line_atomic(&self.users.shadow_path(), &user, 0o640)?;
        let group_path = self.users.root().join("etc/group");
        if group_path.exists() {
            let _ = remove_user_line_atomic(&group_path, &user, 0o644);
        }
        let _ = fs::remove_file(self.users.profile_path(&user));
        Ok(json!({ "ok": true, "user": user }))
    }

    /// `setPassword {user, current, new}`. `current` is checked when a
    /// password is set, by the unlock limiter (wrong: `reason: "wrong"`, too
    /// soon after too many: `reason: "wait"`, neither one changes anything).
    /// Blocking: runs crypt and a child process.
    pub fn set_password(&self, body: &Value) -> Result<Value> {
        self.set_password_at(body, now_ms())
    }

    pub fn set_password_at(&self, body: &Value, now_ms: u64) -> Result<Value> {
        let user = str_field(body, "user")?.ok_or_else(|| anyhow!("setPassword needs a string `user`"))?;
        let new = str_field(body, "new")?.ok_or_else(|| anyhow!("setPassword needs a string `new`"))?;
        let current = str_field(body, "current")?.unwrap_or_default();
        self.users.require_real(&user)?;
        // chpasswd reads one `user:password` per line: a newline in `new`
        // would be a second entry, for any user.
        if new.is_empty() {
            return Err(anyhow!("the new password must have at least one character"));
        }
        if new.len() > PASSWORD_MAX_BYTES {
            return Err(anyhow!("the new password is too long (at most {PASSWORD_MAX_BYTES} bytes)"));
        }
        if new.contains(['\n', '\r', '\0']) {
            return Err(anyhow!("the new password can't contain a line break or NUL"));
        }

        if let Some(hash) = self.users.password_hash(&user)?.filter(|h| hash_is_set(h)) {
            match self.limiter.attempt(&user, now_ms, || verify_password(&hash, &current)) {
                Attempt::Passed => {}
                Attempt::Wrong { attempts_left, retry_at_ms } => {
                    return Ok(json!({
                        "ok": false, "reason": "wrong", "attempts_left": attempts_left, "retry_at_ms": retry_at_ms,
                    }));
                }
                Attempt::Wait { retry_at_ms } => {
                    return Ok(json!({
                        "ok": false, "reason": "wait", "attempts_left": 0, "retry_at_ms": retry_at_ms,
                    }));
                }
            }
        }

        self.run_chpasswd(&user, &new)?;
        Ok(json!({ "ok": true }))
    }

    /// Run `chpasswd` with `user:new` on its stdin (never in argv, where
    /// `ps` shows it) and wait for that child alone.
    fn run_chpasswd(&self, user: &str, new: &str) -> Result<()> {
        let mut cmd = Command::new(&self.chpasswd);
        if self.users.root() != Path::new("/") {
            cmd.arg("--root").arg(self.users.root());
        }
        let mut child = cmd
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("can't run {}", self.chpasswd.display()))?;
        {
            let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("chpasswd has no stdin"))?;
            stdin.write_all(format!("{user}:{new}\n").as_bytes()).context("can't write to chpasswd")?;
        }
        let out = child.wait_with_output().context("can't wait for chpasswd")?;
        if out.status.success() {
            Ok(())
        } else {
            let why = String::from_utf8_lossy(&out.stderr);
            Err(anyhow!("chpasswd failed ({}): {}", out.status, why.lines().next().unwrap_or("no message")))
        }
    }
}

/// A string field: `None` when absent or null, an error when something else.
fn str_field(body: &Value, key: &str) -> Result<Option<String>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(anyhow!("`{key}` must be a string, not {other}")),
    }
}

/// A profile field's new value: null clears it, a string must pass `valid`.
fn clean(value: &Value, key: &str, valid: fn(&str) -> Result<String>) -> Result<Option<String>> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) => valid(s).map(Some).map_err(|e| anyhow!("{key}: {e}")),
        other => Err(anyhow!("{key} must be a string or null, not {other}")),
    }
}

fn valid_display_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > NAME_MAX_CHARS || name.chars().any(char::is_control) {
        return Err(anyhow!("must be 1 to {NAME_MAX_CHARS} characters with no control characters"));
    }
    Ok(name.to_string())
}

fn valid_accent(accent: &str) -> Result<String> {
    let hex = accent.strip_prefix('#').unwrap_or("");
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(accent.to_string())
    } else {
        Err(anyhow!("must be #rrggbb"))
    }
}

fn valid_avatar(avatar: &str) -> Result<String> {
    let Some(rest) = AVATAR_PREFIXES.iter().find_map(|p| avatar.strip_prefix(p)) else {
        return Err(anyhow!("must be a png, jpeg or webp base64 data URL"));
    };
    if avatar.len() > AVATAR_MAX_BYTES {
        return Err(anyhow!("is too big (at most {AVATAR_MAX_BYTES} bytes)"));
    }
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')) {
        return Err(anyhow!("is not base64"));
    }
    Ok(avatar.to_string())
}

fn append_line_atomic(path: &Path, line: &str, mode: u32) -> Result<()> {
    let mut existing = fs::read_to_string(path)
        .with_context(|| format!("can't read {}", path.display()))?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push_str(line);
    write_atomic(path, existing.as_bytes(), mode)
}

fn remove_user_line_atomic(path: &Path, user: &str, mode: u32) -> Result<()> {
    let existing = fs::read_to_string(path)
        .with_context(|| format!("can't read {}", path.display()))?;
    let prefix = format!("{user}:");
    let mut out = String::with_capacity(existing.len());
    for line in existing.lines() {
        if !line.starts_with(&prefix) {
            out.push_str(line);
            out.push('\n');
        }
    }
    write_atomic(path, out.as_bytes(), mode)
}

fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let tmp = dir.join(format!(
        ".{name}.{}.{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(anyhow!("can't write {}: {e}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\n\
        nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin\n\
        geckouser:x:1000:1000:Gecko User,,,:/home/geckouser:/bin/bash\n\
        pyxisapp:x:1001:1001::/home/pyxisapp:/usr/sbin/nologin\n\
        ada:x:1002:1002::/home/ada:/bin/bash\n\
        sam:x:1003:1003:Sam Lee:/home/sam:/bin/bash\n";

    /// A user tree with a shadow whose geckouser has no password.
    fn tree(shadow: &str) -> (tempfile::TempDir, Users) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("etc")).unwrap();
        fs::write(dir.path().join("etc/passwd"), PASSWD).unwrap();
        fs::write(dir.path().join("etc/shadow"), shadow).unwrap();
        let users = Users::with_root(dir.path());
        (dir, users)
    }

    const NO_PASSWORD: &str = "root:*:1:0:99999:7:::\ngeckouser:*:20000:0:99999:7:::\nada:!:1:0:99999:7:::\nsam::1:0:99999:7:::\n";

    #[test]
    fn real_users_are_uid_1000_up_minus_the_app_user_and_nobody() {
        let (_dir, users) = tree(NO_PASSWORD);
        let names: Vec<String> = users.real_users().unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["geckouser", "ada", "sam"]);
        assert!(users.require_real("pyxisapp").is_err());
        assert!(users.require_real("root").is_err());
        assert!(users.require_real("nobody").is_err());
        assert!(users.require_real("../etc/passwd").is_err());
        assert!(users.require_real("").is_err());
        assert!(users.require_real("ada").is_ok());
    }

    #[test]
    fn a_shadow_field_of_star_bang_or_nothing_is_no_password() {
        assert!(!hash_is_set(""));
        assert!(!hash_is_set("*"));
        assert!(!hash_is_set("!"));
        assert!(!hash_is_set("!!"));
        assert!(hash_is_set("$6$salt$hash"));
        // A locked account keeps its hash behind the bang: a password is set,
        // and nothing will match it.
        assert!(hash_is_set("!$6$salt$hash"));

        let (_dir, users) = tree(NO_PASSWORD);
        assert!(!users.has_password("geckouser").unwrap());
        assert!(!users.has_password("ada").unwrap());
        assert!(!users.has_password("sam").unwrap());
        // No line at all: nothing to protect.
        assert!(!users.has_password("ghost").unwrap());
        assert_eq!(users.password_hash("ghost").unwrap(), None);
    }

    #[test]
    fn an_unreadable_shadow_is_an_error_not_no_password() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Users::with_root(dir.path()).has_password("geckouser").is_err());
    }

    #[test]
    fn profiles_default_from_gecos_and_merge_what_was_chosen() {
        let (_dir, users) = tree(NO_PASSWORD);
        let p = users.profile("geckouser");
        assert_eq!(p, Profile { display_name: "Gecko User".into(), accent: DEFAULT_ACCENT.into(), avatar: None });
        // No GECOS name: the user name.
        assert_eq!(users.profile("ada").display_name, "ada");
        assert_eq!(users.profile("sam").display_name, "Sam Lee");

        let svc = AccountsService::new(users.clone(), Arc::new(Limiter::new()), "/bin/false");
        let avatar = "data:image/png;base64,iVBORw0KGgo=";
        svc.set_profile(&json!({"user": "geckouser", "display_name": "  Andrew ", "accent": "#FF8800", "avatar": avatar}))
            .unwrap();
        let p = users.profile("geckouser");
        assert_eq!(p.display_name, "Andrew");
        assert_eq!(p.accent, "#ff8800");
        assert_eq!(p.avatar.as_deref(), Some(avatar));

        // A field left out is kept; null goes back to the default.
        svc.set_profile(&json!({"user": "geckouser", "accent": null})).unwrap();
        let p = users.profile("geckouser");
        assert_eq!(p.display_name, "Andrew");
        assert_eq!(p.accent, DEFAULT_ACCENT);
        assert_eq!(p.avatar.as_deref(), Some(avatar));

        // The file is world-readable, and no temp file is left behind.
        let file = users.profiles_dir().join("geckouser.json");
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o644);
        let names: Vec<_> = fs::read_dir(users.profiles_dir()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["geckouser.json"]);
    }

    #[test]
    fn list_merges_users_with_their_profiles() {
        let (_dir, users) = tree("geckouser:*:1::::::\nada:$6$x$y:1::::::\n");
        let svc = AccountsService::new(users, Arc::new(Limiter::new()), "/bin/false");
        svc.set_profile(&json!({"user": "ada", "display_name": "Ada L", "accent": "#112233"})).unwrap();
        let list = svc.list().unwrap();
        let users = list["users"].as_array().unwrap();
        assert_eq!(users.len(), 3);
        assert_eq!(users[0]["user"], "geckouser");
        assert_eq!(users[0]["uid"], 1000);
        assert_eq!(users[0]["display_name"], "Gecko User");
        assert_eq!(users[0]["has_password"], false);
        assert!(users[0]["avatar"].is_null());
        assert_eq!(users[1]["display_name"], "Ada L");
        assert_eq!(users[1]["accent"], "#112233");
        assert_eq!(users[1]["has_password"], true);
        // sam has no shadow line at all.
        assert_eq!(users[2]["has_password"], false);
    }

    #[test]
    fn set_profile_refuses_what_it_can_not_store() {
        let (_dir, users) = tree(NO_PASSWORD);
        let svc = AccountsService::new(users.clone(), Arc::new(Limiter::new()), "/bin/false");
        let bad = [
            json!({"user": "pyxisapp", "display_name": "x"}),
            json!({"user": "../x", "display_name": "x"}),
            json!({"user": "ada", "display_name": ""}),
            json!({"user": "ada", "display_name": "a\u{7}b"}),
            json!({"user": "ada", "display_name": "x".repeat(NAME_MAX_CHARS + 1)}),
            json!({"user": "ada", "display_name": 5}),
            json!({"user": "ada", "accent": "red"}),
            json!({"user": "ada", "accent": "#12345"}),
            json!({"user": "ada", "accent": "#12345g"}),
            json!({"user": "ada", "avatar": "https://example.com/a.png"}),
            json!({"user": "ada", "avatar": "data:image/svg+xml;base64,AAAA"}),
            json!({"user": "ada", "avatar": "data:image/png;base64,not base64!"}),
            json!({"user": "ada", "avatar": format!("data:image/png;base64,{}", "A".repeat(AVATAR_MAX_BYTES))}),
            json!({"display_name": "x"}),
        ];
        for body in bad {
            assert!(svc.set_profile(&body).is_err(), "{body}");
        }
        // Nothing was written by any of them.
        assert!(!users.profiles_dir().join("ada.json").exists());
    }

    /// A stand-in `chpasswd` that records its argv and stdin.
    fn fake_chpasswd(dir: &Path, exit: i32) -> PathBuf {
        let script = dir.join("chpasswd");
        fs::write(
            &script,
            format!("#!/bin/sh\necho \"$@\" > \"{d}/argv\"\ncat > \"{d}/stdin\"\nexit {exit}\n", d = dir.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[test]
    fn set_password_with_none_set_needs_no_current_and_runs_chpasswd_on_stdin() {
        let (dir, users) = tree(NO_PASSWORD);
        let bin = tempfile::tempdir().unwrap();
        let svc = AccountsService::new(users, Arc::new(Limiter::new()), fake_chpasswd(bin.path(), 0));
        let r = svc.set_password_at(&json!({"user": "geckouser", "new": "hunter2: yes"}), 1).unwrap();
        assert_eq!(r, json!({"ok": true}));
        assert_eq!(fs::read_to_string(bin.path().join("stdin")).unwrap(), "geckouser:hunter2: yes\n");
        // Under a test root chpasswd is pointed at it, and the password is
        // never in argv.
        let argv = fs::read_to_string(bin.path().join("argv")).unwrap();
        assert_eq!(argv.trim(), format!("--root {}", dir.path().display()));
        assert!(!argv.contains("hunter2"));
    }

    #[test]
    fn set_password_refuses_input_that_would_break_chpasswds_format() {
        let (_dir, users) = tree(NO_PASSWORD);
        let bin = tempfile::tempdir().unwrap();
        let svc = AccountsService::new(users, Arc::new(Limiter::new()), fake_chpasswd(bin.path(), 0));
        for new in ["", "a\nroot:b", "a\rb", "a\0b", &"x".repeat(PASSWORD_MAX_BYTES + 1)] {
            assert!(svc.set_password_at(&json!({"user": "geckouser", "new": new}), 1).is_err(), "{new:?}");
        }
        assert!(svc.set_password_at(&json!({"user": "pyxisapp", "new": "x"}), 1).is_err());
        assert!(svc.set_password_at(&json!({"user": "root", "new": "x"}), 1).is_err());
        assert!(svc.set_password_at(&json!({"user": "geckouser"}), 1).is_err());
        // chpasswd never ran.
        assert!(!bin.path().join("stdin").exists());
    }

    #[test]
    fn a_failing_chpasswd_is_an_error() {
        let (_dir, users) = tree(NO_PASSWORD);
        let bin = tempfile::tempdir().unwrap();
        let svc = AccountsService::new(users, Arc::new(Limiter::new()), fake_chpasswd(bin.path(), 1));
        let e = svc.set_password_at(&json!({"user": "geckouser", "new": "x"}), 1).unwrap_err();
        assert!(format!("{e:#}").contains("chpasswd failed"), "{e:#}");
    }

    #[test]
    fn a_wrong_current_counts_against_the_shared_limiter() {
        let hash = super::super::session::test_hash("right");
        let (_dir, users) = tree(&format!("geckouser:{hash}:1::::::\n"));
        let bin = tempfile::tempdir().unwrap();
        let limiter = Arc::new(Limiter::new());
        let svc = AccountsService::new(users, limiter.clone(), fake_chpasswd(bin.path(), 0));
        let body = |current: &str| json!({"user": "geckouser", "current": current, "new": "next"});

        let r = svc.set_password_at(&body("nope"), 1_000).unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(r["reason"], "wrong");
        assert_eq!(r["attempts_left"], 4);
        // A missing current is a wrong one.
        assert_eq!(svc.set_password_at(&json!({"user": "geckouser", "new": "next"}), 1_000).unwrap()["reason"], "wrong");
        assert!(!bin.path().join("stdin").exists());

        // The limiter is the one unlock reads: three more and it waits.
        for _ in 0..3 {
            svc.set_password_at(&body("nope"), 1_000).unwrap();
        }
        assert_eq!(limiter.status("geckouser", 1_000), (0, Some(31_000)));
        let r = svc.set_password_at(&body("right"), 1_001).unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(r["reason"], "wait");
        assert_eq!(r["retry_at_ms"], 31_000);
        assert!(!bin.path().join("stdin").exists());

        // After the wait the right one goes through and resets the count.
        assert_eq!(svc.set_password_at(&body("right"), 31_001).unwrap(), json!({"ok": true}));
        assert!(bin.path().join("stdin").exists());
        assert_eq!(limiter.status("geckouser", 31_002), (5, None));
    }

    #[test]
    fn create_and_delete_manage_multi_user_accounts_safely() {
        let (dir, users) = tree(NO_PASSWORD);
        let svc = AccountsService::new(users.clone(), Arc::new(Limiter::new()), "/bin/false");

        let created = svc
            .create(&json!({
                "user": "grace",
                "display_name": "Grace Hopper",
                "accent": "#EC4899",
                "password": "cobol",
            }))
            .unwrap();
        assert_eq!(created["ok"], true);
        assert_eq!(created["user"]["user"], "grace");
        assert_eq!(created["user"]["uid"], 1004);
        assert_eq!(created["user"]["display_name"], "Grace Hopper");
        assert_eq!(created["user"]["accent"], "#ec4899");
        assert_eq!(created["user"]["has_password"], true);
        assert!(dir.path().join("home/grace").is_dir());

        let hash = users.password_hash("grace").unwrap().unwrap();
        assert!(verify_password(&hash, "cobol"));
        assert!(!verify_password(&hash, "wrong"));

        // Duplicate, reserved, or invalid usernames are refused.
        assert!(svc.create(&json!({"user": "grace"})).is_err());
        assert!(svc.create(&json!({"user": "pyxisapp"})).is_err());
        assert!(svc.create(&json!({"user": "root"})).is_err());
        assert!(svc.create(&json!({"user": "BadName"})).is_err());

        // Primary and active session users cannot be deleted.
        assert!(svc.delete(&json!({"user": "geckouser"}), "grace").is_err());
        assert!(svc.delete(&json!({"user": "grace"}), "grace").is_err());

        // Deleting a non-active secondary user removes them from passwd, shadow, and profiles.
        let deleted = svc.delete(&json!({"user": "grace"}), "geckouser").unwrap();
        assert_eq!(deleted, json!({"ok": true, "user": "grace"}));
        assert!(users.require_real("grace").is_err());
        assert_eq!(users.password_hash("grace").unwrap(), None);
        assert!(!users.profiles_dir().join("grace.json").exists());
    }

    #[test]
    fn the_first_password_needs_no_current_and_is_not_counted() {
        for shadow in ["geckouser:*:1::::::\n", "geckouser:!:1::::::\n"] {
            let (_dir, users) = tree(shadow);
            let bin = tempfile::tempdir().unwrap();
            let limiter = Arc::new(Limiter::new());
            let svc = AccountsService::new(users, limiter.clone(), fake_chpasswd(bin.path(), 0));
            let r = svc.set_password_at(&json!({"user": "geckouser", "new": "first"}), 1).unwrap();
            assert_eq!(r, json!({"ok": true}));
            assert_eq!(limiter.status("geckouser", 1), (5, None));
        }
    }
}
