// gecko/src/gecko.rs - vela launch composition (Approach A launcher/supervisor)
//
// REPURPOSED (2026-07-04): this file previously contained libxul-FFI
// embedding scaffolding (opaque pointers into nsXPCOMGlue/nsIServiceManager/
// nsIBaseWindow, a hand-rolled GeckoRuntimeHandle, extern "C" bindings to a
// libxul.so that was never actually built/shipped). That direction is
// explicitly superseded by pyxis-gecko-embedding-plan.md's "Approach A":
// there is no maintained desktop libxul/GeckoView embedding API anymore
// (see that doc's §0/§1), so Pyxis instead builds a customized, from-source
// Gecko binary ("vela", formerly named "pyxis-webapp" -- renamed 2026-07-07)
// and runs it as an ordinary Wayland client under Evo — no in-process
// embedding, no FFI into libxul at all.
//
// Under Approach A, this crate's job changes completely: it is the
// launcher/supervisor that composes the `vela --pyxis-app=<id>` command
// (env vars, working dir, profile dir), NOT an embedder. Per
// pyxis-gecko-embedding-plan.md §8 ("Retire or repurpose gecko/src/gecko.rs's
// libxul FFI... Confirm nothing else depends on that FFI shape before
// removing it"): confirmed via `grep -rn "geckoOS_gecko_integration\|GeckoInstance\|GeckoRuntimeHandle"`
// across the workspace (evo/, echo/, shift/, rockhopper/, puppis/) that
// nothing outside this crate's own bin targets referenced the old
// GeckoInstance/GeckoRuntimeHandle API — safe to remove outright rather than
// deprecate-and-shim.
//
// Relationship to Nova (nova-runtime-host-plan.md, not yet built): Nova owns
// manifest resolution, permission gating, single-instance policy, and
// ultimately calls `appHost.LaunchApp` over Echo — appHost
// (echo/src/services/apphost.rs) does the actual spawn+supervise, reading a
// native AppManifest {id, name, exec, args, env, ...} that already matches
// the shape this module composes (confirmed by reading AppManifest in
// apphost.rs: `exec: String`, `args: Option<Vec<String>>`,
// `env: Option<HashMap<String,String>>` — exactly what's needed here).
// So THIS module does not spawn anything itself and is not a long-running
// daemon; it is the pure launch-composition logic Nova will eventually call
// (directly, once nova/ exists and can depend on this crate) to build the
// concrete AppManifest fields for a `type: "web"` app, matching
// nova-runtime-host-plan.md §2's worked example almost verbatim. Until Nova
// exists, `gecko-shell` (see bin/gecko-shell.rs) exposes this as a
// standalone dev-time CLI so the composition logic is independently
// testable/usable without waiting on Nova's crate to be scaffolded.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where the built `vela` binary is expected to live once the
/// from-source Gecko build (see gecko/BUILD_RUNBOOK.md) is packaged, per
/// pyxis-gecko-embedding-plan.md §6 ("Bundle the private Gecko tree under
/// /usr/lib/pyxis-gecko/"). Overridable via `VELA_BIN` for
/// dev-time testing against a local `obj-pyxis-webapp/dist/bin/vela`
/// build output before packaging exists.
pub const DEFAULT_VELA_PATH: &str = "/usr/lib/pyxis-gecko/vela";

/// Root of per-app Gecko profile directories (storage/cookies/SW cache),
/// per nova-runtime-host-plan.md §4's on-disk layout
/// (`/var/lib/pyxis/profiles/<app-id>/`).
pub const DEFAULT_PROFILE_ROOT: &str = "/var/lib/pyxis/profiles";

