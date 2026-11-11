//! Confined filesystem calls: the half of the filesystem service that apps
//! get.
//!
//! An app never reaches `org.geckoOS.filesystem` directly. Its calls go
//! through the bridge (`PyxisBridgeParent.sys.mjs`), which stamps each one
//! with a `scope`, over anything the page sent. Only the bridge can: it knows
//! which app is calling, and echo does not (a connection carries no peer
//! credentials, and a message's `sender` is whatever the client wrote). The
//! scope names the one directory tree the call may reach:
//!
//! - `app:<id>`: the app's own data, `/var/lib/pyxis/data/<id>`. Every app
//!   has one. Echo makes it on first use, the app user's (pyxisapp, the user
//!   every Nova-launched app runs as), mode 0700, and the app sees it as `~`. An app holding the privileged `filesystem:apps` (the
//!   shell's desktop search) may name another app's id here instead.
//! - `user`: the user's home, `/home/geckouser`, for an app holding the
//!   privileged `filesystem:user` (the Files app).
//! - `system`: the root filesystem, read-only, for an app holding the
//!   privileged `filesystem:system` (the Files app again).
//!
//! Two independent layers keep a call inside:
//!
//! 1. The kernel resolves every path, with `openat2(RESOLVE_BENEATH |
//!    RESOLVE_NO_XDEV | RESOLVE_NO_MAGICLINKS)` from the scope's root. A `..`
//!    that climbs out, an absolute symlink, a relative one that leaves, or a
//!    mount point all fail with EXDEV. Nothing can change between the check
//!    and the use, because the lookup is the check.
//! 2. The call runs on a thread of its own that drops to a user (no
//!    supplementary groups, and so no capabilities) before it touches a
//!    path. An `app:<id>` call runs as the app user, the owner of the data
//!    directory; `user` and `system` run as geckouser, the shell's user, whose
//!    home and files they serve. Anything that got past (1) still meets the kernel's
//!    permission checks as the user: no /etc/shadow, no setuid-root bits,
//!    nothing in /usr/bin. The thread ends with the call, so nothing else in
//!    this root daemon ever runs with those credentials or has to undo them.
//!
//! RESOLVE_NO_XDEV matters most to `system`. /proc, /sys, /dev, /run and /tmp
//! are other mounts, so they stay out of reach, and /proc would be a hole:
//! the scoped thread is in echo-daemon's thread group, and the kernel lets a
//! thread read its own process's /proc/self/mem and environ whatever its uid.
//!
//! A scope's paths start at `~`: `~` is its root, `~/a/b` and `a/b` both name
//! root/a/b, and replies give paths back the same way. So an app never learns
//! or depends on where its storage lives, and the device answers like the
//! SDK's mock, whose tree is rooted at `~` too. `system` is the exception: its
//! root is `/`, so its paths are absolute, both ways. An absolute path in the
//! other two scopes is refused, and so is `..` in any of them.
//!
//! A call without a scope is the unconfined service in `filesystem.rs`, run as
//! root. Only a client holding the Echo socket can make one (echo-cli, native
//! tools), and those can already run anything as root through
//! `process.spawn`. Confining them needs identity on the bus itself, which is
//! a different change.

use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::fs::{lchown, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, ResolveFlags, Stat, StatxFlags};
use rustix::io::Errno;
use rustix::path::Arg;
use rustix::process::{Gid, Uid};
use serde_json::{json, Value};

use super::{encode_content, over_read_limit, DEFAULT_MAX_READ_BYTES};
use crate::services::nova::{app_owner, GECKO_GID, GECKO_HOME, GECKO_UID};

/// The parent of every app's own directory. Root's, mode 0755, so nothing an
/// app runs can add, remove or swap an entry in it.
pub const APP_DATA_ROOT: &str = "/var/lib/pyxis/data";

/// The methods that change something, which the read-only `system` scope
/// refuses. The bridge gates exactly these on `filesystem:write`, and a test
/// in nova.rs keeps the two lists the same.
pub(crate) const WRITES: &[&str] = &[
    "writeFile",
    "appendFile",
    "deleteFile",
    "deleteDirectory",
    "createDirectory",
    "copy",
    "move",
    "chmod",
];

/// How deep a recursive copy or delete goes before it gives up.
const MAX_DEPTH: usize = 128;

/// How openat2 resolves every path: beneath the directory it starts from, on
/// that directory's mount, and never through /proc's magic links.
const BENEATH: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_XDEV)
    .union(ResolveFlags::NO_MAGICLINKS);

/// BENEATH, following no symlink at all: for the walk of a recursive copy or
/// delete, which must never leave the tree it is walking.
const NO_LINKS: ResolveFlags = BENEATH.union(ResolveFlags::NO_SYMLINKS);

/// fchmodat2(2)'s number. libc names it only for some targets, but it is 452
/// on every architecture: it arrived in Linux 6.6, after the syscall tables
/// were unified.
const SYS_FCHMODAT2: libc::c_long = 452;

/// Which directory tree a call can reach, as the bridge names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// `app:<id>`: the app's own data.
    App(String),
    /// `user`: the user's home.
    User,
    /// `system`: the root filesystem, read-only.
    System,
}

impl Scope {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "user" => Ok(Scope::User),
            "system" => Ok(Scope::System),
            _ => match raw.strip_prefix("app:") {
                Some(id) if valid_app_id(id) => Ok(Scope::App(id.to_string())),
                Some(id) => Err(anyhow!("not an app id: {id:?}")),
                None => Err(anyhow!("unknown filesystem scope {raw:?}")),
            },
        }
    }
}

/// An app id as vroom and Nova write them: dot-separated parts made of
/// lowercase letters, digits, `_` and `-`, starting with a letter or a digit.
/// The id becomes a single path component, so it must never hold a `/` or be
/// `.` or `..`, which this rules out.
pub(crate) fn valid_app_id(id: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    };
    id.len() <= 128
        && id
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && id.split('.').all(part)
}

/// Answers one scoped call, on a thread of its own (see the module doc).
pub async fn call(scope: Scope, method: &str, body: &Value) -> Result<Value> {
    let (method, body) = (method.to_string(), body.clone());
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("fs-scoped".to_string())
        .spawn(move || {
            let _ = tx.send(run(&scope, &method, &body));
        })
        .context("start a thread for a scoped filesystem call")?;
    rx.await
        .map_err(|_| anyhow!("the scoped filesystem call ended without an answer"))?
}

fn run(scope: &Scope, method: &str, body: &Value) -> Result<Value> {
    // Before the drop: an app's directory is made as root, since its parent
    // is root's.
    let jail = Jail::for_scope(scope)?;
    become_user(jail.owner)?;
    let root = jail.open_root()?;
    jail.call(&root, method, body)
}

