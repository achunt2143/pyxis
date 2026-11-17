//! Vroom — the Pyxis SDK/PDK packaging toolchain.
//!
//! One app model, one manifest model, one package model, one installation
//! story across the web (SDK) and native (PDK) runtimes. See
//! docs/pyxis-sdk-pdk-spec.md for the full specification; the shared
//! pipeline here backs all three CLIs (`pyxis`, `pyxis-sdk`, `pyxis-pdk`).

pub mod builder;
pub mod config;
pub mod control;
pub mod device;
pub mod error;
pub mod ipk;
pub mod manifest;
pub mod signing;
pub mod stage;
pub mod template;

use std::path::{Path, PathBuf};

use config::{Project, Runtime};
use error::{PyxisError, Result};

/// `pyxis build`
pub fn op_build(project_dir: &Path) -> Result<()> {
    let project = Project::load(project_dir)?;
    builder::build(&project)?;
    eprintln!("[pyxis] build OK ({})", project.config.runtime);
    Ok(())
}

/// `pyxis package` — unified pipeline stages 1–9 (spec §16.1), minus the
/// payload build which is `pyxis build`'s job.
pub fn op_package(project_dir: &Path) -> Result<PathBuf> {
    let project = Project::load(project_dir)?;
    let appinfo = stage::stage(&project)?;
    let artifact = ipk::build_ipk(&project)?;
    // Optional detached signature (spec §16.3). An unsigned build clears any
    // sidecar left by an earlier signed build of the same name.
    match signing::resolve(&project)? {
        Some(s) => {
            signing::sign_package(&artifact, &s.key, s.key_id.as_deref())?;
            eprintln!("[pyxis] signed {} with {}", artifact.display(), s.key.display());
        }
        None => signing::remove_sidecars(&artifact),
    }
    // Artifact metadata (spec §16.2)
    let meta = serde_json::json!({
        "id": project.config.id,
        "version": project.config.version,
        "runtime": project.config.runtime,
        "architecture": project.config.package.architecture,
        "appRoot": project.app_root(),
        "entrypoint": appinfo.main,
        "packageFile": artifact.to_string_lossy(),
    });
    std::fs::write(
        project.work_dir().join("artifact.json"),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .map_err(|e| PyxisError::Package(format!("writing artifact metadata: {e}")))?;
    eprintln!("[pyxis] created {}", artifact.display());
    Ok(artifact)
}

/// `pyxis sign <file.ipk> --key <private.pem> [--key-id <id>]`
pub fn op_sign(package: &Path, key: &Path, key_id: Option<&str>) -> Result<PathBuf> {
    let sig = signing::sign_package(package, key, key_id)?;
    eprintln!("[pyxis] wrote {}", sig.display());
    if key_id.is_some() {
        eprintln!("[pyxis] wrote {}", signing::keyid_path(package).display());
    }
    Ok(sig)
}

/// `pyxis keygen <id>`: a P-256 key pair in `dir`, with where the public key goes.
pub fn op_keygen(id: &str, dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let (pem, pubkey) = signing::keygen(id, dir)?;
    eprintln!("[pyxis] private key: {}  (keep it secret, keep it out of git)", pem.display());
    eprintln!("[pyxis] public key:  {}", pubkey.display());
    eprintln!(
        "[pyxis] copy the public key to each device as /etc/pyxis/keys/{id}.pub \
         (the device refuses unsigned packages unless /etc/pyxis/allow-unsigned-packages exists)"
    );
    Ok((pem, pubkey))
}

/// `pyxis install [--device <name>]` — packages if needed, then runs the
/// device install sequence (spec §17.2).
pub fn op_install(project_dir: &Path, device_name: Option<&str>) -> Result<()> {
    let project = Project::load(project_dir)?;
    let artifact = project.dist_dir().join(project.artifact_name());
    let artifact = if artifact.is_file() {
        artifact
    } else {
        op_package(project_dir)?
    };
    let device = device::resolve_device(device_name)?;
    device::install(&device, &artifact)?;
    eprintln!("[pyxis] installed {} on {}", project.config.id, device.host);
    Ok(())
}

/// `pyxis run [--device <name>]` — install, then launch.
pub fn op_run(project_dir: &Path, device_name: Option<&str>) -> Result<()> {
    let project = Project::load(project_dir)?;
    op_install(project_dir, device_name)?;
    let device = device::resolve_device(device_name)?;
    device::launch(&device, &project.config.id)
}

/// `pyxis lint` — full config + composed-manifest validation without building.
pub fn op_lint(project_dir: &Path) -> Result<()> {
    let project = Project::load(project_dir)?; // validates pyxis.json
    let appinfo = manifest::AppInfo::from_project(&project)?; // validates manifest rules
    eprintln!(
        "[pyxis] lint OK: {} {} ({}, arch {})",
        appinfo.id, appinfo.version, appinfo.app_type, project.config.package.architecture
    );
    Ok(())
}

/// `pyxis doctor` — report host toolchain availability.
pub fn op_doctor(project_dir: &Path) -> Result<()> {
    let mut missing_required = false;

    let mut check = |name: &str, required: bool, why: &str| {
        match which::which(name) {
            Ok(path) => println!("  ok       {name:<12} {}", path.display()),
            Err(_) if required => {
                println!("  MISSING  {name:<12} ({why})");
                missing_required = true;
            }
            Err(_) => println!("  missing  {name:<12} optional: {why}"),
        }
    };

    println!("host toolchain:");
    check("tar", true, "package assembly");
    check("opkg-build", false, "preferred IPK packager; built-in writer used otherwise");
    check("ssh", false, "device install");
    check("scp", false, "device install");

    // Project-specific checks when run inside a project.
    if let Ok(project) = Project::load(project_dir) {
        println!("project {} ({}):", project.config.id, project.config.runtime);
        match project.config.runtime {
            Runtime::Web => {
                check("node", false, "web builders (vite/esbuild/webpack)");
                check("npm", false, "web builders (vite/esbuild/webpack)");
            }
            Runtime::Native => {
                if let Some(pdk) = &project.config.pdk {
                    let tool = match pdk.builder {
                        config::NativeBuilder::Cmake => "cmake",
                        config::NativeBuilder::Meson => "meson",
                        config::NativeBuilder::Cargo => "cargo",
                        config::NativeBuilder::Make => "make",
                        config::NativeBuilder::Qmake => "qmake",
                    };
                    check(tool, true, "configured pdk.builder");
                }
                check("gdb", false, "pyxis-pdk gdb");
                check("nm", false, "pyxis-pdk symbols");
            }
        }
    } else {
        println!("(not inside a pyxis project; skipping project checks)");
    }

    let devices = device::devices_path();
    if devices.is_file() {
        println!("  ok       devices      {}", devices.display());
    } else {
        println!("  missing  devices      {} (needed for install/run)", devices.display());
    }

    if missing_required {
        Err(PyxisError::Config("required host tools are missing".into()))
    } else {
        Ok(())
    }
}

/// `pyxis new <web|native> <dir>`
pub fn op_new(
    dir: &Path,
    runtime: Runtime,
    id: Option<String>,
    title: Option<String>,
    vendor: Option<String>,
) -> Result<()> {
    let dir_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("app")
        .to_lowercase()
        .replace([' ', '_'], "-");
    let spec = template::NewProject {
        dir,
        runtime,
        id: id.unwrap_or_else(|| format!("com.example.app.{}", dir_name.replace('-', ""))),
        title: title.unwrap_or_else(|| dir_name.clone()),
        vendor: vendor.unwrap_or_else(|| "Example Corp".into()),
    };
    template::scaffold(&spec)?;
    eprintln!(
        "[pyxis] created {} project `{}` at {}",
        runtime,
        spec.id,
        dir.display()
    );
    Ok(())
}

/// `pyxis-sdk inspect` — resolved project summary.
pub fn op_inspect(project_dir: &Path) -> Result<()> {
    let project = Project::load(project_dir)?;
    let appinfo = manifest::AppInfo::from_project(&project)?;
    println!("project root:  {}", project.root.display());
    println!("runtime:       {}", project.config.runtime);
    println!("app root:      {}", project.app_root());
    println!("artifact:      dist/{}", project.artifact_name());
    if project.config.runtime == Runtime::Web {
        println!("web payload:   {}", project.web_out_dir().display());
    } else {
        println!("build dir:     {}", project.build_dir().display());
        println!("entrypoint:    {}", appinfo.main);
    }
    println!("appinfo.json:\n{}", appinfo.to_json());
    println!("CONTROL/control:\n{}", control::render_control(&project));
    Ok(())
}

/// Load + assert the project matches the CLI's runtime (for pyxis-sdk/-pdk).
pub fn assert_runtime(project_dir: &Path, expected: Runtime) -> Result<Project> {
    let project = Project::load(project_dir)?;
    if project.config.runtime != expected {
        return Err(PyxisError::Config(format!(
            "this project is runtime `{}`; use {} instead",
            project.config.runtime,
            match expected {
                Runtime::Web => "pyxis-pdk (or the umbrella `pyxis`)",
                Runtime::Native => "pyxis-sdk (or the umbrella `pyxis`)",
            }
        )));
    }
    Ok(project)
}
