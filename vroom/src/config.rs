//! `pyxis.json` project configuration: parsing and validation.
//!
//! Mirrors schemas/pyxis.schema.json (JSON Schema Draft 2020-12). The schema
//! stays canonical for external tooling; this module enforces the same rules
//! natively so the CLIs need no JSON Schema engine.

use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::{PyxisError, Result};

/// App id: reverse-DNS (`com.example.app.clock`) for third-party apps, or a
/// single lowercase segment (`notes`) for Pyxis's own first-party apps.
/// Single-segment ids are not a concession — for system apps the id is
/// load-bearing identity across the whole stack (it is the `pyxis://<id>`
/// origin authority, the profile dir name under `/var/lib/pyxis/profiles`,
/// and the `PYXIS_APP_ID` Evo binds windows by), so it is fixed by the
/// platform, not chosen at packaging time.
pub const ID_PATTERN: &str = r"^[a-z0-9][a-z0-9_-]*(\.[a-z0-9_-]+)*$";
pub const VERSION_PATTERN: &str = r"^[0-9]+\.[0-9]+\.[0-9]+([.-][A-Za-z0-9]+)?$";

/// The permission vocabulary a `pyxis.json` may declare in `permissions[]`,
/// copied verbatim into the built `appinfo.json`'s `requiredPermissions[]`
/// (SDK/PDK spec §19.1). Kept in lockstep with `schemas/pyxis.schema.json`,
/// `schemas/appinfo.schema.json`, and echo's `nova::APPINFO_PERMISSIONS`.
pub const PERMISSIONS: &[&str] = &[
    "network",
    "notifications",
    "camera",
    "microphone",
    "location",
    "bluetooth",
    "filesystem.read",
    "filesystem.write",
    "ipc",
    "background",
    "power",
    "audio",
    "browser",
    "notifications.manage",
    "appmanager.install",
    "appmanager.manage",
    "settings.write",
    "windows.manage",
    "terminal",
    "filesystem.user",
    "filesystem.system",
    "filesystem.apps",
];

/// The subset of [`PERMISSIONS`] that is *privileged*: declaring it is legal
/// in any `pyxis.json`, but Nova only grants the corresponding bridge
/// capability to apps on its platform trust list (spec §19.2). A third-party
/// `.ipk` may therefore declare these and still build — it just won't be
/// granted them at runtime. Packaging emits a warning so the author learns
/// this at build time rather than by silent no-op on device.
pub const PRIVILEGED_PERMISSIONS: &[&str] = &[
    "notifications.manage",
    "appmanager.install",
    "appmanager.manage",
    "settings.write",
    "windows.manage",
    "terminal",
    "filesystem.user",
    "filesystem.system",
    "filesystem.apps",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Web,
    Native,
}

