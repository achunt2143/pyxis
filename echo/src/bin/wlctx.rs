//! pyxis-wlctx — start an app as its own user, on its own Wayland socket, so
//! Evo knows what it is.
//!
//! Evo decides what a Wayland client may do from how it connected (see
//! `evo/src/trust.rs`). Anything on Evo's own socket is the system. An app
//! must arrive through a `wp_security_context_v1` socket, which carries
//! metadata the compositor attaches to the connection itself and the client
//! cannot alter. And the app must not be able to skip that socket and dial
//! Evo's own: the host socket belongs to the shell's user and is mode 0600,
//! and the app runs as another. This helper does both, as root:
//!
//! 1. connects to Evo's host socket (root passes its permission bits);
//! 2. makes the app's socket in a per-instance directory under a root-owned
//!    parent (`/run/pyxis/wl/`), hands the listener to Evo with sandbox
//!    engine `pyxis`, the app id and an instance id, and gives the directory
//!    and socket to the app's user. The parent is root's so that nothing the
//!    app runs can swap a directory for a symlink under root's feet; the
//!    instance directory is the app's only after root is done in it;
//! 3. makes the app's runtime dir (`--runtime-dir`, owned by the app's user)
//!    and any `--link` entries in it, so what the app finds under
//!    `$XDG_RUNTIME_DIR` (Echo's socket) is the shell's, without the app's
//!    user being able to enter the shell's own runtime dir beyond that;
//! 4. optionally hides paths from the app in a mount namespace of its own
//!    (`--hide`: a bind mount of /dev/null, so the host socket is not merely
//!    refused but absent);
//! 5. forks the app, drops to its uid, gid and supplementary groups in the
//!    child, and only then arms `PR_SET_PDEATHSIG`; execs the command with
//!    `WAYLAND_DISPLAY` pointing at the app's socket; and stays as the
//!    parent until it exits, forwarding termination signals;
//! 6. removes the socket directory.
//!
//! **Why the drop is here and not in `setpriv`.** A change of credentials
//! clears the parent-death signal (`commit_creds`). The helper is what appHost
//! records as the session's pid, and a kill aimed at that pid alone must still
//! take the app with it, which is what `PR_SET_PDEATHSIG` is for. With the
//! helper running as the app's user (the old arrangement, after a `setpriv`
//! drop) the signal could be set before the exec; with the helper as root and
//! the drop in the child it must be set after the drop, and this is the only
//! place that can. The exec afterwards does not clear it: the app is not a
//! set-id binary and has no file capabilities.
//!
//! appHost signals the process group, so the app is signalled whether or not
//! the helper forwards; the helper does not call `setsid`/`setpgid` for that
//! reason. Evo stops listening when the helper's end of the close pipe goes,
//! which is when the helper dies, however it dies; the directory is swept by
//! the next launch if the helper was SIGKILLed.
//!
//! It fails closed: if the context can't be made, or the drop fails, the
//! command is not run, as running it on the host socket or as root would give
//! it the system's standing. A root helper with no `--uid` refuses for the
//! same reason. Run as an ordinary user with no `--uid` it does what it did
//! before: no drop, and the socket under `$XDG_RUNTIME_DIR/pyxis-wl/` (the
//! on-device trust suite starts probes that way).

use std::ffi::{CString, OsString};
use std::fs;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{lchown, MetadataExt, PermissionsExt};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::sync::atomic::{AtomicI32, Ordering};

use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::wl_registry,
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::wp::security_context::v1::client::{
    wp_security_context_manager_v1::WpSecurityContextManagerV1,
    wp_security_context_v1::WpSecurityContextV1,
};

