//! Staging tree generation (spec §14.5 web, §15.5 native) plus the §20
//! post-stage validation pass.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::builder::locate_native_binary;
use crate::config::{InstallMode, Project, Runtime};
use crate::control::{render_control, render_postinst, render_postrm, render_prerm};
use crate::error::{PyxisError, Result};
use crate::manifest::AppInfo;

fn io_err(context: &str, e: std::io::Error) -> PyxisError {
    PyxisError::Package(format!("{context}: {e}"))
}

/// Build the IPK filesystem tree under `.pyxis/pkg/` and return the manifest
/// that was staged. Assumes the payload has already been built.
pub fn stage(project: &Project) -> Result<AppInfo> {
    let appinfo = AppInfo::from_project(project)?;
    let pkg_dir = project.stage_pkg_dir();
    if pkg_dir.exists() {
        fs::remove_dir_all(&pkg_dir).map_err(|e| io_err("clearing stage dir", e))?;
    }
    let control_dir = pkg_dir.join("CONTROL");
    fs::create_dir_all(&control_dir).map_err(|e| io_err("creating CONTROL", e))?;

    // Package metadata
    fs::write(control_dir.join("control"), render_control(project))
        .map_err(|e| io_err("writing control", e))?;
    write_maintainer_scripts(project, &control_dir)?;

    // App root: /usr/pyxis/applications/<id>/
    let app_root = pkg_dir.join(project.app_root().trim_start_matches('/'));
    fs::create_dir_all(&app_root).map_err(|e| io_err("creating app root", e))?;

    match project.config.runtime {
        Runtime::Web => {
            let payload = project.web_out_dir();
            if !payload.is_dir() {
                return Err(PyxisError::Package(format!(
                    "web payload not found at {}; run `pyxis build` first",
                    payload.display()
                )));
            }
            copy_tree(&payload, &app_root)?;
        }
        Runtime::Native => {
            let binary = locate_native_binary(project)?;
            let bin_name = project.binary_name().unwrap();
            let dest = match project.install_mode() {
                InstallMode::BundledApp => pkg_dir
                    .join("opt/pyxis/apps")
                    .join(&project.config.id)
                    .join("bin")
                    .join(bin_name),
                InstallMode::SystemBin => pkg_dir.join("usr/bin").join(bin_name),
            };
            fs::create_dir_all(dest.parent().unwrap()).map_err(|e| io_err("creating bin dir", e))?;
            fs::copy(&binary, &dest).map_err(|e| io_err("copying binary", e))?;
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))
                .map_err(|e| io_err("chmod binary", e))?;
        }
    }

    // Companion service units (spec §11)
    for svc in project.services() {
        let src = project.root.join(&svc.unit);
        let dest = pkg_dir.join(
            project
                .service_unit_install_path(&svc.unit)
                .trim_start_matches('/'),
        );
        fs::create_dir_all(dest.parent().unwrap())
            .map_err(|e| io_err("creating service dir", e))?;
        fs::copy(&src, &dest).map_err(|e| io_err("copying service unit", e))?;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o644))
            .map_err(|e| io_err("chmod service unit", e))?;
    }

    // Icon + manifest into the app root
    let icon_src = project.root.join(&project.config.icon);
    fs::copy(&icon_src, app_root.join(&appinfo.icon)).map_err(|e| io_err("copying icon", e))?;
    fs::write(app_root.join("appinfo.json"), appinfo.to_json())
        .map_err(|e| io_err("writing appinfo.json", e))?;

    normalize_permissions(&pkg_dir)?;
    validate_stage(project, &appinfo)?;
    Ok(appinfo)
}

/// Normalize the staged tree to package-standard modes: directories 0755,
/// executables (maintainer scripts, native binaries) 0755, everything else
/// 0644. Without this the payload inherits whatever the developer's umask
/// produced — commonly 0664/0775, which would ship group-writable files
/// into a root-owned system directory. Applied to the staging tree rather
/// than at archive time so `opkg-build` and the built-in writer agree.
fn normalize_permissions(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|e| io_err("reading stage dir", e))? {
        let entry = entry.map_err(|e| io_err("reading stage entry", e))?;
        let path = entry.path();
        let meta = entry.metadata().map_err(|e| io_err("stat stage entry", e))?;
        if meta.is_dir() {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .map_err(|e| io_err("chmod stage dir", e))?;
            normalize_permissions(&path)?;
        } else if meta.is_file() {
            let executable = meta.permissions().mode() & 0o111 != 0;
            let mode = if executable { 0o755 } else { 0o644 };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                .map_err(|e| io_err("chmod stage file", e))?;
        }
    }
    Ok(())
}

