// gecko/bin/gecko-shell.rs
//
// Dev-time CLI for gecko::compose_launch (see gecko/src/gecko.rs's doc
// comment for the full "why this crate no longer embeds libxul" rationale).
//
// REPURPOSED (2026-07-04): previously a near-total stub daemon ("logs ready
// and idles; all real logic commented out", per this session's brief) that
// pretended to initialize a "Gecko Window Manager" against the old
// libxul-FFI GeckoInstance API. That API no longer exists (see gecko.rs).
// This binary's new job: let a developer exercise the vela
// launch-composition logic from a terminal, standing in for what Nova will
// eventually do internally once nova/ is built
// (nova-runtime-host-plan.md). It does NOT spawn vela itself in
// this pass — Nova/appHost own actual process spawning
// (echo/src/services/apphost.rs already does real spawn+supervise+reap);
// this tool only prints/validates the composed command so the composition
// logic is checkable standalone before Nova exists to call it for real.
//
// Usage:
//   gecko-shell compose --app-id com.pyxis.app.files --entry index.html
//   gecko-shell compose --app-id com.pyxis.app.files --entry index.html --exec-only
//
// Requires WAYLAND_DISPLAY to be set (see LaunchEnvironment::from_ambient_env
// in gecko.rs) — run this from inside an active Evo/Wayland session, or set
// WAYLAND_DISPLAY manually for a dry run against a not-yet-running
// compositor (compose_launch does not verify the socket is live, only that
// the env var is set).

use anyhow::{Context, Result};
use geckoOS_gecko_integration::{compose_launch, DisplayMode, LaunchEnvironment, WebAppLaunchSpec};
use tracing::info;

struct Args {
    app_id: String,
    entry: String,
    display: DisplayMode,
    background: bool,
    exec_only: bool,
}

fn parse_args() -> Result<Args> {
    let mut app_id = None;
    let mut entry = "index.html".to_string();
    let mut display = DisplayMode::Windowed;
    let mut background = false;
    let mut exec_only = false;

    let mut argv = std::env::args().skip(1); // skip argv[0]
    let subcommand = argv.next();
    if subcommand.as_deref() != Some("compose") {
        anyhow::bail!(
            "usage: gecko-shell compose --app-id <id> [--entry <path>] \
             [--display windowed|maximized|fullscreen] [--background] [--exec-only]"
        );
    }

    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--app-id" => {
                app_id = Some(argv.next().context("--app-id needs a value")?);
            }
            "--entry" => {
                entry = argv.next().context("--entry needs a value")?;
            }
            "--display" => {
                let v = argv.next().context("--display needs a value")?;
                display = match v.as_str() {
                    "windowed" => DisplayMode::Windowed,
                    "maximized" => DisplayMode::Maximized,
                    "fullscreen" => DisplayMode::Fullscreen,
                    other => anyhow::bail!("unknown --display value: {other}"),
                };
            }
            "--background" => background = true,
            "--exec-only" => exec_only = true,
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }

    Ok(Args {
        app_id: app_id.context("--app-id is required")?,
        entry,
        display,
        background,
        exec_only,
    })
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args = parse_args()?;

    let spec = WebAppLaunchSpec {
        app_id: args.app_id,
        entry: args.entry,
        display: args.display,
        background: args.background,
    };

    let env = LaunchEnvironment::from_ambient_env()
        .context("failed to build LaunchEnvironment from ambient environment")?;

    info!("Composing vela launch for app_id={}", spec.app_id);
    let composed = compose_launch(&spec, &env)?;

    if args.exec_only {
        // Print a copy-pasteable shell command, e.g. for manually running
        // the composed launch under `strace`/`gdb` during Phase 0/1
        // bring-up (pyxis-gecko-embedding-plan.md §7's build-spike phase).
        let env_prefix: Vec<String> = composed
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        println!(
            "{} {} {}",
            env_prefix.join(" "),
            composed.exec,
            composed.args.join(" ")
        );
    } else {
        println!("{}", serde_json::to_string_pretty(&composed)?);
    }

    Ok(())
}