/// Makes the calling thread `owner` for the rest of its life. The raw
/// syscalls behind rustix's `set_thread_*` change only the caller, where
/// glibc's setresuid(3) would change every thread in this root daemon. Once
/// none of a thread's uids is 0 the kernel clears its capabilities, so from
/// here on the thread passes no permission check that the user would fail.
fn become_user(owner: Owner) -> Result<()> {
    let (uid, gid) = (Uid::from_raw(owner.uid), Gid::from_raw(owner.gid));
    if rustix::process::geteuid().is_root() {
        rustix::thread::set_thread_groups(&[]).context("drop supplementary groups")?;
        rustix::thread::set_thread_res_gid(gid, gid, gid).context("switch to the user's gid")?;
        rustix::thread::set_thread_res_uid(uid, uid, uid).context("switch to the user's uid")?;
    }
    if rustix::process::geteuid() != uid || rustix::process::getegid() != gid {
        bail!(
            "a scoped filesystem call needs echo-daemon to run as root, or as uid {}",
            owner.uid
        );
    }
    Ok(())
}

/// A user and group a scoped call runs as, and whose files it serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Owner {
    pub uid: u32,
    pub gid: u32,
}

impl Owner {
    /// The shell's user.
    const SHELL: Owner = Owner { uid: GECKO_UID, gid: GECKO_GID };

    /// Whoever owns app `id`'s own files: the app user, or the shell's user
    /// for a shell id (see `nova::app_owner`).
    fn of_app(id: &str) -> Owner {
        let (uid, gid) = app_owner(id);
        Owner { uid, gid }
    }
}

/// Gives `dir` and everything under it to `uid:gid`, without ever following a
/// symlink: each level is opened `O_NOFOLLOW|O_DIRECTORY` and each entry
/// changed with `AT_SYMLINK_NOFOLLOW`, relative to the directory it was found
/// in. This runs as root over a tree that was the previous owner's to
/// rearrange (a profile or data dir of an earlier image, written by an app
/// running as geckouser), so a path-based walk could be led out of it by a
/// swapped component. Files with more than one link are left alone: another
/// name for the same inode may be somewhere this walk is not entitled to.
/// Does not cross onto another filesystem.
pub(crate) fn chown_tree(dir: &Path, uid: u32, gid: u32) -> Result<()> {
    let (uid, gid) = (Uid::from_raw(uid), Gid::from_raw(gid));
    let root = rustix::fs::open(
        dir,
        OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::RDONLY,
        Mode::empty(),
    )
    .with_context(|| format!("open {}", dir.display()))?;
    let dev = rustix::fs::fstat(&root).context("stat the directory")?.st_dev;
    rustix::fs::fchown(&root, Some(uid), Some(gid)).context("chown the directory")?;
    chown_dir_contents(root, dev, uid, gid, 0)
}

fn chown_dir_contents(dir: OwnedFd, dev: u64, uid: Uid, gid: Gid, depth: u32) -> Result<()> {
    // Deep enough for any profile, shallow enough that a hostile tree can't
    // exhaust the descriptors this walk holds.
    if depth > 64 {
        bail!("directory tree is too deep");
    }
    let names: Vec<std::ffi::CString> = Dir::read_from(&dir)
        .context("read a directory")?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_owned())
        .filter(|n| n.to_bytes() != b"." && n.to_bytes() != b"..")
        .collect();
    for name in names {
        let Ok(st) = rustix::fs::statat(&dir, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW) else {
            continue;
        };
        if st.st_dev != dev {
            continue;
        }
        match kind(&st) {
            FileType::Directory => {
                let Ok(child) = rustix::fs::openat(
                    &dir,
                    name.as_c_str(),
                    OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::RDONLY,
                    Mode::empty(),
                ) else {
                    continue;
                };
                // The directory we opened is the one we statted, or a
                // different one on this filesystem that is still the tree's.
                rustix::fs::fchown(&child, Some(uid), Some(gid)).context("chown a directory")?;
                chown_dir_contents(child, dev, uid, gid, depth + 1)?;
            }
            FileType::Symlink => {
                let _ = rustix::fs::chownat(&dir, name.as_c_str(), Some(uid), Some(gid), AtFlags::SYMLINK_NOFOLLOW);
            }
            FileType::RegularFile if st.st_nlink > 1 => {}
            _ => {
                let _ = rustix::fs::chownat(&dir, name.as_c_str(), Some(uid), Some(gid), AtFlags::SYMLINK_NOFOLLOW);
            }
        }
    }
    Ok(())
}

/// `/var/lib/pyxis/data/<id>`, made if missing: the app user's, mode 0700.
/// Runs as root, since the parent is root's. That is also why nothing an app
/// runs can put a symlink where this directory should be: only root can add
/// an entry next to it.
fn app_dir(id: &str, owner: Owner) -> Result<PathBuf> {
    let data = Path::new(APP_DATA_ROOT);
    match fs::symlink_metadata(data) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => bail!("{} is not a directory", data.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(data).with_context(|| format!("create {}", data.display()))?;
            fs::set_permissions(data, fs::Permissions::from_mode(0o755))?;
        }
        Err(e) => return Err(e).with_context(|| format!("inspect {}", data.display())),
    }

    let dir = data.join(id);
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => {
            if meta.uid() != owner.uid || meta.gid() != owner.gid {
                // A directory of an earlier image is geckouser's, with files
                // the app user could not write: its contents move too.
                chown_tree(&dir, owner.uid, owner.gid)
                    .with_context(|| format!("give {} to the user", dir.display()))?;
            }
        }
        Ok(_) => bail!("{} is not a directory", dir.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            match fs::create_dir(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e).with_context(|| format!("create {}", dir.display())),
            }
            lchown(&dir, Some(owner.uid), Some(owner.gid))
                .with_context(|| format!("give {} to the user", dir.display()))?;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        Err(e) => return Err(e).with_context(|| format!("inspect {}", dir.display())),
    }
    Ok(dir)
}

/// Deletes an uninstalled app's own data, `/var/lib/pyxis/data/<id>`, and
/// says whether there was any. Nova's `forget_app_state` calls it, with the
/// app's profile, so a package that takes the id later starts empty instead
/// of inheriting what the old app kept. Runs as root, and `remove_dir_all`
/// removes a symlink inside rather than following it.
pub fn remove_app_data(id: &str) -> Result<bool> {
    if !valid_app_id(id) {
        bail!("not an app id: {id:?}");
    }
    let dir = Path::new(APP_DATA_ROOT).join(id);
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("remove {}", dir.display())),
    }
}

/// A scope made concrete: the directory its calls cannot leave.
struct Jail {
    root: PathBuf,
    writable: bool,
    /// Whether paths are absolute (`/usr/share`), as in `system`, rather
    /// than rooted at `~` (`~/notes.json`).
    absolute: bool,
    /// The root, for messages.
    name: &'static str,
    /// Who the call runs as.
    owner: Owner,
}

/// A path relative to a jail's root, as openat2 is given it: names joined by
/// `/`, never `.` or `..`, and empty for the root itself.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rel(String);

