//! Network Service
//! 
//! Provides network interface and connectivity information.

use serde_json::{json, Value};
use anyhow::{Result, anyhow};
use std::process::Command;

pub struct NetworkService;

impl NetworkService {
    pub fn get_status() -> Result<Value> {
        let output = Command::new("nmcli")
            .args(&["-t", "-f", "ACTIVE", "con", "show", "--active"])
            .output();

        let connected = output.is_ok() && !output.unwrap().stdout.is_empty();

        let net_type = if connected {
            get_connection_type()?
        } else {
            "none".to_string()
        };
        let ssid = if connected { get_active_connection_name() } else { String::new() };

        Ok(json!({
            "connected": connected,
            "type": net_type,
            "ssid": ssid
        }))
    }

    pub fn get_type() -> Result<Value> {
        Ok(json!({
            "type": get_connection_type()?
        }))
    }

    pub fn get_interfaces() -> Result<Value> {
        let output = Command::new("ip")
            .arg("link")
            .arg("show")
            .output()
            .map_err(|e| anyhow!("Failed to list interfaces: {}", e))?;

        let mut interfaces = Vec::new();
        let output_str = String::from_utf8_lossy(&output.stdout);

        for line in output_str.lines() {
            if line.contains(":") && !line.starts_with(" ") {
                if let Some(name) = line.split(':').nth(1).map(|s| s.trim()) {
                    if let Ok(info) = get_interface_info(name) {
                        interfaces.push(info);
                    }
                }
            }
        }

        Ok(json!({
            "interfaces": interfaces,
            "count": interfaces.len()
        }))
    }

    pub fn get_ip_address(interface: Option<&str>) -> Result<Value> {
        let iface = interface.unwrap_or("eth0");

        let output = Command::new("ip")
            .args(&["addr", "show", iface])
            .output()
            .map_err(|e| anyhow!("Failed to get IP address: {}", e))?;

        let output_str = String::from_utf8_lossy(&output.stdout);
        let mut ipv4 = String::new();
        let mut ipv6 = String::new();

        for line in output_str.lines() {
            if line.contains("inet ") && !line.contains("inet6") {
                if let Some(addr) = line.trim().split_whitespace().nth(1) {
                    ipv4 = addr.split('/').next().unwrap_or("").to_string();
                }
            }
            if line.contains("inet6") {
                if let Some(addr) = line.trim().split_whitespace().nth(1) {
                    ipv6 = addr.split('/').next().unwrap_or("").to_string();
                }
            }
        }

        Ok(json!({
            "interface": iface,
            "ipv4": ipv4,
            "ipv6": ipv6
        }))
    }

    pub fn get_gateway() -> Result<Value> {
        let output = Command::new("ip")
            .arg("route")
            .output()
            .map_err(|e| anyhow!("Failed to get gateway: {}", e))?;

        let output_str = String::from_utf8_lossy(&output.stdout);
        let mut ipv4 = String::new();

        for line in output_str.lines() {
            if line.starts_with("default") {
                if let Some(gateway) = line.split_whitespace().nth(2) {
                    ipv4 = gateway.to_string();
                    break;
                }
            }
        }

        Ok(json!({
            "ipv4": ipv4,
            "ipv6": ""
        }))
    }

    pub fn get_dns() -> Result<Value> {
        let mut nameservers = Vec::new();

        // Try /etc/resolv.conf
        if let Ok(content) = std::fs::read_to_string("/etc/resolv.conf") {
            for line in content.lines() {
                if line.starts_with("nameserver") {
                    if let Some(ns) = line.split_whitespace().nth(1) {
                        nameservers.push(ns.to_string());
                    }
                }
            }
        }

        Ok(json!({
            "nameservers": nameservers
        }))
    }
}

/// Real network name for the active connection: the WiFi SSID via
/// `iwgetid -r` when on wireless, falling back to NetworkManager's active
/// connection display name otherwise (e.g. a wired connection's profile
/// name) — real values were missing entirely before (the JSON response had
/// no `ssid` field at all, though the shell's `NetworkStatus` type and
/// `TopSystemBar.tsx` both expect one), which is what made Bloom's status
/// bar fall back to displaying nothing/mock data for the network name even
/// once the method-name mismatch (see useEcho.ts) was fixed.
fn get_active_connection_name() -> String {
    if let Ok(output) = Command::new("iwgetid").arg("-r").output() {
        let ssid = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !ssid.is_empty() {
            return ssid;
        }
    }
    if let Ok(output) = Command::new("nmcli")
        .args(&["-t", "-f", "NAME", "con", "show", "--active"])
        .output()
    {
        if let Some(name) = String::from_utf8_lossy(&output.stdout).lines().next() {
            if !name.trim().is_empty() {
                return name.trim().to_string();
            }
        }
    }
    String::new()
}

fn get_connection_type() -> Result<String> {
    // Check for WiFi
    let wifi_check = Command::new("nmcli").args(&["device", "status"]).output();
    
    if let Ok(output) = wifi_check {
        let out_str = String::from_utf8_lossy(&output.stdout);
        if out_str.contains("wifi") && out_str.contains("connected") {
            return Ok("wifi".to_string());
        }
        if out_str.contains("ethernet") && out_str.contains("connected") {
            return Ok("ethernet".to_string());
        }
    }

    Ok("unknown".to_string())
}

fn get_interface_info(name: &str) -> Result<Value> {
    let output = Command::new("ip")
        .args(&["addr", "show", name])
        .output()?;

    let output_str = String::from_utf8_lossy(&output.stdout);
    let mut ipv4 = String::new();
    let mut ipv6 = String::new();
    let mut mac_address = String::new();
    let mut is_up = false;

    for line in output_str.lines() {
        if line.contains("UP") && line.contains("RUNNING") {
            is_up = true;
        }
        if line.contains("link/ether") {
            if let Some(mac) = line.split_whitespace().nth(1) {
                mac_address = mac.to_string();
            }
        }
        if line.contains("inet ") && !line.contains("inet6") {
            if let Some(addr) = line.trim().split_whitespace().nth(1) {
                ipv4 = addr.split('/').next().unwrap_or("").to_string();
            }
        }
        if line.contains("inet6") {
            if let Some(addr) = line.trim().split_whitespace().nth(1) {
                ipv6 = addr.split('/').next().unwrap_or("").to_string();
            }
        }
    }

    Ok(json!({
        "name": name,
        "type": "ethernet",
        "ipv4": ipv4,
        "ipv6": ipv6,
        "macAddress": mac_address,
        "isUp": is_up,
        "isRunning": is_up,
        "mtu": 1500
    }))
}
