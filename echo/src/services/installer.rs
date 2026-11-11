//! Installer — `.ipk`/opkg package installation over Echo.
//!
//! Owns the *package* side of app installation: discovering `.ipk` files,
//! reading their metadata without installing, and driving `opkg
//! install`/`opkg remove`. It deliberately does **not** own the app
//! registry — after opkg succeeds, this service calls
//! [`crate::services::nova::Nova::reload`] so the launcher picks the app up
//! immediately. That split mirrors the boundary already enforced elsewhere:
//! opkg owns the files and its status database (which is why
//! `Nova::uninstall` refuses ipk apps and points here), Nova owns identity,
//! origins, and launch.
//!
//! Packaging contract: `vroom/docs/pyxis-sdk-pdk-spec.md` (§4 package
//! format, §17.2 install sequence, §18 upgrade/removal semantics).
//!
//! Progress is reported two ways on purpose. The `Install` method is
//! awaited and returns the terminal result, so a caller always gets a
//! definitive answer even if it missed a signal; alongside that it emits
//! `InstallProgress`/`InstallCompleted`/`InstallFailed` on
//! `org.geckoOS.installer` so a wizard UI can show live phase updates
//! rather than a spinner with no detail.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use crate::services::apphost::AppHost;
use crate::services::nova::{forget_app_state, AppTier, Nova, APP_EXIT_LIMIT};
use crate::IPCBus;

/// Interface name for installer IPC + signals.
pub const INSTALLER_IFACE: &str = "org.geckoOS.installer";

/// Directories scanned for installable `.ipk` files, in order. `/tmp` is
/// included because that is where `pyxis install` (Vroom's device install,
/// spec §17.2) scp's packages before running opkg.
const PACKAGE_SEARCH_ROOTS: &[&str] = &[
    "/var/lib/pyxis/incoming",
    "/media",
    "/tmp",
];

/// Directories an `.ipk` may legitimately be installed *from*. Installing a
/// package is a root-privileged operation that runs arbitrary maintainer
/// scripts, so the path is constrained to the same drop locations the UI
/// offers rather than accepting any absolute path from a caller.
const ALLOWED_INSTALL_ROOTS: &[&str] = &[
    "/var/lib/pyxis/incoming",
    "/media",
    "/tmp",
    "/home",
    "/root",
];

/// Package metadata read out of an `.ipk` without installing it.
#[derive(Debug, Clone, Serialize)]
pub struct PackageInfo {
    pub path: String,
    pub package: String,
    pub version: String,
    pub architecture: String,
    pub description: String,
    pub maintainer: String,
    #[serde(rename = "installedSize")]
    pub installed_size: u64,
    /// Fields sourced from the staged `appinfo.json`, when the package
    /// carries one (every Pyxis GUI app does; a plain library package may
    /// not).
    pub title: Option<String>,
    pub vendor: Option<String>,
    #[serde(rename = "appType")]
    pub app_type: Option<String>,
    #[serde(rename = "requiredPermissions")]
    pub required_permissions: Vec<String>,
    /// True when a package with this id is already installed.
    pub installed: bool,
    #[serde(rename = "installedVersion")]
    pub installed_version: Option<String>,
}

pub struct InstallerService {
    bus: RwLock<Option<Arc<IPCBus>>>,
}

impl InstallerService {
    fn new() -> Self {
        Self {
            bus: RwLock::new(None),
        }
    }

