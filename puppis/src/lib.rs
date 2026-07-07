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

pub mod gecko_app_launcher;

pub use gecko_app_launcher::GeckoAppLauncher;