/// Minimal fields this module needs from a web-app's `manifest.webapp`
/// (nova-runtime-host-plan.md §3). Nova owns the FULL manifest schema
/// (icons, permissions, integrity, etc.) — this is deliberately just the
/// launch-relevant subset so this crate does not need to depend on Nova's
/// (not-yet-existing) manifest types, keeping this composer usable
/// standalone today.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebAppLaunchSpec {
    /// Reverse-DNS app id; also the `pyxis://` origin host and the Wayland
    /// `app_id`/`WM_CLASS` stamp (pyxis-scheme-and-kiosk-plan.md §B1 point 3).
    pub app_id: String,
    /// Resolved as `pyxis://<app_id>/<entry>` — NOT used directly by this
    /// module (vela resolves entry from the manifest itself per
    /// §B1 point 1), but carried here so callers/tests can assert the
    /// expected navigation target without re-deriving it.
    pub entry: String,
    /// "windowed" | "maximized" | "fullscreen" (nova-runtime-host-plan.md §3).
    pub display: DisplayMode,
    /// Headless/no-window app (webOS "noWindow" equivalent). When true,
    /// vela must run the page without a Wayland toplevel at all
    /// (pyxis-scheme-and-kiosk-plan.md §B1 point 5) — this module still
    /// composes a normal launch command; the no-surface behavior is
    /// vela's own responsibility based on the manifest it reads.
    pub background: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayMode {
    Windowed,
    Maximized,
    Fullscreen,
}

impl DisplayMode {
    fn as_str(self) -> &'static str {
        match self {
            DisplayMode::Windowed => "windowed",
            DisplayMode::Maximized => "maximized",
            DisplayMode::Fullscreen => "fullscreen",
        }
    }
}

/// Composed launch command, matching AppManifest's launch-relevant fields
/// in echo/src/services/apphost.rs (`exec`, `args`, `env`) field-for-field
/// so a `nova::launcher` module (once nova/ exists) can drop this straight
/// into an `AppManifest` it writes/updates, or so `appHost.LaunchApp` can be
/// driven with an equivalent ad hoc spec via the `LaunchResolved` extension
/// point nova-runtime-host-plan.md §2 describes as a future option.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposedLaunch {
    pub exec: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// Options that vary per invocation (as opposed to per-app, which
/// `WebAppLaunchSpec` covers) — the Wayland socket to hand the child and
/// where to find the `vela` binary. Kept distinct from
/// `WebAppLaunchSpec` since these come from the *host's* runtime
/// environment (what compositor session is active right now), not from an
/// app's manifest.
#[derive(Debug, Clone)]
pub struct LaunchEnvironment {
    /// Path to the `vela` binary. Defaults to
    /// `DEFAULT_VELA_PATH`, overridable for dev-time testing against
    /// a local build output (see gecko/BUILD_RUNBOOK.md).
    pub vela_path: PathBuf,
    /// Root directory under which each app gets `<profile_root>/<app_id>/`.
    /// Defaults to `DEFAULT_PROFILE_ROOT`.
    pub profile_root: PathBuf,
    /// The Wayland compositor socket name Evo is listening on (e.g.
    /// `wayland-1`), NOT a full path — matches how `WAYLAND_DISPLAY` is
    /// conventionally set (relative to `$XDG_RUNTIME_DIR`). Required: a
    /// launch composed without a real Wayland socket is a caller bug, not a
    /// recoverable default, since every `vela` process needs Evo to
    /// already be up (Shift's boot ordering guarantees this — see
    /// nova-runtime-host-plan.md §8).
    pub wayland_display: String,
}

