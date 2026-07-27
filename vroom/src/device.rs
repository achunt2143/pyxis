//! Device profiles and the remote install protocol (spec §17).
//!
//! Profiles live in `~/.config/pyxis/devices.json`. Install sequence:
//! copy `.ipk` to target → `opkg install` → `pyxis-appctl rescan` →
//! optionally launch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{PyxisError, Result};

fn default_port() -> u16 {
    22
}

fn default_install_path() -> String {
    "/tmp".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceProfile {
    pub host: String,
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(rename = "installPath", default = "default_install_path")]
    pub install_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicesFile {
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceProfile>,
}

pub fn devices_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("pyxis/devices.json")
}

pub fn load_devices() -> Result<DevicesFile> {
    let path = devices_path();
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        PyxisError::Device(format!(
            "cannot read device profiles at {}: {e}\n\
             create it per docs/pyxis-sdk-pdk-spec.md §17.1",
            path.display()
        ))
    })?;
    serde_json::from_str(&raw)
        .map_err(|e| PyxisError::Device(format!("invalid {}: {e}", path.display())))
}

pub fn resolve_device(name: Option<&str>) -> Result<DeviceProfile> {
    let file = load_devices()?;
    let key = match name {
        Some(n) => n.to_string(),
        None => file
            .default
            .clone()
            .ok_or_else(|| PyxisError::Device("no --device given and no default device configured".into()))?,
    };
    file.devices
        .get(&key)
        .cloned()
        .ok_or_else(|| PyxisError::Device(format!("unknown device profile `{key}`")))
}

fn ssh(device: &DeviceProfile, remote_cmd: &str) -> Result<()> {
    let target = format!("{}@{}", device.user, device.host);
    eprintln!("[pyxis] $ ssh {target} {remote_cmd}");
    let status = Command::new("ssh")
        .args(["-p", &device.port.to_string(), &target, remote_cmd])
        .status()
        .map_err(|e| PyxisError::Device(format!("cannot run ssh: {e}")))?;
    if !status.success() {
        return Err(PyxisError::Device(format!("ssh `{remote_cmd}` exited with {status}")));
    }
    Ok(())
}

/// Spec §17.2: copy, install, rescan.
pub fn install(device: &DeviceProfile, artifact: &Path) -> Result<()> {
    let file_name = artifact
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| PyxisError::Device("artifact has no filename".into()))?;
    let target = format!(
        "{}@{}:{}/{}",
        device.user, device.host, device.install_path, file_name
    );
    eprintln!("[pyxis] $ scp {} {target}", artifact.display());
    let status = Command::new("scp")
        .args(["-P", &device.port.to_string()])
        .arg(artifact)
        .arg(&target)
        .status()
        .map_err(|e| PyxisError::Device(format!("cannot run scp: {e}")))?;
    if !status.success() {
        return Err(PyxisError::Device(format!("scp exited with {status}")));
    }

    let remote_pkg = format!("{}/{}", device.install_path, file_name);
    ssh(device, &format!("opkg install --force-reinstall {remote_pkg}"))?;
    // Registry refresh is best-effort: pyxis-appctl ships with the platform,
    // not with every development image.
    let _ = ssh(
        device,
        "command -v pyxis-appctl >/dev/null 2>&1 && pyxis-appctl rescan || true",
    );
    Ok(())
}

/// Optional step 4 of the install sequence: launch the app if the target
/// has `pyxis-appctl`.
pub fn launch(device: &DeviceProfile, app_id: &str) -> Result<()> {
    ssh(
        device,
        &format!("command -v pyxis-appctl >/dev/null 2>&1 && pyxis-appctl launch {app_id}"),
    )
}
