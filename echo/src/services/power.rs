//! Power Management Service
//!
//! Provides power control and battery status information.
//!
//! Every action here targets **BusyBox init**, which is PID 1 on Pyxis
//! (`scripts/create-rootfs.sh`). There is no systemd and no `util-linux`
//! `shutdown` that init would understand, so the previous implementation --
//! `shutdown -h` / `systemctl suspend` -- could not succeed on any Pyxis
//! machine. `busybox poweroff`/`reboot` signal BusyBox init directly
//! (SIGUSR2 / SIGTERM per its signal table), which is the supported path.

use serde_json::{json, Value};
use anyhow::{Result, anyhow};
use std::process::Command;

/// Kernel entry point for suspend-to-RAM.
const POWER_STATE: &str = "/sys/power/state";

pub struct PowerService;

impl PowerService {
    pub fn shutdown(delay: Option<u64>) -> Result<Value> {
        busybox_halt("poweroff", delay)?;

        Ok(json!({
            "success": true,
            "action": "shutdown",
            "delay": delay
        }))
    }

    pub fn reboot(delay: Option<u64>) -> Result<Value> {
        busybox_halt("reboot", delay)?;

        Ok(json!({
            "success": true,
            "action": "reboot",
            "delay": delay
        }))
    }

    /// Suspend-to-RAM via `/sys/power/state`.
    ///
    /// Returns a structured `supported: false` rather than a spurious success
    /// when the kernel offers no suitable sleep state -- which is the common
    /// case under QEMU. A caller needs to be able to tell "asleep" from
    /// "silently did nothing", and the old `systemctl suspend` shell-out
    /// reported success either way.
    pub fn sleep() -> Result<Value> {
        let states = std::fs::read_to_string(POWER_STATE).map_err(|e| {
            anyhow!("Suspend is unsupported: {} is unreadable ({})", POWER_STATE, e)
        })?;

        // "mem" is suspend-to-RAM; "freeze" (suspend-to-idle) is the fallback
        // every kernel with CONFIG_SUSPEND offers even without platform support.
        let target = ["mem", "freeze"]
            .into_iter()
            .find(|state| states.split_whitespace().any(|s| s == *state));

        let Some(target) = target else {
            return Ok(json!({
                "success": false,
                "supported": false,
                "action": "sleep",
                "error": format!(
                    "no supported sleep state in {} (available: {})",
                    POWER_STATE,
                    states.trim()
                )
            }));
        };

        std::fs::write(POWER_STATE, target)
            .map_err(|e| anyhow!("Failed to enter {} suspend: {}", target, e))?;

        Ok(json!({
            "success": true,
            "supported": true,
            "action": "sleep",
            "state": target
        }))
    }

    pub fn wake() -> Result<Value> {
        // Wake is implicit - send a signal or activate the system
        // This might require hardware-specific implementation
        Ok(json!({
            "success": true,
            "action": "wake"
        }))
    }

    /// Battery level and charge state, or `present: false` on a machine with
    /// no battery (a desktop, or the QEMU target, where `/sys/class/power_supply`
    /// is typically empty).
    ///
    /// "No battery" is reported explicitly rather than as a plausible-looking
    /// number: this used to fall back to a hardcoded 50% and a constant 40°C
    /// temperature, so the shell drew a half-full battery icon on hardware
    /// that has none.
    pub fn get_battery_status() -> Result<Value> {
        // `acpi` is not in the Pyxis image; this stays as a first choice for
        // hosts that do have it, and the sysfs path below is the real one.
        if let Ok(output) = Command::new("acpi").arg("-b").output() {
            let output_str = String::from_utf8_lossy(&output.stdout);
            if let Some((level, charging)) = parse_acpi_battery(&output_str) {
                return Ok(json!({
                    "present": true,
                    "level": level,
                    "charging": charging
                }));
            }
        }

        let Some((level, charging)) = read_battery_from_sysfs() else {
            return Ok(json!({
                "present": false,
                "level": Value::Null,
                "charging": false
            }));
        };

        Ok(json!({
            "present": true,
            "level": level,
            "charging": charging
        }))
    }

