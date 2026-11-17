//! Device Information Service
//! 
//! Provides system and hardware information using standard Linux utilities.

use serde_json::{json, Value};
use anyhow::{Result, anyhow};
use std::process::Command;

pub struct DeviceInfoService;

impl DeviceInfoService {
    pub fn get_os() -> Result<Value> {
        let uname_output = Command::new("uname")
            .arg("-s")
            .output()
            .map_err(|e| anyhow!("Failed to get OS: {}", e))?;
        
        let name = String::from_utf8_lossy(&uname_output.stdout).trim().to_string();

        Ok(json!({
            "name": name,
            "version": get_os_version()?,
            "build": get_build_info()?
        }))
    }

    pub fn get_kernel() -> Result<Value> {
        let release = Command::new("uname").arg("-r").output()
            .map_err(|e| anyhow!("Failed to get kernel release: {}", e))?;
        let version = Command::new("uname").arg("-v").output()
            .map_err(|e| anyhow!("Failed to get kernel version: {}", e))?;
        let name = Command::new("uname").arg("-s").output()
            .map_err(|e| anyhow!("Failed to get kernel name: {}", e))?;

        Ok(json!({
            "name": String::from_utf8_lossy(&name.stdout).trim().to_string(),
            "release": String::from_utf8_lossy(&release.stdout).trim().to_string(),
            "version": String::from_utf8_lossy(&version.stdout).trim().to_string()
        }))
    }

    pub fn get_model() -> Result<Value> {
        // Try /sys/class/dmi/id/product_name
        if let Ok(model) = std::fs::read_to_string("/sys/class/dmi/id/product_name") {
            return Ok(json!({
                "name": model.trim().to_string()
            }));
        }

        Ok(json!({
            "name": "Unknown"
        }))
    }

    pub fn get_manufacturer() -> Result<Value> {
        if let Ok(mfg) = std::fs::read_to_string("/sys/class/dmi/id/sys_vendor") {
            return Ok(json!({
                "name": mfg.trim().to_string()
            }));
        }

        Ok(json!({
            "name": "Unknown"
        }))
    }

    pub fn get_serial() -> Result<Value> {
        if let Ok(serial) = std::fs::read_to_string("/sys/class/dmi/id/product_serial") {
            return Ok(json!({
                "serial": serial.trim().to_string()
            }));
        }

        Ok(json!({
            "serial": "N/A"
        }))
    }

    pub fn get_hardware() -> Result<Value> {
        if let Ok(hardware) = std::fs::read_to_string("/sys/class/dmi/id/board_name") {
            return Ok(json!({
                "hardware": hardware.trim().to_string(),
                "revision": get_hardware_revision()?
            }));
        }

        Ok(json!({
            "hardware": "Unknown",
            "revision": "N/A"
        }))
    }

    pub fn get_uuid() -> Result<Value> {
        // Use /etc/machine-id or /proc/sys/kernel/random/uuid
        if let Ok(uuid) = std::fs::read_to_string("/etc/machine-id") {
            return Ok(json!({
                "uuid": uuid.trim().to_string()
            }));
        }

        // Fallback to generating one
        let output = Command::new("uuidgen")
            .output()
            .map_err(|_| anyhow!("Failed to generate UUID"))?;

        Ok(json!({
            "uuid": String::from_utf8_lossy(&output.stdout).trim().to_string()
        }))
    }

    pub fn get_hostname() -> Result<Value> {
        let output = Command::new("hostname")
            .output()
            .map_err(|e| anyhow!("Failed to get hostname: {}", e))?;
        
        let hostname = String::from_utf8_lossy(&output.stdout).trim().to_string();

        Ok(json!({
            "hostname": hostname,
            "domain": get_domain()?
        }))
    }

    pub fn get_memory_info() -> Result<Value> {
        let meminfo = std::fs::read_to_string("/proc/meminfo")
            .map_err(|e| anyhow!("Failed to read /proc/meminfo: {}", e))?;

        let mut total = 0u64;
        let mut available = 0u64;

        for line in meminfo.lines() {
            if line.starts_with("MemTotal:") {
                if let Some(val) = parse_meminfo_line(line) {
                    total = val * 1024;
                }
            }
            if line.starts_with("MemAvailable:") {
                if let Some(val) = parse_meminfo_line(line) {
                    available = val * 1024;
                }
            }
        }

        Ok(json!({
            "total": total,
            "available": available,
            "used": total - available
        }))
    }

    pub fn get_storage_info(mount: Option<&str>) -> Result<Value> {
        let mount = mount.unwrap_or("/");
        
        let output = Command::new("df")
            .arg("-B1")
            .arg(mount)
            .output()
            .map_err(|e| anyhow!("Failed to get storage info: {}", e))?;

        let df_output = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = df_output.lines().collect();

        if lines.len() < 2 {
            return Err(anyhow!("Invalid df output"));
        }

        let parts: Vec<&str> = lines[1].split_whitespace().collect();
        if parts.len() < 4 {
            return Err(anyhow!("Invalid df format"));
        }

        let total: u64 = parts[1].parse().unwrap_or(0);
        let used: u64 = parts[2].parse().unwrap_or(0);
        let available: u64 = parts[3].parse().unwrap_or(0);

        Ok(json!({
            "total": total,
            "used": used,
            "available": available,
            "mount": mount
        }))
    }

    pub fn get_cpu_info() -> Result<Value> {
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo")
            .map_err(|e| anyhow!("Failed to read /proc/cpuinfo: {}", e))?;

        let mut model = String::new();
        let mut frequency = String::new();

        for line in cpuinfo.lines() {
            if line.starts_with("model name") && model.is_empty() {
                if let Some(val) = line.split(':').nth(1) {
                    model = val.trim().to_string();
                }
            }
            if line.starts_with("cpu MHz") && frequency.is_empty() {
                if let Some(val) = line.split(':').nth(1) {
                    frequency = format!("{} MHz", val.trim());
                }
            }
        }

        // Count should be divided by number of cores per processor, but this is simplified
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1);

        Ok(json!({
            "count": cpus,
            "model": model,
            "frequency": frequency
        }))
    }
}

fn get_os_version() -> Result<String> {
    if let Ok(version) = std::fs::read_to_string("/etc/os-release") {
        for line in version.lines() {
            if line.starts_with("VERSION_ID=") {
                return Ok(line.split('=').nth(1).unwrap_or("").trim_matches('"').to_string());
            }
        }
    }
    Ok("Unknown".to_string())
}

fn get_build_info() -> Result<String> {
    if let Ok(build) = std::fs::read_to_string("/etc/os-release") {
        for line in build.lines() {
            if line.starts_with("BUILD_ID=") {
                return Ok(line.split('=').nth(1).unwrap_or("").trim_matches('"').to_string());
            }
        }
    }
    Ok("Unknown".to_string())
}

fn get_hardware_revision() -> Result<String> {
    if let Ok(revision) = std::fs::read_to_string("/sys/class/dmi/id/board_version") {
        return Ok(revision.trim().to_string());
    }
    Ok("N/A".to_string())
}

fn get_domain() -> Result<String> {
    let output = Command::new("dnsdomainname")
        .output();

    match output {
        Ok(out) => Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        Err(_) => Ok("localdomain".to_string()),
    }
}

fn parse_meminfo_line(line: &str) -> Option<u64> {
    line.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
}
