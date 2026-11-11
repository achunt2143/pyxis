//! Puppis: Gecko native-app lifecycle management for GeckoOS.
//!
//! Extracted from the old `gaia` crate (2026-07-07, per the constellation
//! rename pass alongside Vela/Shift/Rockhopper/Vroom) — `gaia` used to be a
//! full legacy desktop shell (an actix-web HTTP/WebSocket server serving a
//! static JS UI), long superseded by Bloom. This crate keeps only the one
//! piece of that old crate genuinely still in use: `gecko_app_launcher.rs`,
//! which Rockhopper's daemon uses to track native (FirefoxOS-era)
//! application lifecycle (launch/suspend/resume/terminate, resource
//! tracking). Everything else from `gaia` (the web server, static assets,
//! its own now-unused `node_modules/` JS build pipeline, and several
//! entirely orphaned source files that weren't even wired into `gaia`'s own
//! module tree) was dead weight and has been deleted rather than carried
//! forward under a new name.

//! **This crate starts no process.** `GeckoAppLauncher::launch_app` only
//! records an app (ids it allocates itself, never a pid), and nothing in
//! Rockhopper's daemon calls it (the daemon registers a service whose
//! methods have no handler). It is therefore not a launch path, and CLAUDE.md's
//! old note that an app started through it would arrive at Evo as trusted was
//! about a path that does not exist. If it ever does start something, the
//! start must go through Nova's launch (`pyxis-wlctx`, as the app user): a
//! Gecko started by root or the shell's user on Evo's host socket is the
//! system, not an app (`evo/src/trust.rs`). `launcher_spawns_nothing` keeps
//! that from happening by accident.

pub mod gecko_app_launcher;

pub use gecko_app_launcher::GeckoAppLauncher;