impl LaunchEnvironment {
    /// Build from the current process's environment, for dev-time /
    /// same-session testing (e.g. running `gecko-shell compose` from a
    /// terminal inside the already-running Evo session). A real Nova/
    /// Shift-driven launch would construct this explicitly instead of
    /// inheriting ambient env, since Nova launches apps on behalf of a
    /// specific session that may not be the caller's own.
    pub fn from_ambient_env() -> Result<Self> {
        let wayland_display = std::env::var("WAYLAND_DISPLAY")
            .map_err(|_| anyhow::anyhow!(
                "WAYLAND_DISPLAY not set — vela requires a running Evo \
                 Wayland session (MOZ_ENABLE_WAYLAND=1 client); refusing to \
                 compose a launch without one rather than silently falling back \
                 to X11/XWayland, which defeats the compositor-first design."
            ))?;
        let vela_path = std::env::var("VELA_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_VELA_PATH));
        let profile_root = std::env::var("PYXIS_PROFILE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_PROFILE_ROOT));
        Ok(LaunchEnvironment {
            vela_path,
            profile_root,
            wayland_display,
        })
    }
}

/// Compose a `vela --pyxis-app=<id>` launch spec for a web app.
///
/// This is pure/side-effect-free EXCEPT for the two validation checks noted
/// inline (binary existence, profile-dir creation) — kept as real
/// validation rather than silently producing a spec that would fail at
/// spawn time, since `appHost` (the eventual spawner) reports spawn
/// failures as `AppCrashed` with limited diagnostic context
/// (nova-runtime-host-plan.md §6 step 7); catching a missing-binary/
/// unwritable-profile-dir problem here, with a clear error, is strictly
/// more debuggable.
///
/// Env vars set on every launched vela process, per
/// pyxis-gecko-embedding-plan.md §2 and nova-runtime-host-plan.md §2's
/// worked AppManifest example:
///   MOZ_ENABLE_WAYLAND=1       - force the Wayland backend, not X11/XWayland
///   WAYLAND_DISPLAY=<socket>   - which Evo compositor session to attach to
///   PYXIS_APP_ID=<id>          - the app id; vela stamps this onto
///                                the Wayland toplevel's app_id/WM_CLASS so
///                                Evo's two-step create/claim contract
///                                (evo/src/enhancements/echo_ipc.rs) binds
///                                deterministically (pyxis-scheme-and-kiosk-
///                                plan.md §B1 point 3 — "the single most
///                                important integration detail on the
///                                kiosk side").
///   PYXIS_PROFILE_DIR=<path>   - per-app Gecko profile dir (storage
///                                isolation matching the per-origin
///                                isolation the pyxis:// protocol handler
///                                enforces).
pub fn compose_launch(
    spec: &WebAppLaunchSpec,
    env: &LaunchEnvironment,
) -> Result<ComposedLaunch> {
    if spec.app_id.is_empty() {
        bail!("WebAppLaunchSpec::app_id must not be empty");
    }
    // Reverse-DNS-ish sanity check: must be usable as both a Wayland
    // app_id/WM_CLASS token AND (via the protocol handler, elsewhere) a
    // pyxis:// URI host. Keep this check conservative and cheap; Nova's
    // real manifest validation (nova-runtime-host-plan.md §3) is the
    // authoritative gate — this is a second, narrower belt-and-suspenders
    // check specific to what THIS module emits into an argv/env, since a
    // malformed app_id here would corrupt a shell-adjacent env var or
    // confuse Evo's surface-binding correlation.
    if !spec
        .app_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        bail!(
            "WebAppLaunchSpec::app_id '{}' contains characters unsafe for a \
             Wayland app_id / pyxis:// URI host (allowed: alphanumeric, '.', \
             '-', '_')",
            spec.app_id
        );
    }

    if !env.vela_path.exists() {
        bail!(
            "vela binary not found at {:?}. Either the from-source \
             Gecko build (see gecko/BUILD_RUNBOOK.md) hasn't been built/\
             installed yet, or VELA_BIN/DEFAULT_VELA_PATH is \
             pointing at the wrong location for this environment.",
            env.vela_path
        );
    }

    let profile_dir = env.profile_root.join(&spec.app_id);
    ensure_profile_dir(&profile_dir)?;

    let mut vars = HashMap::new();
    vars.insert("MOZ_ENABLE_WAYLAND".to_string(), "1".to_string());
    vars.insert("WAYLAND_DISPLAY".to_string(), env.wayland_display.clone());
    vars.insert("PYXIS_APP_ID".to_string(), spec.app_id.clone());
    vars.insert(
        "PYXIS_PROFILE_DIR".to_string(),
        profile_dir.to_string_lossy().into_owned(),
    );
    vars.insert(
        "PYXIS_DISPLAY_MODE".to_string(),
        spec.display.as_str().to_string(),
    );
    if spec.background {
        vars.insert("PYXIS_BACKGROUND".to_string(), "1".to_string());
    }

    Ok(ComposedLaunch {
        exec: env.vela_path.to_string_lossy().into_owned(),
        // NOT "--app" (found via a real failed boot, not by inspection):
        // Gecko's own native startup code (browser/app/nsBrowserApp.cpp)
        // already recognizes a `-app <path>`/`--app <path>` flag for a
        // completely unrelated, pre-existing purpose (loading an alternate
        // application.ini), checked at argv[1] before XPCOM/JS even
        // initializes -- it intercepts this before the JS-level kiosk-mode
        // handler (BrowserContentHandler.sys.mjs) ever runs, tries to open
        // the app id as a file path, and exits with "Error: unrecognized
        // path: <app_id>". Renamed to "--pyxis-app" on both ends (this
        // module and BrowserContentHandler.sys.mjs's matching
        // handleFlagWithParam call) to stop colliding with argv[1]
        // position-sensitive native parsing.
        args: vec!["--pyxis-app".to_string(), spec.app_id.clone()],
        env: vars,
    })
}

/// Ensures `<profile_root>/<app_id>/` exists with restrictive (0700)
/// permissions, matching nova-runtime-host-plan.md §6 step 4's "profile dir
/// exists (create 0700)" requirement. Idempotent: fine to call on every
/// launch, not just first install.
fn ensure_profile_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_env(tmp: &Path) -> LaunchEnvironment {
        // Use a real, always-present binary (`/bin/true` or equivalent) as
        // the "vela path" stand-in so compose_launch's existence
        // check passes without needing an actual Gecko build in CI/tests.
        let stand_in = if Path::new("/bin/true").exists() {
            PathBuf::from("/bin/true")
        } else {
            PathBuf::from("/usr/bin/true")
        };
        LaunchEnvironment {
            vela_path: stand_in,
            profile_root: tmp.to_path_buf(),
            wayland_display: "wayland-1".to_string(),
        }
    }

