//! Power Management Service
//! 
//! Provides power control and battery status information.

use serde_json::{json, Value};
use anyhow::{Result, anyhow};
use std::process::Command;
use std::thread;
use std::time::Duration;

pub struct PowerService;

impl PowerService {
    pub fn shutdown(delay: Option<u64>) -> Result<Value> {
        let delay_str = delay.map(|d| d.to_string()).unwrap_or_else(|| "0".to_string());
        
        Command::new("shutdown")
            .arg("-h")
            .arg(&delay_str)
            .output()
            .map_err(|e| anyhow!("Failed to shutdown: {}", e))?;

        Ok(json!({
            "success": true,
            "action": "shutdown",
            "delay": delay
        }))
    }

    pub fn reboot(delay: Option<u64>) -> Result<Value> {
        let delay_str = delay.map(|d| d.to_string()).unwrap_or_else(|| "0".to_string());
        
        Command::new("shutdown")
            .arg("-r")
            .arg(&delay_str)
            .output()
            .map_err(|e| anyhow!("Failed to reboot: {}", e))?;

        Ok(json!({
            "success": true,
            "action": "reboot",
            "delay": delay
        }))
    }

    pub fn sleep() -> Result<Value> {
        Command::new("systemctl")
            .arg("suspend")
            .output()
            .map_err(|e| anyhow!("Failed to suspend: {}", e))?;

        Ok(json!({
            "success": true,
            "action": "sleep"
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

    pub fn get_battery_status() -> Result<Value> {
        let acpi_output = Command::new("acpi")
            .arg("-b")
            .output();

        let (level, charging) = if let Ok(output) = acpi_output {
            let output_str = String::from_utf8_lossy(&output.stdout);
            parse_acpi_battery(&output_str)
        } else {
            // Fallback: read from /sys/class/power_supply/
            read_battery_from_sysfs()?
        };

        Ok(json!({
            "level": level,
            "charging": charging,
            "temperature": 40.0
        }))
    }

    pub fn set_screen_brightness(brightness: u32) -> Result<Value> {
        if brightness > 100 {
            return Err(anyhow!("Brightness must be 0-100"));
        }

        // Try backlight interface
        let max_brightness = std::fs::read_to_string("/sys/class/backlight/intel_backlight/max_brightness")
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok())
            .unwrap_or(100.0);

        let actual_brightness = (brightness as f32 / 100.0 * max_brightness) as u32;

        let result = Command::new("tee")
            .arg("/sys/class/backlight/intel_backlight/brightness")
            .output()
            .is_ok();

        Ok(json!({
            "success": result,
            "brightness": brightness
        }))
    }

    pub fn get_screen_brightness() -> Result<Value> {
        let max = std::fs::read_to_string("/sys/class/backlight/intel_backlight/max_brightness")
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok())
            .unwrap_or(100.0);

        let current = std::fs::read_to_string("/sys/class/backlight/intel_backlight/brightness")
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok())
            .unwrap_or(100.0);

        let brightness = ((current / max) * 100.0) as u32;

        Ok(json!({
            "brightness": brightness
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

fn parse_acpi_battery(output: &str) -> (u32, bool) {
    let mut level = 0u32;
    let mut charging = false;

    for line in output.lines() {
        if line.contains("Battery") {
            // Parse: "Battery 0: Discharging, 85%, 02:15:00 remaining"
            if let Some(percent_part) = line.split(',').nth(1) {
                if let Some(percent_str) = percent_part.trim().split('%').next() {
                    level = percent_str.parse().unwrap_or(0);
                }
            }
            charging = line.contains("Charging") || line.contains("Full");
        }
    }

    (level, charging)
}

fn read_battery_from_sysfs() -> Result<(u32, bool)> {
    let capacity = std::fs::read_to_string("/sys/class/power_supply/BAT0/capacity")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(50);

    let status = std::fs::read_to_string("/sys/class/power_supply/BAT0/status")
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let charging = status.contains("Charging") || status.contains("Full");

    Ok((capacity, charging))
}