/// Where per-instance socket directories live when run as root. `/run` is
/// tmpfs, so a reboot clears whatever a SIGKILL left.
const ROOT_SOCKET_BASE: &str = "/run/pyxis/wl";
/// Under `$XDG_RUNTIME_DIR`, when not root.
const USER_SOCKET_SUBDIR: &str = "pyxis-wl";
/// The sandbox engine name Evo expects of a Pyxis launch.
const ENGINE: &str = "pyxis";

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<WpSecurityContextManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &WpSecurityContextManagerV1,
        _: <WpSecurityContextManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<WpSecurityContextV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &WpSecurityContextV1,
        _: <WpSecurityContextV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[derive(Debug, PartialEq)]
struct Args {
    app_id: String,
    /// The user the app runs as. `None` leaves the helper's own.
    uid: Option<u32>,
    gid: Option<u32>,
    /// Supplementary groups: names or numbers.
    groups: Vec<String>,
    /// The app's `$XDG_RUNTIME_DIR`: made, and given to the app's user.
    runtime_dir: Option<PathBuf>,
    /// `NAME=TARGET`: a symlink NAME in the runtime dir, if nothing is there.
    links: Vec<(String, PathBuf)>,
    /// Evo's own socket. Defaults to `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY`.
    host_socket: Option<PathBuf>,
    /// Where instance directories go. Defaults per user, see the module docs.
    socket_base: Option<PathBuf>,
    /// Paths to bind /dev/null over in the app's own mount namespace.
    hide: Vec<PathBuf>,
    command: Vec<OsString>,
}

fn usage() -> ! {
    eprintln!(
        "usage: pyxis-wlctx --app-id ID [--uid N --gid N [--groups g,g]] [--runtime-dir DIR]\n\
         \x20                  [--link NAME=TARGET]... [--host-socket PATH] [--socket-base DIR]\n\
         \x20                  [--hide PATH]... -- COMMAND [ARGS...]"
    );
    exit(2);
}