    pub fn global() -> &'static Arc<InstallerService> {
        static INSTALLER: OnceLock<Arc<InstallerService>> = OnceLock::new();
        INSTALLER.get_or_init(|| Arc::new(InstallerService::new()))
    }

    pub fn set_bus(&self, bus: Arc<IPCBus>) {
        if let Ok(mut slot) = self.bus.write() {
            *slot = Some(bus);
        }
    }

    fn emit(&self, signal_name: &'static str, payload: Value) {
        let bus = match self.bus.read() {
            Ok(g) => g.clone(),
            Err(_) => None,
        };
        let Some(bus) = bus else { return };
        tokio::spawn(async move {
            if let Err(e) = bus.emit_signal(INSTALLER_IFACE, signal_name, payload).await {
                tracing::warn!("installer signal '{}' failed: {}", signal_name, e);
            }
        });
    }

    fn progress(&self, package: &str, phase: &str, message: &str) {
        tracing::info!("installer[{}] {}: {}", package, phase, message);
        self.emit(
            "InstallProgress",
            json!({ "package": package, "phase": phase, "message": message }),
        );
    }

    // ---------- Discovery ----------

    /// Scan the well-known drop directories (or `dir` when given) for
    /// `.ipk` files and return their metadata. Unreadable/corrupt packages
    /// are skipped with a warning rather than failing the whole scan — one
    /// bad file in /tmp must not hide every valid package.
    pub fn list_available(&self, dir: Option<&str>) -> Result<Value> {
        let roots: Vec<PathBuf> = match dir {
            Some(d) => vec![PathBuf::from(d)],
            None => PACKAGE_SEARCH_ROOTS.iter().map(PathBuf::from).collect(),
        };

        let installed = self.installed_versions().unwrap_or_default();
        let mut packages = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for root in roots {
            if !root.is_dir() {
                continue;
            }
            let entries = match fs::read_dir(&root) {
                Ok(e) => e,
                Err(e) => {
                    tracing::debug!("installer: read_dir {} failed: {}", root.display(), e);
                    continue;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file()
                    || path.extension().and_then(|s| s.to_str()) != Some("ipk")
                {
                    continue;
                }
                if !seen.insert(path.clone()) {
                    continue;
                }
                match Self::inspect_path(&path) {
                    Ok(mut info) => {
                        if let Some(v) = installed.get(&info.package) {
                            info.installed = true;
                            info.installed_version = Some(v.clone());
                        }
                        packages.push(info);
                    }
                    Err(e) => {
                        tracing::warn!("installer: skipping {}: {}", path.display(), e);
                    }
                }
            }
        }

        packages.sort_by(|a, b| a.package.cmp(&b.package));
        Ok(json!({ "packages": packages, "count": packages.len() }))
    }

    /// Read one package's metadata without installing it.
    pub fn inspect(&self, package_path: &str) -> Result<Value> {
        let path = Self::validate_package_path(package_path)?;
        let mut info = Self::inspect_path(&path)?;
        if let Some(v) = self.installed_versions().unwrap_or_default().get(&info.package) {
            info.installed = true;
            info.installed_version = Some(v.clone());
        }
        Ok(serde_json::to_value(info)?)
    }

    /// Extract `CONTROL/control` (and `appinfo.json` when present) from an
    /// `.ipk` into a temp dir and parse them. The archive member may be
    /// named `control.tar.gz` or `./control.tar.gz` depending on which
    /// packager produced it (Vroom's built-in writer vs `opkg-build`), so
    /// both spellings are handled.
    fn inspect_path(path: &Path) -> Result<PackageInfo> {
        let staging = std::env::temp_dir().join(format!(
            "pyxis-inspect-{}-{}",
            std::process::id(),
            path.file_stem().and_then(|s| s.to_str()).unwrap_or("pkg")
        ));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging)?;
        let cleanup = |s: &Path| {
            let _ = fs::remove_dir_all(s);
        };

        let outer = std::process::Command::new("tar")
            .arg("-xzf")
            .arg(path)
            .arg("-C")
            .arg(&staging)
            .status();
        match outer {
            Ok(s) if s.success() => {}
            Ok(s) => {
                cleanup(&staging);
                return Err(anyhow!("tar exited with {} reading {}", s, path.display()));
            }
            Err(e) => {
                cleanup(&staging);
                return Err(anyhow!("invoke tar on {}: {}", path.display(), e));
            }
        }

        let control_tar = ["control.tar.gz", "./control.tar.gz"]
            .iter()
            .map(|n| staging.join(n))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                cleanup(&staging);
                anyhow!("{} has no control.tar.gz (not an ipk?)", path.display())
            })?;

        let control_dir = staging.join("control");
        fs::create_dir_all(&control_dir)?;
        let st = std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&control_tar)
            .arg("-C")
            .arg(&control_dir)
            .status();
        if !matches!(st, Ok(ref s) if s.success()) {
            cleanup(&staging);
            return Err(anyhow!("cannot extract control.tar.gz from {}", path.display()));
        }

        let control_file = ["control", "./control"]
            .iter()
            .map(|n| control_dir.join(n))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                cleanup(&staging);
                anyhow!("control.tar.gz in {} has no control file", path.display())
            })?;
        let control_raw = fs::read_to_string(&control_file)?;
        let fields = parse_control(&control_raw);

        let package = fields
            .get("Package")
            .cloned()
            .ok_or_else(|| {
                cleanup(&staging);
                anyhow!("control in {} has no Package field", path.display())
            })?;

        // appinfo.json lives in the data archive; read it best-effort so a
        // non-GUI package still inspects cleanly.
        let (title, vendor, app_type, perms) = Self::read_appinfo(&staging, &package);

        let installed_size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        cleanup(&staging);

        Ok(PackageInfo {
            path: path.to_string_lossy().to_string(),
            package,
            version: fields.get("Version").cloned().unwrap_or_default(),
            architecture: fields.get("Architecture").cloned().unwrap_or_default(),
            description: fields.get("Description").cloned().unwrap_or_default(),
            maintainer: fields.get("Maintainer").cloned().unwrap_or_default(),
            installed_size,
            title,
            vendor,
            app_type,
            required_permissions: perms,
            installed: false,
            installed_version: None,
        })
    }

    /// Pull `appinfo.json` out of the already-extracted data archive.
    /// Returns defaults when the package has none.
    fn read_appinfo(
        staging: &Path,
        package: &str,
    ) -> (Option<String>, Option<String>, Option<String>, Vec<String>) {
        let empty = (None, None, None, Vec::new());
        let data_tar = ["data.tar.gz", "./data.tar.gz"]
            .iter()
            .map(|n| staging.join(n))
            .find(|p| p.is_file());
        let Some(data_tar) = data_tar else {
            return empty;
        };
        let data_dir = staging.join("data");
        if fs::create_dir_all(&data_dir).is_err() {
            return empty;
        }
        // Only the manifest is needed, so extract with a wildcard match
        // rather than unpacking a potentially large payload.
        let st = std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&data_tar)
            .arg("-C")
            .arg(&data_dir)
            .arg("--wildcards")
            .arg("*/appinfo.json")
            .status();
        if !matches!(st, Ok(ref s) if s.success()) {
            return empty;
        }
        let manifest = find_file(&data_dir, "appinfo.json", 8);
        let Some(manifest) = manifest else {
            return empty;
        };
        let Ok(raw) = fs::read_to_string(&manifest) else {
            return empty;
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            return empty;
        };
        if v.get("id").and_then(|s| s.as_str()) != Some(package) {
            tracing::warn!(
                "installer: appinfo id mismatch in package '{}' (manifest says {:?})",
                package,
                v.get("id").and_then(|s| s.as_str())
            );
        }
        let perms = v
            .get("requiredPermissions")
            .and_then(|p| p.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        (
            v.get("title").and_then(|s| s.as_str()).map(str::to_string),
            v.get("vendor").and_then(|s| s.as_str()).map(str::to_string),
            v.get("type").and_then(|s| s.as_str()).map(str::to_string),
            perms,
        )
    }

    // ---------- Install / remove ----------

    /// `installer.Install(packagePath, force?)` — run `opkg install`, then
    /// refresh Nova's registry so the app is immediately launchable.
    /// Emits progress signals throughout and returns the terminal result.
    pub async fn install(&self, package_path: &str, force: bool) -> Result<Value> {
        let path = Self::validate_package_path(package_path)?;
        let info = Self::inspect_path(&path)?;
        let package = info.package.clone();

        self.emit(
            "InstallStarted",
            json!({
                "package": package,
                "version": info.version,
                "path": info.path,
            }),
        );
        self.progress(&package, "verifying", "Reading package metadata");

        Self::require_opkg().map_err(|e| {
            self.fail(&package, &e.to_string());
            e
        })?;

        self.progress(&package, "installing", "Running opkg install");
        let mut cmd = tokio::process::Command::new("opkg");
        cmd.arg("install");
        if force {
            cmd.arg("--force-reinstall");
        }
        cmd.arg(&path);

        let output = match cmd.output().await {
            Ok(o) => o,
            Err(e) => {
                let msg = format!("cannot run opkg: {e}");
                self.fail(&package, &msg);
                return Err(anyhow!(msg));
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            // opkg reports the actionable reason on stderr (dependency
            // conflicts, "already installed", bad architecture) — surface it
            // verbatim rather than a generic failure.
            let msg = if stderr.trim().is_empty() {
                format!("opkg install exited with {}", output.status)
            } else {
                stderr.trim().to_string()
            };
            self.fail(&package, &msg);
            return Err(anyhow!(msg));
        }

        // The package's own postinst already calls `pyxis-appctl rescan`
        // when that binary is on the image; calling reload directly makes
        // the refresh unconditional (and synchronous with this response) so
        // the UI can navigate straight to a launchable app.
        self.progress(&package, "registering", "Refreshing app registry");
        let registered = match Nova::global().reload() {
            Ok(count) => count,
            Err(e) => {
                tracing::warn!("installer: registry reload after install failed: {}", e);
                0
            }
        };

        let launchable = Nova::global().get_info(&package).is_ok();
        let result = json!({
            "package": package,
            "version": info.version,
            "title": info.title,
            "appType": info.app_type,
            "registeredApps": registered,
            "launchable": launchable,
            "log": format!("{stdout}{stderr}").trim().to_string(),
        });
        self.emit("InstallCompleted", result.clone());
        tracing::info!("installer: installed '{}' {}", package, info.version);
        Ok(result)
    }

    fn fail(&self, package: &str, message: &str) {
        tracing::error!("installer: install of '{}' failed: {}", package, message);
        self.emit(
            "InstallFailed",
            json!({ "package": package, "error": message }),
        );
    }

    /// `installer.Remove(package, purge?)` — `opkg remove`, then refresh the
    /// registry. This is the supported removal path for ipk apps (Nova's
    /// own `Uninstall` refuses them so it never desyncs opkg's status db).
    ///
    /// The apps the package took off the registry lose what they kept too,
    /// their own data and their Gecko profile (`nova::forget_app_state`), so
    /// a package that later claims one of their ids starts empty. A package
    /// removed with `opkg remove` directly keeps both, the way `apt remove`
    /// keeps configuration.
    pub async fn remove(&self, package: &str, purge: bool) -> Result<Value> {
        if package.trim().is_empty() {
            return Err(anyhow!("package name is required"));
        }
        Self::require_opkg()?;
        let before = Nova::global().app_ids_in_tier(AppTier::Opkg);

        self.progress(package, "removing", "Running opkg remove");
        let mut cmd = tokio::process::Command::new("opkg");
        cmd.arg("remove").arg(package);
        if purge {
            cmd.arg("--force-removal-of-dependent-packages");
        }
        let output = cmd
            .output()
            .await
            .with_context(|| format!("invoke opkg remove {package}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let msg = if stderr.is_empty() {
                format!("opkg remove exited with {}", output.status)
            } else {
                stderr
            };
            self.emit(
                "RemoveFailed",
                json!({ "package": package, "error": msg }),
            );
            return Err(anyhow!(msg));
        }

        let registered = Nova::global().reload().unwrap_or(0);
        // A failed reload leaves the registry as it was, so nothing looks
        // removed and no data goes.
        let after = Nova::global().app_ids_in_tier(AppTier::Opkg);
        let gone: Vec<String> = before.into_iter().filter(|id| !after.contains(id)).collect();
        // A removed app may still be running. Wait for each to exit, off the
        // runtime's threads, before its profile goes.
        let data_removed = tokio::task::spawn_blocking(move || {
            gone.into_iter()
                .filter(|id| {
                    AppHost::global().close_and_wait(id, APP_EXIT_LIMIT);
                    !forget_app_state(id).is_empty()
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let result = json!({
            "package": package,
            "registeredApps": registered,
            "dataRemoved": data_removed,
        });
        self.emit("RemoveCompleted", result.clone());
        tracing::info!("installer: removed '{}'", package);
        Ok(result)
    }

    /// `installer.ListInstalled()` — packages opkg knows about.
    pub fn list_installed(&self) -> Result<Value> {
        let versions = self.installed_versions()?;
        let mut packages: Vec<Value> = versions
            .into_iter()
            .map(|(name, version)| {
                let registered = Nova::global().get_info(&name).is_ok();
                json!({ "package": name, "version": version, "registered": registered })
            })
            .collect();
        packages.sort_by(|a, b| {
            a["package"]
                .as_str()
                .unwrap_or("")
                .cmp(b["package"].as_str().unwrap_or(""))
        });
        let count = packages.len();
        Ok(json!({ "packages": packages, "count": count }))
    }

    /// `opkg list-installed` → {package: version}. An absent opkg yields an
    /// empty map rather than an error so discovery still works on a dev box.
    fn installed_versions(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut map = std::collections::HashMap::new();
        if which_opkg().is_none() {
            return Ok(map);
        }
        let output = std::process::Command::new("opkg")
            .arg("list-installed")
            .output()
            .context("invoke opkg list-installed")?;
        if !output.status.success() {
            return Ok(map);
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            // Format: "<name> - <version>"
            let mut parts = line.splitn(2, " - ");
            if let (Some(name), Some(version)) = (parts.next(), parts.next()) {
                map.insert(name.trim().to_string(), version.trim().to_string());
            }
        }
        Ok(map)
    }

    fn require_opkg() -> Result<()> {
        which_opkg()
            .map(|_| ())
            .ok_or_else(|| anyhow!("opkg is not available on this system"))
    }

    /// Reject anything that isn't a readable `.ipk` under an allowed root.
    /// Installing runs maintainer scripts as root, so the caller does not
    /// get to name an arbitrary path.
    fn validate_package_path(package_path: &str) -> Result<PathBuf> {
        if package_path.trim().is_empty() {
            return Err(anyhow!("packagePath is required"));
        }
        let path = Path::new(package_path);
        if !path.is_absolute() {
            return Err(anyhow!("packagePath must be absolute: {}", package_path));
        }
        // Canonicalize before the prefix check so `/tmp/../etc/x.ipk`
        // can't slip past it.
        let path = path
            .canonicalize()
            .with_context(|| format!("resolve {}", package_path))?;
        if !path.is_file() {
            return Err(anyhow!("not a file: {}", path.display()));
        }
        if path.extension().and_then(|s| s.to_str()) != Some("ipk") {
            return Err(anyhow!("not an .ipk package: {}", path.display()));
        }
        let allowed = ALLOWED_INSTALL_ROOTS
            .iter()
            .any(|root| path.starts_with(root));
        if !allowed {
            return Err(anyhow!(
                "packages may only be installed from {} (got {})",
                ALLOWED_INSTALL_ROOTS.join(", "),
                path.display()
            ));
        }
        Ok(path)
    }
}

fn which_opkg() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("opkg"))
            .find(|p| p.is_file())
    })
}