    pub fn set_screen_brightness(brightness: u32) -> Result<Value> {
        if brightness > 100 {
            return Err(anyhow!("Brightness must be 0-100"));
        }

        // Previously this piped nothing into `tee` (no stdin was ever
        // attached) and then reported success if the *process spawned* --
        // so it always claimed to work and never changed the backlight.
        let Some(backlight) = backlight_device() else {
            return Ok(json!({
                "success": false,
                "supported": false,
                "brightness": brightness,
                "error": "no backlight device under /sys/class/backlight"
            }));
        };

        let max = std::fs::read_to_string(backlight.join("max_brightness"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .ok_or_else(|| anyhow!("Backlight {} reports no max_brightness", backlight.display()))?;

        let scaled = (brightness as f32 / 100.0 * max as f32).round() as u32;
        std::fs::write(backlight.join("brightness"), scaled.to_string())
            .map_err(|e| anyhow!("Failed to set brightness on {}: {}", backlight.display(), e))?;

        Ok(json!({
            "success": true,
            "supported": true,
            "brightness": brightness
        }))
    }

    pub fn get_screen_brightness() -> Result<Value> {
        let Some(backlight) = backlight_device() else {
            return Ok(json!({
                "brightness": Value::Null,
                "supported": false
            }));
        };

        let read = |name: &str| {
            std::fs::read_to_string(backlight.join(name))
                .ok()
                .and_then(|s| s.trim().parse::<f32>().ok())
        };

        let brightness = match (read("brightness"), read("max_brightness")) {
            (Some(current), Some(max)) if max > 0.0 => ((current / max) * 100.0).round() as u32,
            _ => {
                return Ok(json!({
                    "brightness": Value::Null,
                    "supported": false
                }))
            }
        };

        Ok(json!({
            "brightness": brightness,
            "supported": true
        }))
    }

    pub fn set_screen_timeout(timeout: u64) -> Result<Value> {
        // Set using xset if X11 is available
        Command::new("xset")
            .args(&["dpms", "s", &timeout.to_string()])
            .output()
            .ok();

        Ok(json!({
            "success": true,
            "timeout": timeout
        }))
    }

    pub fn get_screen_timeout() -> Result<Value> {
        // This varies by display server; return a default for now
        Ok(json!({
            "timeout": 300
        }))
    }
}

/// Ask BusyBox init to halt or reboot.
///
/// `action` is `poweroff` or `reboot`. BusyBox's applets take `-d SECS` for a
/// delay, so the caller's delay is honoured rather than dropped.
///
/// Invoked as `busybox <action>` explicitly: the bare `poweroff`/`reboot`
/// names may not be symlinked into PATH in a minimal rootfs, but `busybox`
/// always is.
fn busybox_halt(action: &str, delay: Option<u64>) -> Result<()> {
    let mut command = Command::new("busybox");
    command.arg(action);
    if let Some(delay) = delay.filter(|d| *d > 0) {
        command.args(["-d", &delay.to_string()]);
    }

    let output = command
        .output()
        .map_err(|e| anyhow!("Failed to run `busybox {}`: {}", action, e))?;

    if !output.status.success() {
        return Err(anyhow!(
            "`busybox {}` failed: {}",
            action,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(())
}

/// First backlight device the kernel exposes.
///
/// Not hardcoded to `intel_backlight`: that path exists only on Intel
/// integrated graphics, so on anything else the old code silently read and
/// wrote nothing.
fn backlight_device() -> Option<std::path::PathBuf> {
    std::fs::read_dir("/sys/class/backlight")
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| path.join("max_brightness").exists())
}

/// Parse `acpi -b` output, or `None` if it reported no battery.
fn parse_acpi_battery(output: &str) -> Option<(u32, bool)> {
    for line in output.lines() {
        if line.contains("Battery") {
            // Parse: "Battery 0: Discharging, 85%, 02:15:00 remaining"
            let level = line
                .split(',')
                .nth(1)
                .and_then(|percent_part| percent_part.trim().split('%').next())
                .and_then(|percent_str| percent_str.parse().ok())?;

            return Some((level, line.contains("Charging") || line.contains("Full")));
        }
    }

    None
}

/// Read the first real battery under `/sys/class/power_supply`, or `None` if
/// the machine has none.
///
/// Scans rather than hardcoding `BAT0`: the name is firmware-assigned and is
/// routinely `BAT1` (or `CMB0` on some laptops). Entries are filtered by
/// `type == Battery` so an AC adapter -- which has no `capacity` -- isn't
/// mistaken for one.
fn read_battery_from_sysfs() -> Option<(u32, bool)> {
    let supplies = std::fs::read_dir("/sys/class/power_supply").ok()?;

    for entry in supplies.filter_map(|entry| entry.ok()) {
        let path = entry.path();

        let is_battery = std::fs::read_to_string(path.join("type"))
            .map(|t| t.trim() == "Battery")
            .unwrap_or(false);
        if !is_battery {
            continue;
        }

        let Some(capacity) = std::fs::read_to_string(path.join("capacity"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
        else {
            continue;
        };

        let status = std::fs::read_to_string(path.join("status")).unwrap_or_default();
        let charging = status.contains("Charging") || status.contains("Full");

        return Some((capacity, charging));
    }

    None
}