fn parse_args(raw: impl IntoIterator<Item = OsString>) -> Result<Args, String> {
    let mut args = Args {
        app_id: String::new(),
        uid: None,
        gid: None,
        groups: Vec::new(),
        runtime_dir: None,
        links: Vec::new(),
        host_socket: None,
        socket_base: None,
        hide: Vec::new(),
        command: Vec::new(),
    };
    let mut it = raw.into_iter();
    while let Some(arg) = it.next() {
        let flag = arg.to_str().ok_or("a non-UTF-8 option")?.to_string();
        if flag == "--" {
            args.command.extend(it.by_ref());
            break;
        }
        let mut value = |what: &str| -> Result<OsString, String> {
            it.next().ok_or_else(|| format!("{what} needs a value"))
        };
        let text = |v: OsString| v.into_string().map_err(|_| "a non-UTF-8 value".to_string());
        match flag.as_str() {
            "--app-id" => args.app_id = text(value(&flag)?)?,
            "--uid" => args.uid = Some(text(value(&flag)?)?.parse().map_err(|_| "--uid is a number")?),
            "--gid" => args.gid = Some(text(value(&flag)?)?.parse().map_err(|_| "--gid is a number")?),
            "--groups" => {
                args.groups = text(value(&flag)?)?
                    .split(',')
                    .filter(|g| !g.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            "--runtime-dir" => args.runtime_dir = Some(value(&flag)?.into()),
            "--link" => {
                let spec = text(value(&flag)?)?;
                let (name, target) = spec.split_once('=').ok_or("--link is NAME=TARGET")?;
                if name.is_empty() || name.contains('/') || name == "." || name == ".." {
                    return Err(format!("--link name {name:?} is not a plain name"));
                }
                args.links.push((name.to_string(), PathBuf::from(target)));
            }
            "--host-socket" => args.host_socket = Some(value(&flag)?.into()),
            "--socket-base" => args.socket_base = Some(value(&flag)?.into()),
            "--hide" => args.hide.push(value(&flag)?.into()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    if args.app_id.is_empty() {
        return Err("--app-id is required".into());
    }
    if args.command.is_empty() {
        return Err("no command".into());
    }
    if args.uid.is_some() != args.gid.is_some() {
        return Err("--uid and --gid go together".into());
    }
    if (!args.groups.is_empty() || args.runtime_dir.is_some()) && args.uid.is_none() {
        return Err("--groups and --runtime-dir need --uid".into());
    }
    Ok(args)
}

/// A name safe to use in a path, and short enough that the socket path fits
/// in `sun_path` (108 bytes).
fn path_component(app_id: &str) -> String {
    app_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .take(48)
        .collect()
}

/// Remove directories left by a helper that was killed outright: those whose
/// instance pid (the suffix of the name) is gone.
fn sweep_stale(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.rsplit('-').next())
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if !Path::new(&format!("/proc/{pid}")).exists() {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// The user and group the app's files belong to, once there is one.
#[derive(Clone, Copy)]
struct Owner {
    uid: u32,
    gid: u32,
}

/// Resolve a group name (or number) to a gid.
fn resolve_group(name: &str) -> Result<u32, String> {
    if let Ok(n) = name.parse::<u32>() {
        return Ok(n);
    }
    let c = CString::new(name).map_err(|_| format!("group {name:?}"))?;
    // getgrnam is not thread-safe, and this helper has one thread when it
    // calls it.
    let gr = unsafe { libc::getgrnam(c.as_ptr()) };
    if gr.is_null() {
        return Err(format!("no group named {name:?}"));
    }
    Ok(unsafe { (*gr).gr_gid })
}

/// Make `dir` if needed and make sure it is a real directory of `owner`'s,
/// mode 0700. It never follows a symlink at `dir`. Safe to run as root only
/// where `dir`'s parent is root's (`/run/user`), which the caller checks by
/// choosing the path.
fn ensure_owned_dir(dir: &Path, owner: Owner) -> Result<(), String> {
    match fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => {
            if m.uid() != owner.uid || m.gid() != owner.gid {
                lchown(dir, Some(owner.uid), Some(owner.gid))
                    .map_err(|e| format!("chown {}: {e}", dir.display()))?;
            }
        }
        Ok(_) => return Err(format!("{} exists and is not a directory", dir.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
            lchown(dir, Some(owner.uid), Some(owner.gid))
                .map_err(|e| format!("chown {}: {e}", dir.display()))?;
        }
        Err(e) => return Err(format!("inspect {}: {e}", dir.display())),
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod {}: {e}", dir.display()))
}

/// Add `NAME` -> `TARGET` links to the runtime dir where nothing is called
/// NAME. An existing entry is left alone: root writes only what is not
/// there, so nothing the app planted is followed or replaced. (A link of a
/// previous run is still right: the targets are fixed.)
fn ensure_links(runtime_dir: &Path, links: &[(String, PathBuf)]) -> Result<(), String> {
    for (name, target) in links {
        let at = runtime_dir.join(name);
        match fs::symlink_metadata(&at) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::os::unix::fs::symlink(target, &at)
                    .map_err(|e| format!("link {} -> {}: {e}", at.display(), target.display()))?;
            }
            Err(e) => return Err(format!("inspect {}: {e}", at.display())),
        }
    }
    Ok(())
}

/// Make the socket and give it to Evo. Returns the pipe end that keeps the
/// listener alive (Evo stops listening when it closes) and the socket path.
fn make_context(
    args: &Args,
    host: &Path,
    instance: &str,
    dir: &Path,
    owner: Option<Owner>,
) -> Result<(OwnedFd, PathBuf), String> {
    fs::create_dir(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;

    let path = dir.join("wayland");
    let listener = UnixListener::bind(&path).map_err(|e| format!("bind {}: {e}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    if let Some(o) = owner {
        // The socket first, then the directory that holds it: until the
        // directory is the app's, nothing of the app's can touch what is in it.
        lchown(&path, Some(o.uid), Some(o.gid)).map_err(|e| format!("chown socket: {e}"))?;
        lchown(dir, Some(o.uid), Some(o.gid)).map_err(|e| format!("chown {}: {e}", dir.display()))?;
    }

    // Both ends close-on-exec: the app must not inherit the write end, or
    // the listener would outlive the helper.
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(format!("pipe2: {}", std::io::Error::last_os_error()));
    }
    let (read_end, write_end) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };

    let stream = UnixStream::connect(host).map_err(|e| format!("connect to Evo at {}: {e}", host.display()))?;
    let conn = Connection::from_socket(stream).map_err(|e| format!("connect to Evo: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<State>(&conn).map_err(|e| format!("registry: {e}"))?;
    let qh = queue.handle();
    let manager: WpSecurityContextManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| format!("Evo offers no wp_security_context_manager_v1 to this client: {e}"))?;

    let context = manager.create_listener(listener.as_fd(), read_end.as_fd(), &qh, ());
    context.set_sandbox_engine(ENGINE.to_string());
    context.set_app_id(args.app_id.clone());
    context.set_instance_id(instance.to_string());
    context.commit();
    // The roundtrip is what makes the socket live before the app dials it,
    // and what surfaces a protocol error as a failure here.
    queue
        .roundtrip(&mut State)
        .map_err(|e| format!("security context refused: {e}"))?;
    context.destroy();
    let _ = conn.flush();
    // Evo holds its own copy of the listener now; this one can go.
    drop(listener);
    Ok((write_end, path))
}

/// Give this process (and so the app it forks) a private mount namespace in
/// which each of `paths` is /dev/null. Best effort, and said so: the host
/// socket is already refused to the app by its owner and mode, and this makes
/// it absent too, which matters for whoever later loosens the mode.
fn hide_paths(paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }
    unsafe {
        if libc::unshare(libc::CLONE_NEWNS) != 0 {
            eprintln!("pyxis-wlctx: no mount namespace, paths stay visible: {}", std::io::Error::last_os_error());
            return;
        }
        // Mounts made below must not propagate back to the host.
        let root = CString::new("/").unwrap();
        if libc::mount(std::ptr::null(), root.as_ptr(), std::ptr::null(), libc::MS_REC | libc::MS_PRIVATE, std::ptr::null()) != 0 {
            eprintln!("pyxis-wlctx: cannot make mounts private, paths stay visible: {}", std::io::Error::last_os_error());
            return;
        }
    }
    let null = CString::new("/dev/null").unwrap();
    for path in paths {
        let Ok(target) = CString::new(path.as_os_str().as_bytes()) else { continue };
        let rc = unsafe {
            libc::mount(null.as_ptr(), target.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null())
        };
        if rc != 0 {
            eprintln!("pyxis-wlctx: cannot hide {}: {}", path.display(), std::io::Error::last_os_error());
        }
    }
}

static CHILD: AtomicI32 = AtomicI32::new(0);

extern "C" fn forward(sig: libc::c_int) {
    let child = CHILD.load(Ordering::SeqCst);
    if child > 0 {
        unsafe { libc::kill(child, sig) };
    }
}

fn main() {
    let args = match parse_args(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("pyxis-wlctx: {e}");
            usage();
        }
    };
    let is_root = unsafe { libc::geteuid() } == 0;
    if is_root && args.uid.is_none() {
        // Refuse rather than run an app as root.
        eprintln!("pyxis-wlctx: running as root needs --uid/--gid: it will not start an app as root");
        exit(70);
    }
    let owner = args.uid.zip(args.gid).map(|(uid, gid)| Owner { uid, gid });
    if let Some(o) = owner {
        if o.uid == 0 {
            eprintln!("pyxis-wlctx: --uid 0: it will not start an app as root");
            exit(70);
        }
        if !is_root && (o.uid != unsafe { libc::geteuid() } || o.gid != unsafe { libc::getegid() }) {
            eprintln!("pyxis-wlctx: only root can switch to another user");
            exit(70);
        }
    }

    // Everything that can fail does so before the app exists.
    let supplementary: Vec<libc::gid_t> = match args.groups.iter().map(|g| resolve_group(g)).collect() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("pyxis-wlctx: {e}");
            exit(70);
        }
    };
    let host = match &args.host_socket {
        Some(p) => p.clone(),
        None => match (std::env::var_os("XDG_RUNTIME_DIR"), std::env::var_os("WAYLAND_DISPLAY")) {
            (Some(d), Some(n)) => Path::new(&d).join(n),
            _ => {
                eprintln!("pyxis-wlctx: no --host-socket, and XDG_RUNTIME_DIR/WAYLAND_DISPLAY are not set");
                exit(70);
            }
        },
    };
    let base = match &args.socket_base {
        Some(b) => b.clone(),
        None if is_root => PathBuf::from(ROOT_SOCKET_BASE),
        None => match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(d) => Path::new(&d).join(USER_SOCKET_SUBDIR),
            None => {
                eprintln!("pyxis-wlctx: XDG_RUNTIME_DIR is not set");
                exit(70);
            }
        },
    };
    if let Err(e) = fs::create_dir_all(&base) {
        eprintln!("pyxis-wlctx: create {}: {e}", base.display());
        exit(70);
    }
    // 0755: apps must reach their own instance directory beneath it; they
    // have no business listing the others, but a name is not a secret and
    // the directories themselves are 0700.
    let _ = fs::set_permissions(&base, fs::Permissions::from_mode(0o755));
    sweep_stale(&base);

    if let (Some(rd), Some(o)) = (&args.runtime_dir, owner) {
        if let Err(e) = ensure_owned_dir(rd, o).and_then(|_| ensure_links(rd, &args.links)) {
            eprintln!("pyxis-wlctx: {e}");
            exit(70);
        }
    }

    let instance = format!("{}-{}", path_component(&args.app_id), std::process::id());
    let dir = base.join(&instance);
    let (keepalive, socket) = match make_context(&args, &host, &instance, &dir, owner) {
        Ok(made) => made,
        Err(e) => {
            eprintln!("pyxis-wlctx: {e}");
            let _ = fs::remove_dir_all(&dir);
            exit(70);
        }
    };

    hide_paths(&args.hide);

    let mut cmd = Command::new(&args.command[0]);
    cmd.args(&args.command[1..]);
    // An absolute path is taken as is by libwayland, whatever toolkit sits on
    // top of it.
    cmd.env("WAYLAND_DISPLAY", &socket);
    let parent = std::process::id() as libc::pid_t;
    unsafe {
        cmd.pre_exec(move || {
            // Drop first, arm the parent-death signal after: a credential
            // change clears it. Groups before gid before uid, since setgroups
            // and setgid need the privilege setuid gives up.
            if let Some(o) = owner {
                if libc::setgroups(supplementary.len(), supplementary.as_ptr()) != 0
                    || libc::setresgid(o.gid, o.gid, o.gid) != 0
                    || libc::setresuid(o.uid, o.uid, o.uid) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                // A drop that did not take would hand the app root.
                if libc::geteuid() != o.uid || libc::getuid() != o.uid {
                    return Err(std::io::Error::from_raw_os_error(libc::EPERM));
                }
                if o.uid != 0 && libc::setuid(0) == 0 {
                    return Err(std::io::Error::from_raw_os_error(libc::EPERM));
                }
            }
            // The app lives and dies with the helper. appHost records the
            // helper's pid as the session's, so a kill aimed at the session
            // must take the app with it, and an app that outlived its helper
            // would have no one to clean up after it.
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The helper may already be gone: the signal only fires on a
            // death that happens after this call.
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    let code = match cmd.spawn() {
        Err(e) => {
            eprintln!("pyxis-wlctx: run {:?}: {e}", args.command[0]);
            127
        }
        Ok(mut child) => {
            CHILD.store(child.id() as i32, Ordering::SeqCst);
            for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                unsafe { libc::signal(sig, forward as usize) };
            }
            match child.wait() {
                Ok(status) => status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
                Err(_) => 1,
            }
        }
    };
    drop(keepalive);
    let _ = fs::remove_dir_all(&dir);
    exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_a_root_launch() {
        let a = parse(&[
            "--app-id", "files", "--uid", "1001", "--gid", "1001", "--groups", "video,render",
            "--runtime-dir", "/run/user/1001", "--link", "geckoOS=/run/user/1000/geckoOS",
            "--hide", "/run/user/1000/wayland-0", "--", "vela", "--profile", "/p",
        ])
        .unwrap();
        assert_eq!(a.uid, Some(1001));
        assert_eq!(a.groups, ["video", "render"]);
        assert_eq!(a.links, [("geckoOS".to_string(), PathBuf::from("/run/user/1000/geckoOS"))]);
        assert_eq!(a.hide, [PathBuf::from("/run/user/1000/wayland-0")]);
        assert_eq!(a.command, ["vela", "--profile", "/p"].map(OsString::from));
    }

    #[test]
    fn a_drop_needs_both_ids_and_the_options_that_follow_it_need_a_drop() {
        assert!(parse(&["--app-id", "a", "--uid", "1001", "--", "x"]).is_err());
        assert!(parse(&["--app-id", "a", "--groups", "video", "--", "x"]).is_err());
        assert!(parse(&["--app-id", "a", "--runtime-dir", "/r", "--", "x"]).is_err());
        assert!(parse(&["--app-id", "a", "--", "x"]).is_ok());
    }

    #[test]
    fn needs_an_app_id_and_a_command() {
        assert!(parse(&["--", "x"]).is_err());
        assert!(parse(&["--app-id", "a"]).is_err());
        assert!(parse(&["--app-id", "a", "--bogus", "--", "x"]).is_err());
    }

    #[test]
    fn a_link_name_is_one_path_component() {
        assert!(parse(&["--app-id", "a", "--uid", "1", "--gid", "1", "--link", "../x=/y", "--", "c"]).is_err());
        assert!(parse(&["--app-id", "a", "--uid", "1", "--gid", "1", "--link", "x", "--", "c"]).is_err());
    }

    #[test]
    fn app_ids_become_path_components() {
        assert_eq!(path_component("com.pyxis/../x y"), "com.pyxis_.._x_y");
        assert!(path_component(&"a".repeat(200)).len() <= 48);
    }

    #[test]
    fn links_are_added_only_where_nothing_is() {
        let tmp = tempfile::tempdir().unwrap();
        let planted = tmp.path().join("planted");
        std::os::unix::fs::symlink("/etc", &planted).unwrap();
        ensure_links(
            tmp.path(),
            &[
                ("geckoOS".to_string(), PathBuf::from("/run/user/1000/geckoOS")),
                ("planted".to_string(), PathBuf::from("/run/user/1000/other")),
            ],
        )
        .unwrap();
        assert_eq!(fs::read_link(tmp.path().join("geckoOS")).unwrap(), Path::new("/run/user/1000/geckoOS"));
        assert_eq!(fs::read_link(&planted).unwrap(), Path::new("/etc"), "an existing entry is left alone");
        // And a second run changes nothing.
        ensure_links(tmp.path(), &[("geckoOS".to_string(), PathBuf::from("/run/user/1000/geckoOS"))]).unwrap();
    }

    #[test]
    fn a_runtime_dir_is_never_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let me = Owner { uid: unsafe { libc::geteuid() }, gid: unsafe { libc::getegid() } };
        assert!(ensure_owned_dir(&link, me).is_err());
        assert!(ensure_owned_dir(&real, me).is_ok());
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn groups_resolve_by_number() {
        assert_eq!(resolve_group("44"), Ok(44));
        assert!(resolve_group("no-such-group-here").is_err());
    }
}