/// Write `postinst`/`prerm`/`postrm` (and optionally `preinst`) into
/// CONTROL/. A developer-provided script at `<project>/packaging/<name>`
/// overrides the generated one verbatim (spec §9's standardized-but-
/// customizable maintainer script model); `preinst` is only emitted when
/// provided, since there is no generated default for it.
fn write_maintainer_scripts(project: &Project, control_dir: &Path) -> Result<()> {
    let generated: [(&str, Option<String>); 4] = [
        ("preinst", None),
        ("postinst", Some(render_postinst(project))),
        ("prerm", Some(render_prerm(project))),
        ("postrm", Some(render_postrm(project))),
    ];
    for (name, default_body) in generated {
        let override_path = project.root.join("packaging").join(name);
        let body = if override_path.is_file() {
            Some(
                fs::read_to_string(&override_path)
                    .map_err(|e| io_err(&format!("reading packaging/{name}"), e))?,
            )
        } else {
            default_body
        };
        let Some(body) = body else { continue };
        if !body.starts_with("#!/bin/sh") {
            return Err(PyxisError::Package(format!(
                "maintainer script {name} must begin with #!/bin/sh (spec section 9.1)"
            )));
        }
        let dest = control_dir.join(name);
        fs::write(&dest, body).map_err(|e| io_err(&format!("writing {name}"), e))?;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))
            .map_err(|e| io_err(&format!("chmod {name}"), e))?;
    }
    Ok(())
}

/// Recursive copy of the built web payload into the staged app root.
/// Skips `.ipk` artifacts and the toolchain's own `pkg/` staging output so a
/// `dist/`-based builder output can coexist with the `dist/` artifact dir.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    for entry in fs::read_dir(src).map_err(|e| io_err("reading payload dir", e))? {
        let entry = entry.map_err(|e| io_err("reading payload entry", e))?;
        let path = entry.path();
        let name = entry.file_name();
        let dest = dst.join(&name);
        let ty = entry.file_type().map_err(|e| io_err("stat payload entry", e))?;
        if ty.is_dir() {
            if name == "pkg" && src.file_name().map(|n| n == "dist").unwrap_or(false) {
                continue;
            }
            fs::create_dir_all(&dest).map_err(|e| io_err("creating payload dir", e))?;
            copy_tree(&path, &dest)?;
        } else if ty.is_file() {
            if name.to_string_lossy().ends_with(".ipk") {
                continue;
            }
            fs::copy(&path, &dest).map_err(|e| io_err("copying payload file", e))?;
        }
    }
    Ok(())
}

/// Spec §20 validation against the staged tree.
fn validate_stage(project: &Project, appinfo: &AppInfo) -> Result<()> {
    let pkg_dir = project.stage_pkg_dir();
    let app_root = pkg_dir.join(project.app_root().trim_start_matches('/'));

    if !app_root.join("appinfo.json").is_file() {
        return Err(PyxisError::Package("staged app root missing appinfo.json".into()));
    }
    if !app_root.join(&appinfo.icon).is_file() {
        return Err(PyxisError::Package(format!("staged app root missing icon {}", appinfo.icon)));
    }
    match project.config.runtime {
        Runtime::Web => {
            if !app_root.join(&appinfo.main).is_file() {
                return Err(PyxisError::Package(format!(
                    "web entry `{}` not present in staged payload",
                    appinfo.main
                )));
            }
        }
        Runtime::Native => {
            let staged_main = pkg_dir.join(appinfo.main.trim_start_matches('/'));
            let meta = staged_main.metadata().map_err(|_| {
                PyxisError::Package(format!(
                    "native binary `{}` not present in staged tree",
                    appinfo.main
                ))
            })?;
            if meta.permissions().mode() & 0o111 == 0 {
                return Err(PyxisError::Package(format!(
                    "staged binary `{}` is not executable",
                    appinfo.main
                )));
            }
        }
    }
    Ok(())
}