impl Rel {
    fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// What openat2 takes: `.` for the root.
    fn arg(&self) -> &str {
        if self.0.is_empty() {
            "."
        } else {
            &self.0
        }
    }

    fn join(&self, name: &str) -> Rel {
        if self.0.is_empty() {
            Rel(name.to_string())
        } else {
            Rel(format!("{}/{name}", self.0))
        }
    }

    /// The directory holding this path, and its last name. `None` for the
    /// root.
    fn split(&self) -> Option<(Rel, &str)> {
        if self.0.is_empty() {
            return None;
        }
        Some(match self.0.rsplit_once('/') {
            Some((parent, name)) => (Rel(parent.to_string()), name),
            None => (Rel(String::new()), self.0.as_str()),
        })
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|name| !name.is_empty())
    }
}

/// A string field of a call's body, empty when it is missing: in a scope, an
/// empty path is the root.
fn text<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or("")
}

/// openat2, tried again on EAGAIN, which it returns when a concurrent rename
/// races a `..` in the lookup (openat2(2)). Only a symlink's target can hold
/// one, since `Jail::parse` refuses them in paths.
fn open<P: Arg + Copy>(
    dir: impl AsFd,
    path: P,
    flags: OFlags,
    mode: Mode,
    resolve: ResolveFlags,
) -> rustix::io::Result<OwnedFd> {
    let mut tries = 0;
    loop {
        match rustix::fs::openat2(dir.as_fd(), path, flags | OFlags::CLOEXEC, mode, resolve) {
            Err(Errno::AGAIN) if tries < 8 => tries += 1,
            result => return result,
        }
    }
}

fn kind(st: &Stat) -> FileType {
    FileType::from_raw_mode(st.st_mode as _)
}

fn same_file(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}

/// Seconds since the epoch, or `None` before it.
fn secs(seconds: i64) -> Option<u64> {
    u64::try_from(seconds).ok()
}

impl Jail {
    fn for_scope(scope: &Scope) -> Result<Self> {
        Ok(match scope {
            Scope::App(id) => {
                let owner = Owner::of_app(id);
                Jail {
                    root: app_dir(id, owner)?,
                    writable: true,
                    absolute: false,
                    name: "the app's own storage",
                    owner,
                }
            }
            Scope::User => Jail {
                root: PathBuf::from(GECKO_HOME),
                writable: true,
                absolute: false,
                name: "the home folder",
                owner: Owner::SHELL,
            },
            Scope::System => Jail {
                root: PathBuf::from("/"),
                writable: false,
                absolute: true,
                name: "the root filesystem",
                owner: Owner::SHELL,
            },
        })
    }

    /// The root, as the descriptor every path resolves beneath. `run` opens
    /// it after the drop, so it is opened as the user.
    fn open_root(&self) -> Result<OwnedFd> {
        rustix::fs::open(
            &self.root,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| format!("open {}", self.root.display()))
    }

    fn call(&self, root: &OwnedFd, method: &str, body: &Value) -> Result<Value> {
        if !self.writable && WRITES.contains(&method) {
            bail!("{} is read-only here: {method} is refused", self.name);
        }
        let encoding = body.get("encoding").and_then(Value::as_str).unwrap_or("utf-8");
        let mode = body.get("mode").and_then(Value::as_u64);
        match method {
            "readDirectory" => self.read_directory(root, text(body, "path")),
            "readFile" => self.read_file(
                root,
                text(body, "path"),
                encoding,
                body.get("maxBytes").and_then(Value::as_u64),
            ),
            "writeFile" => self.write_file(root, text(body, "path"), text(body, "content"), encoding),
            "appendFile" => {
                self.append_file(root, text(body, "path"), text(body, "content"), encoding)
            }
            "deleteFile" => self.delete_file(root, text(body, "path")),
            "deleteDirectory" => self.delete_directory(
                root,
                text(body, "path"),
                body.get("recursive").and_then(Value::as_bool).unwrap_or(false),
            ),
            "createDirectory" => self.create_directory(root, text(body, "path"), mode),
            "exists" => self.exists(root, text(body, "path")),
            "stat" => self.stat(root, text(body, "path")),
            "copy" => self.copy(root, text(body, "source"), text(body, "destination")),
            "move" => self.rename(root, text(body, "source"), text(body, "destination")),
            "chmod" => self.chmod(root, text(body, "path"), mode.unwrap_or(0o755)),
            other => bail!("org.geckoOS.filesystem has no method {other:?}"),
        }
    }

    /// A path the app sent, as one the kernel resolves beneath the root.
    fn parse(&self, raw: &str) -> Result<Rel> {
        if raw.contains('\0') {
            bail!("a path may not contain a NUL byte");
        }
        let rest = if self.absolute {
            if raw.is_empty() || raw.starts_with('/') {
                raw.trim_start_matches('/')
            } else {
                bail!("paths in {} are absolute, and {raw:?} is not", self.name);
            }
        } else if raw.is_empty() || raw == "~" {
            ""
        } else if let Some(rest) = raw.strip_prefix("~/") {
            rest
        } else if raw.starts_with('~') {
            bail!("{raw:?} names another user's home, which is not in {}", self.name);
        } else if raw.starts_with('/') {
            bail!("{raw} is outside {}: use a path under ~", self.name);
        } else {
            raw
        };
        let mut names = Vec::new();
        for name in rest.split('/') {
            match name {
                "" | "." => {}
                ".." => bail!("{raw:?} climbs with '..', which no path in {} may do", self.name),
                name => names.push(name),
            }
        }
        Ok(Rel(names.join("/")))
    }

    /// A path as the app sees it: `~/a/b`, or `/a/b` in the system scope.
    fn show(&self, rel: &Rel) -> String {
        if self.absolute {
            format!("/{}", rel.0)
        } else if rel.is_root() {
            "~".to_string()
        } else {
            format!("~/{}", rel.0)
        }
    }

    /// `errno`, from looking up or acting on `rel`, in the words the unscoped
    /// service uses, so an app's messages read the same either way. `what`
    /// is the noun those messages start with ("File", "Source"…).
    fn fail(&self, errno: Errno, what: &str, rel: &Rel) -> anyhow::Error {
        let shown = self.show(rel);
        match errno {
            Errno::NOENT => anyhow!("{what} does not exist: {shown}"),
            Errno::XDEV => anyhow!("{shown} leads outside {}", self.name),
            Errno::ACCESS | Errno::PERM => anyhow!("Permission denied: {shown}"),
            Errno::NOTDIR => anyhow!("Something on the way to {shown} is not a directory"),
            Errno::ISDIR => anyhow!("Path is a directory: {shown}"),
            Errno::LOOP => anyhow!("Too many levels of symbolic links: {shown}"),
            Errno::EXIST => anyhow!("Path already exists: {shown}"),
            Errno::NOTEMPTY => anyhow!("Directory is not empty: {shown}"),
            other => anyhow!("{shown}: {}", io::Error::from(other)),
        }
    }

