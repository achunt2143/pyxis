//! `pyxis-pdk` — native application toolchain frontend (spec §13.2, §15).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use geckoOS_vroom::builder::locate_native_binary;
use geckoOS_vroom::config::Runtime;
use geckoOS_vroom::error::PyxisError;

#[derive(Parser)]
#[command(name = "pyxis-pdk", about = "Pyxis PDK: build and package native applications", version)]
struct Cli {
    /// Project directory (defaults to the current directory)
    #[arg(short, long, global = true, default_value = ".")]
    project: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a new native app project
    New {
        /// Must be `native`
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
    /// Run the configured native builder
    Build,
    /// Stage and emit the .ipk package into dist/
    Package,
    /// Copy the package to a device and opkg-install it
    Install {
        #[arg(long)]
        device: Option<String>,
    },
    /// Run the built binary under gdb
    Gdb {
        /// Arguments passed through to the app
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// List symbols of the built binary
    Symbols,
}

fn gdb(project_dir: &PathBuf, args: &[String]) -> Result<(), PyxisError> {
    let project = geckoOS_vroom::assert_runtime(project_dir, Runtime::Native)?;
    let binary = locate_native_binary(&project)?;
    let mut cmd = std::process::Command::new("gdb");
    cmd.arg("--args").arg(&binary).args(args);
    let status = cmd
        .status()
        .map_err(|e| PyxisError::Build(format!("cannot run gdb: {e}")))?;
    if !status.success() {
        return Err(PyxisError::Build(format!("gdb exited with {status}")));
    }
    Ok(())
}

fn symbols(project_dir: &PathBuf) -> Result<(), PyxisError> {
    let project = geckoOS_vroom::assert_runtime(project_dir, Runtime::Native)?;
    let binary = locate_native_binary(&project)?;
    let status = std::process::Command::new("nm")
        .args(["-C", "--defined-only"])
        .arg(&binary)
        .status()
        .map_err(|e| PyxisError::Build(format!("cannot run nm: {e}")))?;
    if !status.success() {
        return Err(PyxisError::Build(format!("nm exited with {status}")));
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::New { kind, dir, id, title, vendor } => {
            if kind != "native" {
                Err(PyxisError::Config(format!(
                    "pyxis-pdk creates native projects only (got `{kind}`); use pyxis-sdk for web"
                )))
            } else {
                geckoOS_vroom::op_new(&dir, Runtime::Native, id, title, vendor)
            }
        }
        Command::Build => geckoOS_vroom::assert_runtime(&cli.project, Runtime::Native)
            .and_then(|_| geckoOS_vroom::op_build(&cli.project)),
        Command::Package => geckoOS_vroom::assert_runtime(&cli.project, Runtime::Native)
            .and_then(|_| geckoOS_vroom::op_package(&cli.project))
            .map(|_| ()),
        Command::Install { device } => geckoOS_vroom::assert_runtime(&cli.project, Runtime::Native)
            .and_then(|_| geckoOS_vroom::op_install(&cli.project, device.as_deref())),
        Command::Gdb { args } => gdb(&cli.project, &args),
        Command::Symbols => symbols(&cli.project),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pyxis-pdk: {e}");
            e.exit_code()
        }
    }
}