    #[test]
    fn composes_expected_env_and_args() {
        let tmp = std::env::temp_dir().join(format!(
            "pyxis-gecko-test-{}",
            std::process::id()
        ));
        let spec = WebAppLaunchSpec {
            app_id: "com.pyxis.app.files".to_string(),
            entry: "index.html".to_string(),
            display: DisplayMode::Windowed,
            background: false,
        };
        let env = test_env(&tmp);
        let composed = compose_launch(&spec, &env).expect("compose_launch failed");

        assert_eq!(composed.args, vec!["--pyxis-app", "com.pyxis.app.files"]);
        assert_eq!(composed.env["MOZ_ENABLE_WAYLAND"], "1");
        assert_eq!(composed.env["WAYLAND_DISPLAY"], "wayland-1");
        assert_eq!(composed.env["PYXIS_APP_ID"], "com.pyxis.app.files");
        assert!(composed.env["PYXIS_PROFILE_DIR"].ends_with("com.pyxis.app.files"));
        assert_eq!(composed.env["PYXIS_DISPLAY_MODE"], "windowed");
        assert!(!composed.env.contains_key("PYXIS_BACKGROUND"));

        let profile_dir = tmp.join("com.pyxis.app.files");
        assert!(profile_dir.is_dir(), "profile dir should have been created");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rejects_unsafe_app_id() {
        let tmp = std::env::temp_dir().join(format!(
            "pyxis-gecko-test-unsafe-{}",
            std::process::id()
        ));
        let spec = WebAppLaunchSpec {
            app_id: "../escape".to_string(),
            entry: "index.html".to_string(),
            display: DisplayMode::Windowed,
            background: false,
        };
        let env = test_env(&tmp);
        assert!(compose_launch(&spec, &env).is_err());
    }

    #[test]
    fn background_apps_set_pyxis_background_flag() {
        let tmp = std::env::temp_dir().join(format!(
            "pyxis-gecko-test-bg-{}",
            std::process::id()
        ));
        let spec = WebAppLaunchSpec {
            app_id: "com.pyxis.app.sync".to_string(),
            entry: "worker.html".to_string(),
            display: DisplayMode::Windowed,
            background: true,
        };
        let env = test_env(&tmp);
        let composed = compose_launch(&spec, &env).expect("compose_launch failed");
        assert_eq!(composed.env["PYXIS_BACKGROUND"], "1");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
