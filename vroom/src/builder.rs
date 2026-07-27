//! Payload builders: web (SDK, spec §14) and native (PDK, spec §15).
//!
//! `pyxis build` dispatches on `pyxis.json.runtime` (spec §13.3). Builders
//! shell out to the project's normal toolchain with inherited stdio, so the
//! developer sees exactly what the underlying tool prints.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{NativeBuilder, Project, Runtime, WebBuilder};
use crate::error::{PyxisError, Result};

pub fn build(project: &Project) -> Result<()> {
    match project.config.runtime {
        Runtime::Web => build_web(project),
        Runtime::Native => build_native(project).map(|_| ()),
    }
}

fn run(project_root: &Path, program: &str, args: &[&str]) -> Result<()> {
    eprintln!("[pyxis] $ {} {}", program, args.join(" "));
    let status = Command::new(program)
        .args(args)
        .current_dir(project_root)
        .status()
        .map_err(|e| PyxisError::Build(format!("cannot run `{program}`: {e}")))?;
    if !status.success() {
        return Err(PyxisError::Build(format!(
            "`{program} {}` exited with {status}",
            args.join(" ")
        )));
    }
    Ok(())
}

fn has_npm_build_script(project_root: &Path) -> bool {
    let pkg = project_root.join("package.json");
    let Ok(raw) = std::fs::read_to_string(pkg) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("scripts")?.get("build").cloned())
        .is_some()
}

pub fn build_web(project: &Project) -> Result<()> {
    let builder = project
        .config
        .sdk
        .as_ref()
        .ok_or_else(|| PyxisError::Config("web apps require an `sdk` section".into()))?
        .builder;
    match builder {
        WebBuilder::Static => {
            // No build step; payload is served straight from publicDir/outDir.
            let dir = project.web_out_dir();
            if !dir.is_dir() {
                return Err(PyxisError::Build(format!(
                    "static app payload directory not found: {}",
                    dir.display()
                )));
            }
            Ok(())
        }
        // Prefer the project's own `npm run build` contract; fall back to
        // invoking the builder directly through npx.
        WebBuilder::Vite | WebBuilder::Esbuild | WebBuilder::Webpack => {
            if has_npm_build_script(&project.root) {
                run(&project.root, "npm", &["run", "build"])
            } else {
                match builder {
                    WebBuilder::Vite => run(&project.root, "npx", &["--no-install", "vite", "build"]),
                    WebBuilder::Webpack => {
                        run(&project.root, "npx", &["--no-install", "webpack", "--mode", "production"])
                    }
                    WebBuilder::Esbuild => Err(PyxisError::Build(
                        "esbuild projects need a `build` script in package.json".into(),
                    )),
                    WebBuilder::Static => unreachable!(),
                }
            }
        }
    }
}

/// Build the native payload and return the path to the produced executable.
pub fn build_native(project: &Project) -> Result<PathBuf> {
    let pdk = project
        .config
        .pdk
        .as_ref()
        .ok_or_else(|| PyxisError::Config("native apps require a `pdk` section".into()))?;
    let build_dir = project.build_dir();
    let build_dir_str = build_dir.to_string_lossy().to_string();

    match pdk.builder {
        NativeBuilder::Cmake => {
            run(&project.root, "cmake", &["-S", ".", "-B", &build_dir_str, "-DCMAKE_BUILD_TYPE=Release"])?;
            run(&project.root, "cmake", &["--build", &build_dir_str, "--config", "Release"])?;
        }
        NativeBuilder::Meson => {
            if !build_dir.join("build.ninja").exists() {
                run(&project.root, "meson", &["setup", &build_dir_str])?;
            }
            run(&project.root, "meson", &["compile", "-C", &build_dir_str])?;
        }
        NativeBuilder::Cargo => {
            run(&project.root, "cargo", &["build", "--release"])?;
        }
        NativeBuilder::Make => {
            run(&project.root, "make", &[])?;
        }
        NativeBuilder::Qmake => {
            run(&project.root, "qmake", &[])?;
            run(&project.root, "make", &[])?;
        }
    }
    locate_native_binary(project)
}

/// Find the built executable for staging. Checked in order:
/// `<buildDir>/<bin>`, `<buildDir>/bin/<bin>`, `target/release/<bin>` (cargo).
pub fn locate_native_binary(project: &Project) -> Result<PathBuf> {
    let bin = project
        .binary_name()
        .ok_or_else(|| PyxisError::Config("native apps require pdk.binaryName".into()))?;
    let candidates = [
        project.build_dir().join(bin),
        project.build_dir().join("bin").join(bin),
        project.root.join("target/release").join(bin),
    ];
    for path in &candidates {
        if path.is_file() {
            return Ok(path.clone());
        }
    }
    Err(PyxisError::Build(format!(
        "built binary `{bin}` not found (looked in {}); run `pyxis build` first",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}
