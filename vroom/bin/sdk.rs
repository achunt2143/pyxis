//! `pyxis-sdk` — web application toolchain frontend (spec §13.2, §14).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use geckoOS_vroom::config::{Runtime, WebBuilder};
use geckoOS_vroom::error::PyxisError;

#[derive(Parser)]
#[command(name = "pyxis-sdk", about = "Pyxis SDK: build and package web applications", version)]
struct Cli {
    /// Project directory (defaults to the current directory)
    #[arg(short, long, global = true, default_value = ".")]
    project: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a new web app project
    New {
        /// Must be `web`
        kind: String,
        /// Directory to create
        dir: PathBuf,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        vendor: Option<String>,
    },
    /// Run the configured web builder
    Build,
    /// Stage and emit the .ipk package into dist/
    Package,
    /// Copy the package to a device and opkg-install it
    Install {
        #[arg(long)]
        device: Option<String>,
    },
    /// Serve the app locally for development
    Serve {
        #[arg(long, default_value = "8080")]
        port: u16,
    },
    /// Print the resolved project, manifest, and package plan
    Inspect,
}

fn serve(project_dir: &PathBuf, port: u16) -> Result<(), PyxisError> {
    let project = geckoOS_vroom::assert_runtime(project_dir, Runtime::Web)?;
    let builder = project.config.sdk.as_ref().map(|s| s.builder);
    match builder {
        Some(WebBuilder::Static) => {
            let dir = project.web_out_dir();
            eprintln!("[pyxis-sdk] serving {} on http://localhost:{port}", dir.display());
            let status = std::process::Command::new("python3")
                .args([
                    "-m",
                    "http.server",
                    &port.to_string(),
                    "--directory",
                    &dir.to_string_lossy(),
                ])
                .status()
                .map_err(|e| PyxisError::Build(format!("cannot run python3 http.server: {e}")))?;
            if !status.success() {
                return Err(PyxisError::Build(format!("dev server exited with {status}")));
            }
            Ok(())
        }
        // Builder projects define their own dev-server contract.
        _ => {
            eprintln!("[pyxis-sdk] delegating to `npm run dev`");
            let status = std::process::Command::new("npm")
                .args(["run", "dev"])
                .current_dir(&project.root)
                .status()
                .map_err(|e| PyxisError::Build(format!("cannot run npm: {e}")))?;
            if !status.success() {
                return Err(PyxisError::Build(format!("`npm run dev` exited with {status}")));
            }
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::New { kind, dir, id, title, vendor } => {
            if kind != "web" {
                Err(PyxisError::Config(format!(
                    "pyxis-sdk creates web projects only (got `{kind}`); use pyxis-pdk for native"
                )))
            } else {
                geckoOS_vroom::op_new(&dir, Runtime::Web, id, title, vendor)
            }
        }
        Command::Build => geckoOS_vroom::assert_runtime(&cli.project, Runtime::Web)
            .and_then(|_| geckoOS_vroom::op_build(&cli.project)),
        Command::Package => geckoOS_vroom::assert_runtime(&cli.project, Runtime::Web)
            .and_then(|_| geckoOS_vroom::op_package(&cli.project))
            .map(|_| ()),
        Command::Install { device } => geckoOS_vroom::assert_runtime(&cli.project, Runtime::Web)
            .and_then(|_| geckoOS_vroom::op_install(&cli.project, device.as_deref())),
        Command::Serve { port } => serve(&cli.project, port),
        Command::Inspect => geckoOS_vroom::assert_runtime(&cli.project, Runtime::Web)
            .and_then(|_| geckoOS_vroom::op_inspect(&cli.project)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pyxis-sdk: {e}");
            e.exit_code()
        }
    }
}