    /// The directory `rel`, as a descriptor to act on names inside it.
    fn open_dir(&self, root: &OwnedFd, rel: &Rel) -> Result<OwnedFd> {
        open(root, rel.arg(), OFlags::PATH | OFlags::DIRECTORY, Mode::empty(), BENEATH)
            .map_err(|e| self.fail(e, "Directory", rel))
    }

    /// An existing regular file, followed through symlinks that stay inside,
    /// with its length. O_NONBLOCK stops a FIFO holding the call until a
    /// writer comes along; it changes nothing for a regular file.
    fn open_file(&self, root: &OwnedFd, rel: &Rel, flags: OFlags, what: &str) -> Result<(File, u64)> {
        let fd = open(root, rel.arg(), flags | OFlags::NONBLOCK | OFlags::NOCTTY, Mode::empty(), BENEATH)
            .map_err(|e| self.fail(e, what, rel))?;
        let st = rustix::fs::fstat(&fd)?;
        if kind(&st) != FileType::RegularFile {
            bail!("Path is not a file: {}", self.show(rel));
        }
        Ok((File::from(fd), st.st_size as u64))
    }

    /// Makes `rel` and any of its parents that are missing, like
    /// `create_dir_all`, one name at a time: each parent is found by openat2
    /// from the root, and mkdirat gets only a single name inside it.
    fn make_dirs(&self, root: &OwnedFd, rel: &Rel) -> Result<()> {
        let mut done = Rel(String::new());
        for name in rel.names() {
            let next = done.join(name);
            match open(root, next.arg(), OFlags::PATH | OFlags::DIRECTORY, Mode::empty(), BENEATH) {
                Ok(_) => {}
                Err(Errno::NOENT) => {
                    let parent = self.open_dir(root, &done)?;
                    match rustix::fs::mkdirat(&parent, name, Mode::from_raw_mode(0o755)) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(e) => return Err(self.fail(e, "Path", &next)),
                    }
                }
                Err(e) => return Err(self.fail(e, "Path", &next)),
            }
            done = next;
        }
        Ok(())
    }

    fn read_directory(&self, root: &OwnedFd, raw: &str) -> Result<Value> {
        let rel = self.parse(raw)?;
        let dir = open(root, rel.arg(), OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty(), BENEATH)
            .map_err(|e| match e {
                Errno::NOTDIR => anyhow!("Path is not a directory: {}", self.show(&rel)),
                e => self.fail(e, "Path", &rel),
            })?;

        // As in the unscoped read_directory, an entry that can't be read
        // costs its own row, never the listing.
        let mut entries = Vec::new();
        for entry in Dir::read_from(&dir).context("read the directory")? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            let Ok(st) = rustix::fs::statx(
                &dir,
                name,
                AtFlags::SYMLINK_NOFOLLOW,
                StatxFlags::BASIC_STATS | StatxFlags::BTIME,
            ) else {
                continue;
            };
            let name = name.to_string_lossy();
            let file_type = FileType::from_raw_mode(st.stx_mode.into());
            let created = if st.stx_mask & StatxFlags::BTIME.bits() != 0 {
                secs(st.stx_btime.tv_sec)
            } else {
                None
            };
            entries.push(json!({
                "name": name,
                "path": self.show(&rel.join(&name)),
                "isFile": file_type == FileType::RegularFile,
                "isDirectory": file_type == FileType::Directory,
                "isSymlink": file_type == FileType::Symlink,
                "size": st.stx_size,
                "modified": secs(st.stx_mtime.tv_sec),
                "created": created,
                "permissions": format!("{:o}", st.stx_mode),
            }));
        }

        Ok(json!({
            "entries": entries,
            "path": self.show(&rel),
            "count": entries.len()
        }))
    }

    fn read_file(&self, root: &OwnedFd, raw: &str, encoding: &str, max_bytes: Option<u64>) -> Result<Value> {
        let rel = self.parse(raw)?;
        let shown = self.show(&rel);
        let (file, len) = self.open_file(root, &rel, OFlags::RDONLY, "File")?;
        let limit = max_bytes.unwrap_or(DEFAULT_MAX_READ_BYTES);
        if len > limit {
            return Err(over_read_limit(len, limit, &shown));
        }
        // Read one byte past the limit, in case the file grew since fstat.
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(over_read_limit(bytes.len() as u64, limit, &shown));
        }
        let size = bytes.len();
        let content = encode_content(bytes, encoding, &shown)?;
        Ok(json!({
            "content": content,
            "size": size,
            "encoding": encoding,
            "path": shown
        }))
    }

    fn write_file(&self, root: &OwnedFd, raw: &str, content: &str, encoding: &str) -> Result<Value> {
        let rel = self.parse(raw)?;
        let Some((parent, _)) = rel.split() else {
            bail!("Path is a directory: {}", self.show(&rel));
        };
        self.make_dirs(root, &parent)?;
        let fd = open(
            root,
            rel.arg(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::NONBLOCK | OFlags::NOCTTY,
            Mode::from_raw_mode(0o644),
            BENEATH,
        )
        .map_err(|e| self.fail(e, "Path", &rel))?;
        // Truncated only once it is known to be a regular file.
        if kind(&rustix::fs::fstat(&fd)?) != FileType::RegularFile {
            bail!("Path is not a file: {}", self.show(&rel));
        }
        rustix::fs::ftruncate(&fd, 0)?;
        File::from(fd).write_all(content.as_bytes())?;
        Ok(json!({
            "bytesWritten": content.len(),
            "path": self.show(&rel),
            "encoding": encoding,
            "success": true
        }))
    }

    fn append_file(&self, root: &OwnedFd, raw: &str, content: &str, encoding: &str) -> Result<Value> {
        let rel = self.parse(raw)?;
        let (mut file, _) = self.open_file(root, &rel, OFlags::WRONLY | OFlags::APPEND, "File")?;
        file.write_all(content.as_bytes())?;
        Ok(json!({
            "bytesWritten": content.len(),
            "path": self.show(&rel),
            "encoding": encoding,
            "success": true
        }))
    }

    fn delete_file(&self, root: &OwnedFd, raw: &str) -> Result<Value> {
        let rel = self.parse(raw)?;
        let Some((parent, name)) = rel.split() else {
            bail!("Path is a directory: {}", self.show(&rel));
        };
        let dir = self.open_dir(root, &parent)?;
        let st = rustix::fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| self.fail(e, "File", &rel))?;
        if kind(&st) == FileType::Directory {
            bail!("Path is a directory: {}", self.show(&rel));
        }
        // A symlink is removed, not what it points at.
        rustix::fs::unlinkat(&dir, name, AtFlags::empty()).map_err(|e| self.fail(e, "File", &rel))?;
        Ok(json!({ "success": true, "path": self.show(&rel) }))
    }

    fn delete_directory(&self, root: &OwnedFd, raw: &str, recursive: bool) -> Result<Value> {
        let rel = self.parse(raw)?;
        let Some((parent, name)) = rel.split() else {
            bail!("{} is the top of {}, which can't be deleted", self.show(&rel), self.name);
        };
        let dir = self.open_dir(root, &parent)?;
        let st = rustix::fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| self.fail(e, "Directory", &rel))?;
        if kind(&st) != FileType::Directory {
            bail!("Path is not a directory: {}", self.show(&rel));
        }
        if recursive {
            empty_tree(&dir, name, 0).with_context(|| format!("delete {}", self.show(&rel)))?;
        }
        rustix::fs::unlinkat(&dir, name, AtFlags::REMOVEDIR)
            .map_err(|e| self.fail(e, "Directory", &rel))?;
        Ok(json!({ "success": true, "path": self.show(&rel) }))
    }

    fn create_directory(&self, root: &OwnedFd, raw: &str, mode: Option<u64>) -> Result<Value> {
        let rel = self.parse(raw)?;
        let Some((parent, name)) = rel.split() else {
            bail!("Path already exists: {}", self.show(&rel));
        };
        match open(root, rel.arg(), OFlags::PATH, Mode::empty(), BENEATH) {
            Ok(_) => bail!("Path already exists: {}", self.show(&rel)),
            Err(Errno::NOENT) => {}
            Err(e) => return Err(self.fail(e, "Path", &rel)),
        }
        self.make_dirs(root, &parent)?;
        let dir = self.open_dir(root, &parent)?;
        rustix::fs::mkdirat(&dir, name, Mode::from_raw_mode(0o755))
            .map_err(|e| self.fail(e, "Path", &rel))?;
        if let Some(mode) = mode {
            let made = open(&dir, name, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW, Mode::empty(), NO_LINKS)
                .map_err(|e| self.fail(e, "Path", &rel))?;
            // Only the permission bits: no app needs setuid, setgid or sticky.
            rustix::fs::fchmod(&made, Mode::from_raw_mode(mode as u32 & 0o777))?;
        }
        Ok(json!({ "success": true, "path": self.show(&rel) }))
    }

    fn exists(&self, root: &OwnedFd, raw: &str) -> Result<Value> {
        let rel = self.parse(raw)?;
        let exists = match open(root, rel.arg(), OFlags::PATH, Mode::empty(), BENEATH) {
            Ok(_) => true,
            // What Path::exists answers false for. Leading outside the scope
            // is an error instead, like everywhere else.
            Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP | Errno::ACCESS) => false,
            Err(e) => return Err(self.fail(e, "Path", &rel)),
        };
        Ok(json!({ "exists": exists, "path": self.show(&rel) }))
    }

    fn stat(&self, root: &OwnedFd, raw: &str) -> Result<Value> {
        let rel = self.parse(raw)?;
        let fd = open(root, rel.arg(), OFlags::PATH, Mode::empty(), BENEATH)
            .map_err(|e| self.fail(e, "Path", &rel))?;
        let st = rustix::fs::fstat(&fd)?;
        let file_type = kind(&st);
        Ok(json!({
            "path": self.show(&rel),
            "size": st.st_size,
            "isFile": file_type == FileType::RegularFile,
            "isDirectory": file_type == FileType::Directory,
            "isSymlink": file_type == FileType::Symlink,
            "isBlockDevice": file_type == FileType::BlockDevice,
            "isCharDevice": file_type == FileType::CharacterDevice,
            "isFifo": file_type == FileType::Fifo,
            "isSocket": file_type == FileType::Socket,
            "permissions": format!("{:o}", st.st_mode),
            "owner": st.st_uid,
            "group": st.st_gid,
            "accessed": st.st_atime,
            "modified": st.st_mtime,
            "changed": st.st_ctime
        }))
    }

    fn copy(&self, root: &OwnedFd, source: &str, destination: &str) -> Result<Value> {
        let (src, dst) = (self.parse(source)?, self.parse(destination)?);
        let from = open(root, src.arg(), OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY, Mode::empty(), BENEATH)
            .map_err(|e| self.fail(e, "Source", &src))?;
        let st = rustix::fs::fstat(&from)?;
        match kind(&st) {
            FileType::RegularFile => {
                let to = open(
                    root,
                    dst.arg(),
                    OFlags::WRONLY | OFlags::CREATE | OFlags::NONBLOCK | OFlags::NOCTTY,
                    Mode::from_raw_mode(0o644),
                    BENEATH,
                )
                .map_err(|e| self.fail(e, "Path", &dst))?;
                copy_file(from, &st, to)
                    .with_context(|| format!("copy {} to {}", self.show(&src), self.show(&dst)))?;
            }
            FileType::Directory => {
                self.make_dirs(root, &dst)?;
                let to = open(root, dst.arg(), OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty(), BENEATH)
                    .map_err(|e| self.fail(e, "Path", &dst))?;
                let top = rustix::fs::fstat(&to)?;
                if same_file(&st, &top) {
                    bail!("{} can't be copied onto itself", self.show(&src));
                }
                copy_tree(&from, &to, &top, 0)
                    .with_context(|| format!("copy {} to {}", self.show(&src), self.show(&dst)))?;
            }
            _ => bail!("Only files and folders can be copied: {}", self.show(&src)),
        }
        Ok(json!({
            "success": true,
            "source": self.show(&src),
            "destination": self.show(&dst)
        }))
    }

    fn rename(&self, root: &OwnedFd, source: &str, destination: &str) -> Result<Value> {
        let (src, dst) = (self.parse(source)?, self.parse(destination)?);
        let Some((src_parent, src_name)) = src.split() else {
            bail!("{} is the top of {}, which can't be moved", self.show(&src), self.name);
        };
        let Some((dst_parent, dst_name)) = dst.split() else {
            bail!("Path already exists: {}", self.show(&dst));
        };
        let from = self.open_dir(root, &src_parent)?;
        let to = self.open_dir(root, &dst_parent)?;
        rustix::fs::statat(&from, src_name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| self.fail(e, "Source", &src))?;
        rustix::fs::renameat(&from, src_name, &to, dst_name).map_err(|e| self.fail(e, "Path", &dst))?;
        Ok(json!({
            "success": true,
            "source": self.show(&src),
            "destination": self.show(&dst)
        }))
    }

    fn chmod(&self, root: &OwnedFd, raw: &str, mode: u64) -> Result<Value> {
        let rel = self.parse(raw)?;
        if rel.is_root() {
            bail!("{} is the top of {}, whose permissions stay as they are", self.show(&rel), self.name);
        }
        // Only the permission bits: no app needs setuid, setgid or sticky.
        let mode = mode as u32 & 0o777;
        let fd = open(root, rel.arg(), OFlags::PATH, Mode::empty(), BENEATH)
            .map_err(|e| self.fail(e, "Path", &rel))?;
        chmod_fd(&fd, mode).map_err(|e| self.fail(e, "Path", &rel))?;
        Ok(json!({
            "success": true,
            "path": self.show(&rel),
            "mode": format!("{mode:o}")
        }))
    }
}

