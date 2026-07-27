//! pyxis-appctl — the device-side launcher/registry control CLI the SDK/PDK
//! packaging spec's maintainer scripts call (`vroom/docs/pyxis-sdk-pdk-spec.md`
//! §9.3/§17.2, Phase 3 of §22).
//!
//! Deliberately NOT a new daemon: the spec's "launcher registry daemon" role
//! is already owned by Nova (in-process in echo-daemon, registry contract in
//! `echo/src/services/nova.rs`), per the project's one-bus/one-registry
//! direction. This binary is a thin Echo client over the existing
//! `org.geckoOS.nova` interface — `rescan` maps to `nova.Reload`, `launch`
//! to `nova.Launch`, and so on.
//!
//! Exit codes matter here: package `postinst`/`prerm` scripts invoke this
//! with `|| true`, so a missing/unreachable daemon must fail fast with a
//! clear message on stderr and a nonzero status, never hang.

use clap::{Parser, Subcommand};
use geckoOS_echo::EchoClient;
use serde_json::json;
use std::error::Error;

const NOVA: &str = "org.geckoOS.nova";

#[derive(Parser)]
#[command(
    name = "pyxis-appctl",
    about = "Pyxis app registry/launcher control (talks to Nova over Echo)",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Rescan all app roots and rebuild the registry (nova.Reload)
    Rescan,
    /// List registered apps
    List,
    /// Show one app's full registry entry
    Info {
        /// App id (reverse-DNS)
        app_id: String,
    },
    /// Launch an app (single-instance aware; raises if already running)
    Launch {
        app_id: String,
        /// Optional launch params as JSON
        #[arg(long, default_value = "{}")]
        params: String,
    },
    /// Close a running app
    Close { app_id: String },
    /// Raise + focus an already-running app's window
    Focus { app_id: String },
    /// Uninstall a Nova-managed app (ipk apps: use `opkg remove` instead)
    Uninstall {
        app_id: String,
        /// Also purge the app's profile/data
        #[arg(long)]
        purge: bool,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("pyxis-appctl: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    let client = EchoClient::connect()
        .await
        .map_err(|e| format!("cannot reach echo-daemon: {e}"))?;

    match cli.command {
        Command::Rescan => {
            let result = client.call_method(NOVA, NOVA, "Reload", json!({})).await?;
            println!(
                "registry reloaded: {} apps",
                result["count"].as_u64().unwrap_or(0)
            );
        }
        Command::List => {
            let result = client.call_method(NOVA, NOVA, "List", json!({})).await?;
            let apps = result["apps"].as_array().cloned().unwrap_or_default();
            if apps.is_empty() {
                println!("(no apps registered)");
                return Ok(());
            }
            for app in &apps {
                println!(
                    "{:<36} {:<10} {:<7} {}",
                    app["id"].as_str().unwrap_or("?"),
                    app["version"].as_str().unwrap_or("?"),
                    app["type"].as_str().unwrap_or("?"),
                    app["name"].as_str().unwrap_or("?"),
                );
            }
        }
        Command::Info { app_id } => {
            let result = client
                .call_method(NOVA, NOVA, "GetInfo", json!({ "appId": app_id }))
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::Launch { app_id, params } => {
            let params: serde_json::Value = serde_json::from_str(&params)?;
            let result = client
                .call_method(
                    NOVA,
                    NOVA,
                    "Launch",
                    json!({ "appId": app_id, "params": params }),
                )
                .await?;
            if result["raised"].as_bool().unwrap_or(false) {
                println!("raised running instance (session {})", result["sessionId"]);
            } else {
                println!("launched (session {})", result["sessionId"]);
            }
        }
        Command::Close { app_id } => {
            client
                .call_method(NOVA, NOVA, "Close", json!({ "appId": app_id }))
                .await?;
            println!("closed {app_id}");
        }
        Command::Focus { app_id } => {
            client
                .call_method(NOVA, NOVA, "Focus", json!({ "appId": app_id }))
                .await?;
            println!("focused {app_id}");
        }
        Command::Uninstall { app_id, purge } => {
            client
                .call_method(
                    NOVA,
                    NOVA,
                    "Uninstall",
                    json!({ "appId": app_id, "purgeData": purge }),
                )
                .await?;
            println!("uninstalled {app_id}");
        }
    }
    Ok(())
}