impl std::fmt::Display for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Runtime::Web => write!(f, "web"),
            Runtime::Native => write!(f, "native"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebBuilder {
    Vite,
    Esbuild,
    Webpack,
    Static,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NativeBuilder {
    Cmake,
    Meson,
    Cargo,
    Make,
    Qmake,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallMode {
    #[serde(rename = "bundled-app")]
    BundledApp,
    #[serde(rename = "system-bin")]
    SystemBin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdkConfig {
    pub builder: WebBuilder,
    #[serde(rename = "outDir", skip_serializing_if = "Option::is_none")]
    pub out_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdkConfig {
    pub builder: NativeBuilder,
    #[serde(rename = "binaryName", skip_serializing_if = "Option::is_none")]
    pub binary_name: Option<String>,
    #[serde(rename = "installMode", skip_serializing_if = "Option::is_none")]
    pub install_mode: Option<InstallMode>,
    #[serde(rename = "bundleLibraries", skip_serializing_if = "Option::is_none")]
    pub bundle_libraries: Option<bool>,
}

/// A companion service the package ships (spec §11). `unit` is a
/// project-relative systemd unit file; it is staged into the package and
/// (policy permitting) enabled by the generated maintainer scripts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// Reverse-DNS service name, listed in appinfo.json `services[]`.
    pub name: String,
    /// Project-relative path to the systemd unit file to install.
    pub unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageConfig {
    pub section: String,
    pub priority: String,
    pub architecture: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends: Option<Vec<String>>,
    #[serde(rename = "maintainerEmail", skip_serializing_if = "Option::is_none")]
    pub maintainer_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub id: String,
    pub title: String,
    pub vendor: String,
    pub version: String,
    pub runtime: Runtime,
    #[serde(rename = "sourceDir", skip_serializing_if = "Option::is_none")]
    pub source_dir: Option<String>,
    #[serde(rename = "publicDir", skip_serializing_if = "Option::is_none")]
    pub public_dir: Option<String>,
    #[serde(rename = "buildDir", skip_serializing_if = "Option::is_none")]
    pub build_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    pub icon: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk: Option<SdkConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdk: Option<PdkConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<ServiceConfig>>,
    pub package: PackageConfig,
}

/// A `ProjectConfig` loaded from disk, validated, with defaults resolved.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub config: ProjectConfig,
}

impl Project {
    pub fn load(project_dir: &Path) -> Result<Self> {
        let root = project_dir
            .canonicalize()
            .map_err(|e| PyxisError::Config(format!("cannot resolve project dir {}: {e}", project_dir.display())))?;
        let path = root.join("pyxis.json");
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| PyxisError::Config(format!("cannot read {}: {e}", path.display())))?;
        let config: ProjectConfig = serde_json::from_str(&raw)
            .map_err(|e| PyxisError::Config(format!("invalid pyxis.json: {e}")))?;
        let project = Project { root, config };
        project.validate()?;
        Ok(project)
    }

    /// Validation per spec §20 and schemas/pyxis.schema.json.
    pub fn validate(&self) -> Result<()> {
        let c = &self.config;
        let id_re = Regex::new(ID_PATTERN).unwrap();
        let ver_re = Regex::new(VERSION_PATTERN).unwrap();

        if !id_re.is_match(&c.id) {
            return Err(PyxisError::Config(format!(
                "id `{}` must be reverse-DNS or a single lowercase segment ({})",
                c.id, ID_PATTERN
            )));
        }
        if !ver_re.is_match(&c.version) {
            return Err(PyxisError::Config(format!(
                "version `{}` must be semantic ({})",
                c.version, VERSION_PATTERN
            )));
        }
        if c.title.is_empty() || c.vendor.is_empty() || c.icon.is_empty() {
            return Err(PyxisError::Config(
                "title, vendor, and icon must be non-empty".into(),
            ));
        }
        if let Some(perms) = &c.permissions {
            for p in perms {
                if !PERMISSIONS.contains(&p.as_str()) {
                    return Err(PyxisError::Config(format!(
                        "unknown permission `{p}` (allowed: {})",
                        PERMISSIONS.join(", ")
                    )));
                }
                if PRIVILEGED_PERMISSIONS.contains(&p.as_str()) {
                    eprintln!(
                        "[pyxis] warning: `{p}` is a privileged permission; it is granted \
                         only to apps on the platform trust list. A sideloaded .ipk may \
                         declare it but will not receive it at runtime (spec §19.2)."
                    );
                }
            }
        }
        if let Some(services) = &c.services {
            for svc in services {
                if !id_re.is_match(&svc.name) {
                    return Err(PyxisError::Config(format!(
                        "service name `{}` must be reverse-DNS or a single lowercase segment ({})",
                        svc.name, ID_PATTERN
                    )));
                }
                let unit_path = self.root.join(&svc.unit);
                if !unit_path.is_file() {
                    return Err(PyxisError::Config(format!(
                        "service unit not found at {}",
                        unit_path.display()
                    )));
                }
                if !svc.unit.ends_with(".service") {
                    return Err(PyxisError::Config(format!(
                        "service unit `{}` must be a .service file",
                        svc.unit
                    )));
                }
            }
        }

        match c.runtime {
            Runtime::Web => {
                if c.entry.is_none() {
                    return Err(PyxisError::Config("web apps require `entry`".into()));
                }
                if c.sdk.is_none() {
                    return Err(PyxisError::Config("web apps require an `sdk` section".into()));
                }
                if c.package.architecture != "all" {
                    return Err(PyxisError::Config(
                        "web apps require package.architecture == \"all\"".into(),
                    ));
                }
                if let Some(entry) = &c.entry {
                    if entry.starts_with('/') {
                        return Err(PyxisError::Config("web `entry` must be a relative path".into()));
                    }
                }
            }
            Runtime::Native => {
                let pdk = c
                    .pdk
                    .as_ref()
                    .ok_or_else(|| PyxisError::Config("native apps require a `pdk` section".into()))?;
                if pdk.binary_name.as_deref().unwrap_or("").is_empty() {
                    return Err(PyxisError::Config("native apps require pdk.binaryName".into()));
                }
                if c.package.architecture == "all" {
                    return Err(PyxisError::Config(
                        "native apps must not use package.architecture == \"all\"".into(),
                    ));
                }
            }
        }

        let icon_path = self.root.join(&c.icon);
        if !icon_path.is_file() {
            return Err(PyxisError::Config(format!(
                "icon not found at {}",
                icon_path.display()
            )));
        }
        Ok(())
    }

    // --- resolved defaults (schema `default` values) ---

    pub fn source_dir(&self) -> PathBuf {
        self.root.join(self.config.source_dir.as_deref().unwrap_or("src"))
    }

    pub fn public_dir(&self) -> PathBuf {
        self.root.join(self.config.public_dir.as_deref().unwrap_or("public"))
    }

    pub fn build_dir(&self) -> PathBuf {
        self.root.join(self.config.build_dir.as_deref().unwrap_or("build"))
    }

    /// Directory holding built web payload, staged verbatim into the app root.
    /// For the `static` builder this defaults to `publicDir` (no build step);
    /// for real builders it defaults to `dist` unless `sdk.outDir` overrides.
    pub fn web_out_dir(&self) -> PathBuf {
        let sdk = self.config.sdk.as_ref();
        if let Some(out) = sdk.and_then(|s| s.out_dir.as_deref()) {
            return self.root.join(out);
        }
        match sdk.map(|s| s.builder) {
            Some(WebBuilder::Static) => self.public_dir(),
            _ => self.root.join("dist"),
        }
    }

    pub fn install_mode(&self) -> InstallMode {
        self.config
            .pdk
            .as_ref()
            .and_then(|p| p.install_mode)
            .unwrap_or(InstallMode::BundledApp)
    }

    pub fn binary_name(&self) -> Option<&str> {
        self.config.pdk.as_ref().and_then(|p| p.binary_name.as_deref())
    }

    /// Installed app root (spec §5): `/usr/pyxis/applications/<app-id>`.
    pub fn app_root(&self) -> String {
        format!("/usr/pyxis/applications/{}", self.config.id)
    }

    /// Native entrypoint path on the target per install mode (spec §7.3).
    pub fn native_main(&self) -> Result<String> {
        let bin = self
            .binary_name()
            .ok_or_else(|| PyxisError::Config("native apps require pdk.binaryName".into()))?;
        Ok(match self.install_mode() {
            InstallMode::BundledApp => format!("/opt/pyxis/apps/{}/bin/{}", self.config.id, bin),
            InstallMode::SystemBin => format!("/usr/bin/{bin}"),
        })
    }

    /// Where a shipped service unit installs on the target (spec §11.2):
    /// bundled-app native apps keep units under their payload root (enabled
    /// by absolute path); everything else goes into systemd's unit dir.
    pub fn service_unit_install_path(&self, unit: &str) -> String {
        let base = Path::new(unit)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| unit.to_string());
        match (self.config.runtime, self.install_mode()) {
            (Runtime::Native, InstallMode::BundledApp) => {
                format!("/opt/pyxis/apps/{}/services/{}", self.config.id, base)
            }
            _ => format!("/usr/lib/systemd/system/{base}"),
        }
    }

    pub fn services(&self) -> &[ServiceConfig] {
        self.config.services.as_deref().unwrap_or(&[])
    }

    pub fn maintainer(&self) -> String {
        format!(
            "{} <{}>",
            self.config.vendor,
            self.config
                .package
                .maintainer_email
                .as_deref()
                .unwrap_or("devnull@example.invalid")
        )
    }

    pub fn description(&self) -> &str {
        self.config
            .package
            .description
            .as_deref()
            .unwrap_or(&self.config.title)
    }

    /// `<id>_<version>_<arch>.ipk` (spec §24).
    pub fn artifact_name(&self) -> String {
        format!(
            "{}_{}_{}.ipk",
            self.config.id, self.config.version, self.config.package.architecture
        )
    }

    /// Toolchain workspace inside the project (staging, intermediates).
    pub fn work_dir(&self) -> PathBuf {
        self.root.join(".pyxis")
    }

    pub fn stage_pkg_dir(&self) -> PathBuf {
        self.work_dir().join("pkg")
    }

    pub fn dist_dir(&self) -> PathBuf {
        self.root.join("dist")
    }
}