/// Parse a Debian-style `control` file into key→value pairs. Continuation
/// lines (leading whitespace) are appended to the previous field.
fn parse_control(raw: &str) -> std::collections::HashMap<String, String> {
    let mut fields = std::collections::HashMap::new();
    let mut last_key: Option<String> = None;
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(key) = &last_key {
                let entry = fields.entry(key.clone()).or_insert_with(String::new);
                entry.push(' ');
                entry.push_str(line.trim());
            }
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            fields.insert(key.clone(), value.trim().to_string());
            last_key = Some(key);
        }
    }
    fields
}

/// Find `name` anywhere under `dir`, bounded to `max_depth` levels.
fn find_file(dir: &Path, name: &str, max_depth: usize) -> Option<PathBuf> {
    if max_depth == 0 {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_name().and_then(|s| s.to_str()) == Some(name) {
            return Some(path);
        }
        if path.is_dir() {
            subdirs.push(path);
        }
    }
    subdirs
        .into_iter()
        .find_map(|d| find_file(&d, name, max_depth - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_control_fields() {
        let raw = "Package: notes\nVersion: 0.1.0\nArchitecture: all\n\
                   Description: Note-taking app\n";
        let f = parse_control(raw);
        assert_eq!(f.get("Package").unwrap(), "notes");
        assert_eq!(f.get("Version").unwrap(), "0.1.0");
        assert_eq!(f.get("Architecture").unwrap(), "all");
    }

    #[test]
    fn parses_control_continuation_lines() {
        let raw = "Package: notes\nDescription: First line\n  continued here\n";
        let f = parse_control(raw);
        assert_eq!(f.get("Description").unwrap(), "First line continued here");
    }

    #[test]
    fn rejects_relative_path() {
        let err = InstallerService::validate_package_path("notes.ipk").unwrap_err();
        assert!(err.to_string().contains("must be absolute"));
    }

    #[test]
    fn rejects_empty_path() {
        let err = InstallerService::validate_package_path("").unwrap_err();
        assert!(err.to_string().contains("required"));
    }

    #[test]
    fn rejects_non_ipk_extension() {
        // /etc/hostname exists on any Linux host and is not an .ipk.
        let err = InstallerService::validate_package_path("/etc/hostname").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not an .ipk") || msg.contains("may only be installed"),
            "unexpected error: {msg}"
        );
    }

    /// Build a real `.ipk` (the exact layout Vroom's built-in writer emits:
    /// debian-binary + control.tar.gz + data.tar.gz) under /tmp and inspect
    /// it. Covers the whole metadata path — outer untar, control parse, and
    /// pulling `appinfo.json` back out of the data archive.
    #[test]
    fn inspects_a_real_ipk() {
        let root = std::env::temp_dir().join(format!("pyxis-ipk-fixture-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let stage = root.join("stage");
        let app_dir = stage.join("usr/pyxis/applications/fixtureapp");
        fs::create_dir_all(&app_dir).unwrap();
        fs::create_dir_all(stage.join("CONTROL")).unwrap();
        fs::write(
            stage.join("CONTROL/control"),
            "Package: fixtureapp\nVersion: 2.1.0\nArchitecture: all\n\
             Section: applications\nPriority: optional\n\
             Maintainer: Pyxis Project <dev@pyxis.local>\n\
             Description: Fixture app\n",
        )
        .unwrap();
        fs::write(
            app_dir.join("appinfo.json"),
            r#"{"id":"fixtureapp","version":"2.1.0","vendor":"Pyxis Project",
                "title":"Fixture App","type":"web","main":"index.html",
                "icon":"icon.png","requiredPermissions":["filesystem.read"]}"#,
        )
        .unwrap();
        fs::write(app_dir.join("index.html"), "<html></html>").unwrap();

        let run = |args: &[&str], cwd: &Path| {
            let st = std::process::Command::new("tar")
                .args(args)
                .current_dir(cwd)
                .status()
                .expect("tar");
            assert!(st.success(), "tar {args:?} failed");
        };
        // Both member archives must land in `root`, next to debian-binary.
        run(&["-czf", "../../control.tar.gz", "."], &stage.join("CONTROL"));
        run(&["-czf", "../data.tar.gz", "usr"], &stage);
        fs::write(root.join("debian-binary"), b"2.0\n").unwrap();
        run(
            &[
                "-czf",
                "fixtureapp_2.1.0_all.ipk",
                "debian-binary",
                "control.tar.gz",
                "data.tar.gz",
            ],
            &root,
        );

        let ipk = root.join("fixtureapp_2.1.0_all.ipk");
        let info = InstallerService::inspect_path(&ipk).expect("inspect");
        assert_eq!(info.package, "fixtureapp");
        assert_eq!(info.version, "2.1.0");
        assert_eq!(info.architecture, "all");
        assert_eq!(info.description, "Fixture app");
        assert_eq!(info.title.as_deref(), Some("Fixture App"));
        assert_eq!(info.app_type.as_deref(), Some("web"));
        assert_eq!(info.required_permissions, vec!["filesystem.read"]);
        // /tmp is an allowed install root, so the same path must validate.
        InstallerService::validate_package_path(ipk.to_str().unwrap()).expect("path allowed");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_package_outside_allowed_roots() {
        // Build a real .ipk-named file outside the allowed roots.
        let dir = std::env::current_dir().unwrap().join("target");
        if !dir.is_dir() {
            return; // no target dir in this environment; nothing to assert
        }
        let path = dir.join("installer-root-check.ipk");
        if fs::write(&path, b"x").is_err() {
            return;
        }
        let err = InstallerService::validate_package_path(path.to_str().unwrap())
            .unwrap_err()
            .to_string();
        let _ = fs::remove_file(&path);
        assert!(err.contains("may only be installed"), "unexpected: {err}");
    }
}
