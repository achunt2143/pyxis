//! `pyxis` — umbrella CLI (spec §13.1). Dispatches on `pyxis.json.runtime`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use geckoOS_vroom::config::Runtime;

#[derive(Parser)]
#[command(name = "pyxis", about = "Pyxis application toolchain (SDK + PDK)", version)]
struct Cli {
    /// Project directory (defaults to the current directory)
    #[arg(short, long, global = true, default_value = ".")]
    project: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a new app project
    New {
        /// Runtime: web or native
        #[arg(value_enum)]
        runtime: RuntimeArg,
        /// Directory to create
        dir: PathBuf,
        /// Reverse-DNS app id (default derived from the directory name)
        #[arg(long)]
        id: Option<String>,
        /// Display title
        #[arg(long)]
        title: Option<String>,
        /// Vendor/publisher
        #[arg(long)]
        vendor: Option<String>,
    },
    /// Build the app payload (dispatches on pyxis.json runtime)
    Build,
    /// Stage and emit the .ipk package into dist/
    Package,
    /// Copy the package to a device and opkg-install it
    Install {
        /// Device profile name from ~/.config/pyxis/devices.json
        #[arg(long)]
        device: Option<String>,
    },
    /// Install, then launch on the device
    Run {
        #[arg(long)]
        device: Option<String>,
    },
    /// Check host toolchain and project health
    Doctor,
    /// Validate pyxis.json and the composed app manifest
    Lint,
    /// Launch the Pyxis emulator (requires PYXIS_EMULATOR_CMD)
    Emulator,
    /// Sign an .ipk with a detached signature (<file>.sig)
    Sign {
        /// Package to sign
        file: PathBuf,
        /// PEM private key
        #[arg(long)]
        key: PathBuf,
        /// Key id written to <file>.sig.keyid (devices check /etc/pyxis/keys/<id>.pub)
        #[arg(long = "key-id")]
        key_id: Option<String>,
    },
    /// Generate a signing key pair: <id>.pem (private) and <id>.pub (public)
    Keygen {
        /// Key id, e.g. `pyxis-dev-2026`
        id: String,
        /// Directory to write the keys into (default: current directory)
        #[arg(long, default_value = ".")]
        out_dir: PathBuf,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum RuntimeArg {
    Web,
    Native,
}

impl From<RuntimeArg> for Runtime {
    fn from(r: RuntimeArg) -> Self {
        match r {
            RuntimeArg::Web => Runtime::Web,
            RuntimeArg::Native => Runtime::Native,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::New { runtime, dir, id, title, vendor } => {
            geckoOS_vroom::op_new(&dir, runtime.into(), id, title, vendor)
        }
        Command::Build => geckoOS_vroom::op_build(&cli.project),
        Command::Package => geckoOS_vroom::op_package(&cli.project).map(|_| ()),
        Command::Install { device } => geckoOS_vroom::op_install(&cli.project, device.as_deref()),
        Command::Run { device } => geckoOS_vroom::op_run(&cli.project, device.as_deref()),
        Command::Doctor => geckoOS_vroom::op_doctor(&cli.project),
        Command::Lint => geckoOS_vroom::op_lint(&cli.project),
        Command::Sign { file, key, key_id } => {
            geckoOS_vroom::op_sign(&file, &key, key_id.as_deref()).map(|_| ())
        }
        Command::Keygen { id, out_dir } => {
            geckoOS_vroom::op_keygen(&id, &out_dir).map(|_| ())
        }
        Command::Emulator => match std::env::var("PYXIS_EMULATOR_CMD") {
            Ok(cmd) => {
                let status = std::process::Command::new("sh").args(["-c", &cmd]).status();
                match status {
                    Ok(s) if s.success() => Ok(()),
                    Ok(s) => Err(geckoOS_vroom::error::PyxisError::Device(format!(
                        "emulator exited with {s}"
                    ))),
                    Err(e) => Err(geckoOS_vroom::error::PyxisError::Device(format!(
                        "cannot run emulator: {e}"
                    ))),
                }
            }
            Err(_) => Err(geckoOS_vroom::error::PyxisError::Device(
                "no emulator configured; set PYXIS_EMULATOR_CMD (e.g. to your scripts/emulate.sh invocation)"
                    .into(),
            )),
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pyxis: {e}");
            e.exit_code()
        }
    }
}