/// Changes exactly what `fd`, an O_PATH descriptor from openat2, refers to:
/// fchmodat2(fd, "", mode, AT_EMPTY_PATH). fchmod(2) refuses O_PATH
/// descriptors, and changing the mode by name would look the path up again,
/// outside the jail. Needs Linux 6.6.
fn chmod_fd(fd: &OwnedFd, mode: u32) -> rustix::io::Result<()> {
    // SAFETY: the arguments are a live descriptor, a NUL-terminated empty
    // string, and two integers; the kernel writes nothing back.
    let rc = unsafe {
        libc::syscall(
            SYS_FCHMODAT2,
            fd.as_raw_fd() as libc::c_long,
            c"".as_ptr(),
            mode as libc::c_long,
            libc::AT_EMPTY_PATH as libc::c_long,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(Errno::from_io_error(&io::Error::last_os_error()).unwrap_or(Errno::IO))
    }
}

/// The names in a directory, without `.` and `..`, read in full before
/// anything in it changes.
fn names_in(dir: &OwnedFd) -> Result<Vec<CString>> {
    let mut names = Vec::new();
    for entry in Dir::read_from(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if !matches!(name.to_bytes(), b"." | b"..") {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// Copies a regular file's bytes and permission bits, as `fs::copy` does, into
/// `to`. `to` is truncated first, but only once it is known to be a different
/// regular file: truncating the source would lose it.
fn copy_file(from: OwnedFd, st: &Stat, to: OwnedFd) -> Result<()> {
    let to_st = rustix::fs::fstat(&to)?;
    if same_file(st, &to_st) {
        bail!("the source and the destination are the same file");
    }
    if kind(&to_st) != FileType::RegularFile {
        bail!("the destination is not a file");
    }
    rustix::fs::ftruncate(&to, 0)?;
    rustix::fs::fchmod(&to, Mode::from_raw_mode(st.st_mode & 0o777))?;
    io::copy(&mut File::from(from), &mut File::from(to))?;
    Ok(())
}

/// Copies what is in `from` into `to`, one entry at a time, never following
/// a symlink or crossing into another mount. A symlink is copied as a
/// symlink, and sockets, FIFOs and devices are left out. `top` is the copy's
/// own top folder, skipped if it turns up: it is inside `from` when a folder
/// is copied into itself.
fn copy_tree(from: &OwnedFd, to: &OwnedFd, top: &Stat, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("folders nested more than {MAX_DEPTH} deep");
    }
    for name in names_in(from)? {
        let name = name.as_c_str();
        let st = rustix::fs::statat(from, name, AtFlags::SYMLINK_NOFOLLOW)?;
        match kind(&st) {
            FileType::Directory => {
                if same_file(&st, top) {
                    continue;
                }
                match rustix::fs::mkdirat(to, name, Mode::from_raw_mode(0o755)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(e) => return Err(e.into()),
                }
                let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW;
                let sub_from = open(from, name, flags, Mode::empty(), NO_LINKS)?;
                let sub_to = open(to, name, flags, Mode::empty(), NO_LINKS)?;
                copy_tree(&sub_from, &sub_to, top, depth + 1)?;
            }
            FileType::RegularFile => {
                let src = open(
                    from,
                    name,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY,
                    Mode::empty(),
                    NO_LINKS,
                )?;
                let src_st = rustix::fs::fstat(&src)?;
                let dst = open(
                    to,
                    name,
                    OFlags::WRONLY | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY,
                    Mode::from_raw_mode(0o644),
                    NO_LINKS,
                )?;
                copy_file(src, &src_st, dst)?;
            }
            FileType::Symlink => {
                let target = rustix::fs::readlinkat(from, name, Vec::new())?;
                match rustix::fs::symlinkat(target.as_c_str(), to, name) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Deletes everything in the directory `name` inside `parent`, never
/// following a symlink or crossing into another mount: a symlink is removed,
/// not what it points at.
fn empty_tree<P: Arg + Copy>(parent: &OwnedFd, name: P, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("folders nested more than {MAX_DEPTH} deep");
    }
    let dir = open(parent, name, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW, Mode::empty(), NO_LINKS)?;
    for child in names_in(&dir)? {
        let child = child.as_c_str();
        let st = rustix::fs::statat(&dir, child, AtFlags::SYMLINK_NOFOLLOW)?;
        if kind(&st) == FileType::Directory {
            empty_tree(&dir, child, depth + 1)?;
            rustix::fs::unlinkat(&dir, child, AtFlags::REMOVEDIR)?;
        } else {
            rustix::fs::unlinkat(&dir, child, AtFlags::empty())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A jail over a fresh temporary directory, called the way `run` calls
    /// one, but as whoever runs the tests: dropping to geckouser needs root,
    /// and is checked on the device instead (scripts/serial-tests/
    /// alpha5-fs-confine.txt).
    struct Fixture {
        dir: tempfile::TempDir,
        jail: Jail,
        root: OwnedFd,
    }

    impl Fixture {
        fn home() -> Self {
            Self::new(true, false)
        }

        fn new(writable: bool, absolute: bool) -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let jail = Jail {
                root: dir.path().to_path_buf(),
                writable,
                absolute,
                name: "the test jail",
                owner: Owner::SHELL,
            };
            let root = jail.open_root().expect("open the jail's root");
            Fixture { dir, jail, root }
        }

        fn call(&self, method: &str, body: Value) -> Result<Value> {
            self.jail.call(&self.root, method, &body)
        }

        fn err(&self, method: &str, body: Value) -> String {
            match self.call(method, body.clone()) {
                Ok(v) => panic!("{method} {body} should have been refused, got {v}"),
                Err(e) => format!("{e:#}"),
            }
        }

        fn at(&self, rel: &str) -> PathBuf {
            self.dir.path().join(rel)
        }
    }

    #[test]
    fn an_app_call_runs_as_the_app_user_and_the_shells_scopes_as_the_shell() {
        use crate::services::nova::{APP_GID, APP_UID};
        // Apps are not the shell's user: that is what stops one opening the
        // host Wayland socket. Their own data is theirs.
        assert_eq!(Owner::of_app("com.test.app"), Owner { uid: APP_UID, gid: APP_GID });
        assert_ne!(Owner::of_app("com.test.app"), Owner::SHELL);
        // The shell's own app (bloom) keeps its own state under its own user.
        assert_eq!(Owner::of_app("bloom"), Owner::SHELL);
        // `user` and `system` serve geckouser's home and the system's files.
        assert_eq!(Jail::for_scope(&Scope::User).unwrap().owner, Owner::SHELL);
        assert_eq!(Jail::for_scope(&Scope::System).unwrap().owner, Owner::SHELL);
    }

    #[test]
    fn a_thread_that_is_not_root_cannot_become_another_user() {
        // The drop is refused rather than skipped: a scoped call that could
        // not become its user must not run as someone else.
        if rustix::process::geteuid().is_root() {
            return;
        }
        let other = Owner { uid: rustix::process::geteuid().as_raw() + 1, gid: 0 };
        let err = become_user(other).unwrap_err();
        assert!(format!("{err:#}").contains("needs echo-daemon to run as root"), "{err:#}");
    }

    #[test]
    fn chown_tree_walks_directories_and_leaves_symlinks_unfollowed() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), "x").unwrap();
        let tree = tmp.path().join("tree");
        fs::create_dir_all(tree.join("a/b")).unwrap();
        fs::write(tree.join("a/b/file"), "x").unwrap();
        symlink(&outside, tree.join("a/escape")).unwrap();
        symlink(&tree, tree.join("a/loop")).unwrap();
        fs::hard_link(tree.join("a/b/file"), tree.join("a/linked")).unwrap();
        let (uid, gid) = (rustix::process::geteuid().as_raw(), rustix::process::getegid().as_raw());
        chown_tree(&tree, uid, gid).expect("a tree with a loop and an escaping link is walked once");
        assert!(chown_tree(&tree.join("a/escape"), uid, gid).is_err(), "a symlink is not a starting point");
        assert!(chown_tree(&tree.join("missing"), uid, gid).is_err());
    }

    #[test]
    fn paths_start_at_home_and_come_back_the_same_way() {
        let fx = Fixture::home();
        let wrote = fx
            .call("writeFile", json!({ "path": "~/notes/today.txt", "content": "hello" }))
            .expect("write, making the folder");
        assert_eq!(wrote["path"], "~/notes/today.txt");
        assert_eq!(fs::read_to_string(fx.at("notes/today.txt")).unwrap(), "hello");

        let top = fx.call("readDirectory", json!({ "path": "~" })).expect("list ~");
        assert_eq!(top["path"], "~");
        assert_eq!(top["entries"][0]["path"], "~/notes");
        assert_eq!(top["entries"][0]["isDirectory"], true);

        // A relative path starts at ~ as well.
        let notes = fx.call("readDirectory", json!({ "path": "notes" })).expect("list notes");
        assert_eq!(notes["entries"][0]["path"], "~/notes/today.txt");
        let read = fx.call("readFile", json!({ "path": "notes/today.txt" })).expect("read");
        assert_eq!(read["content"], "hello");
    }

    #[test]
    fn absolute_paths_and_dotdot_are_refused() {
        let fx = Fixture::home();
        assert!(fx.err("readFile", json!({ "path": "/etc/hostname" })).contains("outside"));
        assert!(fx.err("readFile", json!({ "path": "~/../x" })).contains(".."));
        assert!(fx.err("writeFile", json!({ "path": "a/../../x", "content": "" })).contains(".."));
        assert!(fx.err("readFile", json!({ "path": "~root/.bashrc" })).contains("another user"));
        assert!(fx.err("readFile", json!({ "path": "~/a\u{0}b" })).contains("NUL"));
    }

    #[test]
    fn symlinks_that_leave_are_refused_and_ones_that_stay_are_followed() {
        let fx = Fixture::home();
        fs::create_dir(fx.at("notes")).unwrap();
        fs::write(fx.at("notes/a.txt"), "inside").unwrap();
        symlink("/etc", fx.at("out")).unwrap();
        symlink("../..", fx.at("notes/up")).unwrap();
        symlink("notes", fx.at("inside")).unwrap();

        assert!(fx.err("readFile", json!({ "path": "~/out/hostname" })).contains("leads outside"));
        assert!(fx.err("readDirectory", json!({ "path": "~/notes/up" })).contains("leads outside"));
        let read = fx.call("readFile", json!({ "path": "~/inside/a.txt" })).expect("follow");
        assert_eq!(read["content"], "inside");
    }

    #[test]
    fn nothing_writes_through_a_symlink_that_leaves() {
        let fx = Fixture::home();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim");
        fs::write(&victim, "keep").unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&victim, fx.at("victim")).unwrap();

        fx.err("writeFile", json!({ "path": "~/victim", "content": "gone" }));
        fx.err("appendFile", json!({ "path": "~/victim", "content": "gone" }));
        fx.err("chmod", json!({ "path": "~/victim", "mode": 0o777 }));
        fx.err("copy", json!({ "source": "~/victim", "destination": "~/stolen" }));
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(fs::metadata(&victim).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!fx.at("stolen").exists());

        // Deleting the link removes the link, never the file.
        fx.call("deleteFile", json!({ "path": "~/victim" })).expect("delete the link");
        assert!(victim.exists());
    }

    #[test]
    fn a_recursive_delete_removes_links_not_what_they_point_at() {
        let fx = Fixture::home();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("precious"), "keep").unwrap();
        fs::create_dir_all(fx.at("d/sub")).unwrap();
        fs::write(fx.at("d/sub/file"), "x").unwrap();
        symlink(outside.path(), fx.at("d/link")).unwrap();

        fx.call("deleteDirectory", json!({ "path": "~/d", "recursive": true })).expect("delete");
        assert!(!fx.at("d").exists());
        assert!(outside.path().join("precious").exists());
    }

    #[test]
    fn copy_keeps_symlinks_as_links_and_copies_a_folder_into_itself_once() {
        let fx = Fixture::home();
        fs::create_dir(fx.at("src")).unwrap();
        fs::write(fx.at("src/a.txt"), "a").unwrap();
        symlink("/etc/hostname", fx.at("src/link")).unwrap();

        fx.call("copy", json!({ "source": "~/src", "destination": "~/src/copy" })).expect("copy");
        assert_eq!(fs::read_to_string(fx.at("src/copy/a.txt")).unwrap(), "a");
        assert_eq!(fs::read_link(fx.at("src/copy/link")).unwrap(), Path::new("/etc/hostname"));
        assert!(!fx.at("src/copy/copy").exists());

        let same = fx.err("copy", json!({ "source": "~/src/a.txt", "destination": "~/src/a.txt" }));
        assert!(same.contains("same file"), "{same}");
        assert_eq!(fs::read_to_string(fx.at("src/a.txt")).unwrap(), "a");
    }

    #[test]
    fn the_top_of_the_jail_stays_put() {
        let fx = Fixture::home();
        fx.err("deleteDirectory", json!({ "path": "~", "recursive": true }));
        fx.err("move", json!({ "source": "~", "destination": "~/elsewhere" }));
        fx.err("chmod", json!({ "path": "~", "mode": 0 }));
        assert!(fx.dir.path().is_dir());
    }

    #[test]
    fn chmod_keeps_only_the_permission_bits() {
        let fx = Fixture::home();
        fx.call("writeFile", json!({ "path": "~/x", "content": "" })).unwrap();
        let res = fx.call("chmod", json!({ "path": "~/x", "mode": 0o4755 })).expect("chmod");
        assert_eq!(res["mode"], "755");
        assert_eq!(fs::metadata(fx.at("x")).unwrap().permissions().mode() & 0o7777, 0o755);
    }

    #[test]
    fn move_create_and_exists_work_inside() {
        let fx = Fixture::home();
        fx.call("createDirectory", json!({ "path": "~/a/b", "mode": 0o700 })).expect("mkdir -p");
        assert_eq!(fs::metadata(fx.at("a/b")).unwrap().permissions().mode() & 0o777, 0o700);
        fx.err("createDirectory", json!({ "path": "~/a/b" }));
        fx.call("move", json!({ "source": "~/a/b", "destination": "~/c" })).expect("move");
        assert_eq!(fx.call("exists", json!({ "path": "~/c" })).unwrap()["exists"], true);
        assert_eq!(fx.call("exists", json!({ "path": "~/a/b" })).unwrap()["exists"], false);

        symlink("/etc", fx.at("out")).unwrap();
        assert!(fx.err("exists", json!({ "path": "~/out/hostname" })).contains("leads outside"));
    }

    #[test]
    fn the_system_scope_is_absolute_and_read_only() {
        let fx = Fixture::new(false, true);
        fs::create_dir(fx.at("etc")).unwrap();
        fs::write(fx.at("etc/hostname"), "pyxis").unwrap();

        let top = fx.call("readDirectory", json!({ "path": "/" })).expect("list /");
        assert_eq!(top["entries"][0]["path"], "/etc");
        let read = fx.call("readFile", json!({ "path": "/etc/hostname" })).expect("read");
        assert_eq!(read["content"], "pyxis");
        assert_eq!(read["path"], "/etc/hostname");

        for method in WRITES {
            let err = fx.err(method, json!({ "path": "/etc/hostname", "source": "/etc/hostname", "destination": "/etc/x" }));
            assert!(err.contains("read-only"), "{method}: {err}");
        }
        assert!(fx.err("readFile", json!({ "path": "~/x" })).contains("absolute"));
        assert_eq!(fs::read_to_string(fx.at("etc/hostname")).unwrap(), "pyxis");
    }

    #[test]
    fn a_fifo_is_refused_instead_of_waited_on() {
        let fx = Fixture::home();
        let path = CString::new(fx.at("pipe").into_os_string().into_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(fx.err("readFile", json!({ "path": "~/pipe" })).contains("not a file"));
        assert!(fx.err("appendFile", json!({ "path": "~/pipe", "content": "x" })).contains("pipe"));
    }

    #[test]
    fn scope_names_and_app_ids_parse_strictly() {
        assert_eq!(Scope::parse("app:notes").unwrap(), Scope::App("notes".into()));
        assert_eq!(
            Scope::parse("app:com.pyxis.hello").unwrap(),
            Scope::App("com.pyxis.hello".into())
        );
        assert_eq!(Scope::parse("user").unwrap(), Scope::User);
        assert_eq!(Scope::parse("system").unwrap(), Scope::System);
        for bad in ["app:", "app:..", "app:.", "app:a/b", "app:Notes", "app:-x", "app:a..b", "root", ""] {
            assert!(Scope::parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    /// Every file and directory under `dir`, never following a symlink, with
    /// its kind, mode and contents: what must not change outside the jail.
    fn snapshot(dir: &Path) -> Vec<(PathBuf, String, u32, Vec<u8>)> {
        let mut out = Vec::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for entry in fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                let rel = path.strip_prefix(dir).unwrap().to_path_buf();
                let mode = meta.permissions().mode();
                if meta.is_dir() {
                    todo.push(path);
                    out.push((rel, "dir".into(), mode, Vec::new()));
                } else if meta.is_symlink() {
                    out.push((rel, "link".into(), mode, fs::read_link(&path).unwrap().into_os_string().into_encoded_bytes()));
                } else {
                    out.push((rel, "file".into(), mode, fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    /// Thousands of calls, every method, on paths made at random from plain
    /// names, `..`, `.`, a link in, links out (absolute and relative), a
    /// link loop and a FIFO, with `~`, absolute or bare starts. None may
    /// change anything outside the jail, or answer with anything from there.
    #[test]
    fn random_calls_never_leave_the_jail() {
        const SECRET: &str = "SECRET-outside-the-jail";
        let fx = Fixture::home();
        // A sibling of the jail, so a relative link can climb into it.
        let outside = tempfile::tempdir().expect("outside dir");
        fs::write(outside.path().join("secret.txt"), SECRET).unwrap();
        fs::create_dir(outside.path().join("sub")).unwrap();
        fs::write(outside.path().join("sub/secret.txt"), SECRET).unwrap();
        let before = snapshot(outside.path());

        fs::create_dir_all(fx.at("a/b")).unwrap();
        fs::write(fx.at("a/f"), "inside").unwrap();
        symlink("a", fx.at("in")).unwrap();
        symlink(outside.path(), fx.at("abs")).unwrap();
        symlink(Path::new("..").join(outside.path().file_name().unwrap()), fx.at("out")).unwrap();
        symlink("loop", fx.at("loop")).unwrap();
        let fifo = CString::new(fx.at("fifo").into_os_string().into_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

        // xorshift64: the same walk every run, so a failure reproduces.
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        const NAMES: &[&str] = &[
            "a", "b", "f", "in", "out", "abs", "loop", "fifo", "..", ".", "new", "secret.txt", "sub",
        ];
        const STARTS: &[&str] = &["~/", "~/", "~/", "", "/", "~"];
        let mut path = |next: &mut dyn FnMut() -> u64| {
            let mut p = STARTS[(next() % STARTS.len() as u64) as usize].to_string();
            let parts = 1 + next() % 4;
            let names: Vec<&str> = (0..parts)
                .map(|_| NAMES[(next() % NAMES.len() as u64) as usize])
                .collect();
            if p == "~" {
                p.push('/');
            }
            p + &names.join("/")
        };

        let mut answered = 0;
        for i in 0..3000 {
            let methods = crate::services::filesystem::METHODS;
            let method = methods[(next() % methods.len() as u64) as usize];
            let body = json!({
                "path": path(&mut next),
                "source": path(&mut next),
                "destination": path(&mut next),
                "content": format!("written-{i}"),
                "mode": 0o4777,
                "recursive": true,
            });
            if let Ok(answer) = fx.jail.call(&fx.root, method, &body) {
                answered += 1;
                assert!(!answer.to_string().contains(SECRET), "{method} {body} answered {answer}");
            }
        }
        assert!(answered > 100, "only {answered} calls succeeded: the walk barely ran");
        assert_eq!(snapshot(outside.path()), before, "something outside the jail changed");
    }
}
