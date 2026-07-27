//! Project scaffolding for `pyxis new` / `pyxis-sdk new web` / `pyxis-pdk new native`.
//!
//! Web projects scaffold with the `static` builder so they work with zero
//! extra toolchain; the README explains switching to vite. Native projects
//! scaffold with `make` and a hello-world C program for the same reason.

use std::fs;
use std::path::Path;

use crate::config::Runtime;
use crate::error::{PyxisError, Result};

/// Minimal valid 1x1 transparent PNG, used as the placeholder icon.
const PLACEHOLDER_ICON: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
    0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
    0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
    0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
    0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

pub struct NewProject<'a> {
    pub dir: &'a Path,
    pub runtime: Runtime,
    pub id: String,
    pub title: String,
    pub vendor: String,
}

fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "arm" => "armv7",
        other => {
            eprintln!("[pyxis] warning: unmapped host arch `{other}`, defaulting to x86_64");
            "x86_64"
        }
    }
}

fn write(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| PyxisError::Config(format!("creating {}: {e}", parent.display())))?;
    }
    fs::write(path, contents)
        .map_err(|e| PyxisError::Config(format!("writing {}: {e}", path.display())))
}

pub fn scaffold(spec: &NewProject) -> Result<()> {
    if spec.dir.exists() && spec.dir.read_dir().map(|mut d| d.next().is_some()).unwrap_or(true) {
        return Err(PyxisError::Config(format!(
            "target directory {} exists and is not empty",
            spec.dir.display()
        )));
    }
    match spec.runtime {
        Runtime::Web => scaffold_web(spec),
        Runtime::Native => scaffold_native(spec),
    }
}

fn scaffold_web(spec: &NewProject) -> Result<()> {
    let pyxis_json = serde_json::json!({
        "id": spec.id,
        "title": spec.title,
        "vendor": spec.vendor,
        "version": "0.1.0",
        "runtime": "web",
        "publicDir": "public",
        "entry": "index.html",
        "icon": "assets/icon.png",
        "sdk": { "builder": "static" },
        "package": {
            "section": "applications",
            "priority": "optional",
            "architecture": "all"
        }
    });
    write(
        &spec.dir.join("pyxis.json"),
        serde_json::to_string_pretty(&pyxis_json).unwrap().as_bytes(),
    )?;
    write(
        &spec.dir.join("public/index.html"),
        format!(
            "<!doctype html>\n<html>\n<head><meta charset=\"utf-8\"><title>{title}</title></head>\n\
             <body>\n  <h1>{title}</h1>\n  <p>Hello from Pyxis.</p>\n</body>\n</html>\n",
            title = spec.title
        )
        .as_bytes(),
    )?;
    write(&spec.dir.join("assets/icon.png"), PLACEHOLDER_ICON)?;
    write(
        &spec.dir.join("README.md"),
        format!(
            "# {title}\n\nA Pyxis web app.\n\n\
             - `pyxis build` — no-op for the `static` builder; serves `public/` as-is.\n\
             - `pyxis package` — emits `dist/{id}_0.1.0_all.ipk`.\n\
             - `pyxis install --device <name>` — installs to a device from `~/.config/pyxis/devices.json`.\n\n\
             To use vite instead, set `sdk.builder` to `\"vite\"`, add a package.json with a\n\
             `build` script, and the SDK will stage the build output from `dist/`.\n",
            title = spec.title,
            id = spec.id
        )
        .as_bytes(),
    )
}

fn scaffold_native(spec: &NewProject) -> Result<()> {
    let bin_name = spec.id.rsplit('.').next().unwrap_or("app").to_string();
    let pyxis_json = serde_json::json!({
        "id": spec.id,
        "title": spec.title,
        "vendor": spec.vendor,
        "version": "0.1.0",
        "runtime": "native",
        "sourceDir": "src",
        "buildDir": "build",
        "icon": "assets/icon.png",
        "pdk": {
            "builder": "make",
            "binaryName": bin_name,
            "installMode": "bundled-app"
        },
        "package": {
            "section": "applications",
            "priority": "optional",
            "architecture": host_arch()
        }
    });
    write(
        &spec.dir.join("pyxis.json"),
        serde_json::to_string_pretty(&pyxis_json).unwrap().as_bytes(),
    )?;
    write(
        &spec.dir.join("src/main.c"),
        format!(
            "#include <stdio.h>\n\nint main(void) {{\n    printf(\"{title}: hello from Pyxis\\n\");\n    return 0;\n}}\n",
            title = spec.title
        )
        .as_bytes(),
    )?;
    write(
        &spec.dir.join("Makefile"),
        format!(
            "CC ?= cc\nCFLAGS ?= -O2 -Wall\n\nall: build/{bin}\n\nbuild/{bin}: src/main.c\n\
             \tmkdir -p build\n\t$(CC) $(CFLAGS) -o $@ $<\n\nclean:\n\trm -rf build\n\n.PHONY: all clean\n",
            bin = bin_name
        )
        .as_bytes(),
    )?;
    write(&spec.dir.join("assets/icon.png"), PLACEHOLDER_ICON)?;
    write(
        &spec.dir.join("README.md"),
        format!(
            "# {title}\n\nA Pyxis native app.\n\n\
             - `pyxis build` — runs `make`, producing `build/{bin}`.\n\
             - `pyxis package` — emits `dist/{id}_0.1.0_{arch}.ipk` (bundled-app mode:\n\
               binary installs to `/opt/pyxis/apps/{id}/bin/{bin}`).\n\
             - `pyxis install --device <name>` — installs to a device from `~/.config/pyxis/devices.json`.\n",
            title = spec.title,
            id = spec.id,
            bin = bin_name,
            arch = host_arch()
        )
        .as_bytes(),
    )
}
