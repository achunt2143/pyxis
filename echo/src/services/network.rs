//! Network Service
//!
//! Provides network interface and connectivity information.
//!
//! Connectivity state is read straight from the kernel (`/sys/class/net`,
//! `/proc/net/route`) rather than from NetworkManager. Pyxis boots BusyBox
//! `udhcpc` under BusyBox init (`scripts/create-rootfs.sh`) and ships no
//! NetworkManager at all, so the previous `nmcli` implementation reported
//! `connected: false` on every machine regardless of its actual lease --
//! the status bar showed "offline" over a working connection. `ip`-based
//! helpers further down are fine to keep: `iproute2` *is* in the image.

use serde_json::{json, Value};
use anyhow::{Result, anyhow};
use std::fs;
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct NetworkService;

impl NetworkService {
    pub fn get_status() -> Result<Value> {
        let active = active_interface();

        let (connected, net_type, ssid) = match &active {
            Some(iface) => (
                true,
                if iface.is_wireless { "wifi" } else { "ethernet" }.to_string(),
                connection_name(iface),
            ),
            None => (false, "none".to_string(), String::new()),
        };

        Ok(json!({
            "connected": connected,
            "type": net_type,
            "ssid": ssid
        }))
    }

    pub fn get_type() -> Result<Value> {
        Ok(json!({
            "type": match active_interface() {
                Some(iface) if iface.is_wireless => "wifi",
                Some(_) => "ethernet",
                None => "none",
            }
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
        let resolv_path = std::env::var("PYXIS_NETWORK_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|d| PathBuf::from(d).join("resolv.conf"))
            .unwrap_or_else(|| PathBuf::from("/etc/resolv.conf"));

        if let Ok(content) = fs::read_to_string(&resolv_path) {
            for line in content.lines() {
                if let Some(rest) = line.trim().strip_prefix("nameserver") {
                    if let Some(ns) = rest.split_whitespace().next() {
                        nameservers.push(ns.to_string());
                    }
                }
            }
        }

        Ok(json!({
            "nameservers": nameservers
        }))
    }

    pub fn scan_wifi() -> Result<Value> {
        let wifi_ifaces = wireless_interfaces();
        let Some(iface) = wifi_ifaces.first().cloned() else {
            return Ok(json!({
                "networks": [],
                "wirelessAvailable": false,
                "interface": Value::Null,
            }));
        };

        let _ = Command::new("ip").args(["link", "set", &iface, "up"]).output();
        if let Ok(output) = Command::new("iw").args(["dev", &iface, "scan"]).output() {
            if output.status.success() {
                let networks = parse_iw_scan(&String::from_utf8_lossy(&output.stdout));
                return Ok(json!({
                    "networks": networks,
                    "wirelessAvailable": true,
                    "interface": iface,
                }));
            }
        }

        if let Ok(output) = Command::new("nmcli")
            .args(["-t", "-f", "SSID,SIGNAL,SECURITY,ACTIVE", "dev", "wifi", "list"])
            .output()
        {
            if output.status.success() {
                let networks = parse_nmcli_scan(&String::from_utf8_lossy(&output.stdout));
                return Ok(json!({
                    "networks": networks,
                    "wirelessAvailable": true,
                    "interface": iface,
                }));
            }
        }

        Ok(json!({
            "networks": [],
            "wirelessAvailable": true,
            "interface": iface,
            "error": "Unable to scan wireless networks (iw/nmcli unavailable or scan busy)"
        }))
    }

    pub fn connect_wifi(ssid: &str, password: Option<&str>) -> Result<Value> {
        let state_dir = network_state_dir();
        let wifi_ifaces = wireless_interfaces();
        connect_wifi_in(&state_dir, wifi_ifaces.first().map(String::as_str), ssid, password)
    }

    pub fn disconnect_wifi() -> Result<Value> {
        let state_dir = network_state_dir();
        let wifi_ifaces = wireless_interfaces();
        disconnect_wifi_in(&state_dir, wifi_ifaces.first().map(String::as_str))
    }

    pub fn set_interface_enabled(interface: &str, enabled: bool) -> Result<Value> {
        let iface = validate_interface_name(interface)?;
        if iface == "lo" {
            return Err(anyhow!("Cannot disable loopback interface `lo`"));
        }
        let action = if enabled { "up" } else { "down" };
        let output = Command::new("ip")
            .args(["link", "set", &iface, action])
            .output()
            .map_err(|e| anyhow!("Failed to run `ip link set {iface} {action}`: {e}"))?;
        if !output.status.success() {
            return Err(anyhow!(
                "`ip link set {iface} {action}` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(json!({
            "success": true,
            "interface": iface,
            "enabled": enabled
        }))
    }

    pub fn set_dns(nameservers: &[String]) -> Result<Value> {
        let state_dir = network_state_dir();
        let resolv_path = std::env::var("PYXIS_NETWORK_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|d| PathBuf::from(d).join("resolv.conf"))
            .unwrap_or_else(|| PathBuf::from("/etc/resolv.conf"));
        set_dns_at(&state_dir, &resolv_path, nameservers)
    }

    pub fn set_hostname(hostname: &str) -> Result<Value> {
        let override_dir = std::env::var("PYXIS_NETWORK_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from);
        let etc_hostname = override_dir
            .as_ref()
            .map(|d| d.join("hostname"))
            .unwrap_or_else(|| PathBuf::from("/etc/hostname"));
        let res = set_hostname_at(&etc_hostname, hostname)?;
        if override_dir.is_none() {
            let clean = res["hostname"].as_str().unwrap_or(hostname);
            let output = Command::new("hostname").arg(clean).output();
            if let Ok(out) = output {
                if !out.status.success() {
                    return Err(anyhow!(
                        "`hostname {clean}` failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ));
                }
            }
        }
        Ok(res)
    }
}

/// A real, kernel-visible network interface.
struct NetInterface {
    name: String,
    is_wireless: bool,
}

/// The interface currently carrying the default route, if it is a real device
/// that is operationally up.
///
/// "Connected" is deliberately defined as *has a usable default route* rather
/// than *a link is up*: a NIC with carrier but no DHCP lease is up, and
/// reporting that as connected is exactly the kind of lie this pass exists to
/// remove.
fn active_interface() -> Option<NetInterface> {
    let iface = default_route_interface()?;

    // Synthetic interfaces (lo, dummy0, sit0, ...) have no `device` symlink to
    // a PCI/USB parent. The same filter is applied at boot when picking a NIC
    // for udhcpc -- this kernel's CONFIG_DUMMY/CONFIG_IPV6_SIT always create
    // dummy0/sit0, and they sort before any real NIC name.
    if !Path::new(&format!("/sys/class/net/{}/device", iface)).exists() {
        return None;
    }

    if operstate(&iface) != "up" {
        return None;
    }

    Some(NetInterface {
        is_wireless: is_wireless(&iface),
        name: iface,
    })
}

/// Interface name owning the IPv4 default route, read from `/proc/net/route`.
fn default_route_interface() -> Option<String> {
    parse_default_route(&std::fs::read_to_string("/proc/net/route").ok()?)
}

/// Columns are `Iface Destination Gateway Flags ...` with the addresses as
/// little-endian hex; a destination of all zeros is the default route.
fn parse_default_route(routes: &str) -> Option<String> {
    routes
        .lines()
        .skip(1) // header
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let iface = fields.next()?;
            let destination = fields.next()?;
            (destination == "00000000").then(|| iface.to_string())
        })
}

/// `/sys/class/net/<iface>/operstate` -- "up", "down", "unknown", ...
fn operstate(iface: &str) -> String {
    std::fs::read_to_string(format!("/sys/class/net/{}/operstate", iface))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Wireless NICs expose a `wireless` directory (and, on modern cfg80211
/// drivers, a `phy80211` symlink); wired ones expose neither.
fn is_wireless(iface: &str) -> bool {
    Path::new(&format!("/sys/class/net/{}/wireless", iface)).exists()
        || Path::new(&format!("/sys/class/net/{}/phy80211", iface)).exists()
}

/// Display name for the active connection: the WiFi SSID when on wireless,
/// and the interface name otherwise.
///
/// The shell's `NetworkStatus` type and `TopSystemBar.tsx` both expect an
/// `ssid` field, so this must always produce *something* meaningful -- a
/// wired connection has no SSID, and showing the interface it's actually on
/// beats showing an empty string.
fn connection_name(iface: &NetInterface) -> String {
    if iface.is_wireless {
        if let Ok(output) = Command::new("iwgetid").arg("-r").output() {
            let ssid = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !ssid.is_empty() {
                return ssid;
            }
        }
    }
    iface.name.clone()
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

    let iface_type = if name == "lo" {
        "loopback"
    } else if is_wireless(name) {
        "wifi"
    } else {
        "ethernet"
    };

    Ok(json!({
        "name": name,
        "type": iface_type,
        "ipv4": ipv4,
        "ipv6": ipv6,
        "macAddress": mac_address,
        "isUp": is_up,
        "isRunning": is_up,
        "mtu": 1500
    }))
}

fn network_state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("PYXIS_NETWORK_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    PathBuf::from("/var/lib/pyxis/network")
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("tmp");
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let write_res = (|| -> Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content)?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if write_res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_res
}

fn wireless_interfaces() -> Vec<String> {
    let Ok(entries) = fs::read_dir("/sys/class/net") else {
        return Vec::new();
    };
    let mut ifaces: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| is_wireless(name))
        .collect();
    ifaces.sort();
    ifaces
}

fn parse_iw_scan(output: &str) -> Vec<Value> {
    let mut results = Vec::new();
    let mut ssid = String::new();
    let mut signal_dbm: Option<f64> = None;
    let mut secured = false;
    let mut in_bss = false;

    let flush = |results: &mut Vec<Value>, ssid: &mut String, signal_dbm: &mut Option<f64>, secured: &mut bool| {
        let s = ssid.trim().to_string();
        if !s.is_empty() {
            let quality = signal_dbm
                .map(|dbm| ((dbm + 100.0) * 2.0).round().clamp(0.0, 100.0) as u32)
                .unwrap_or(50);
            results.push(json!({
                "ssid": s,
                "signal": quality,
                "security": if *secured { "WPA2/WPA3" } else { "Open" },
                "connected": false
            }));
        }
        ssid.clear();
        *signal_dbm = None;
        *secured = false;
    };

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("BSS ") {
            if in_bss {
                flush(&mut results, &mut ssid, &mut signal_dbm, &mut secured);
            }
            in_bss = true;
        } else if let Some(val) = trimmed.strip_prefix("SSID:") {
            ssid = val.trim().to_string();
        } else if let Some(val) = trimmed.strip_prefix("signal:") {
            signal_dbm = val
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<f64>().ok());
        } else if trimmed.starts_with("RSN:") || trimmed.starts_with("WPA:") || trimmed.contains("Privacy") {
            secured = true;
        }
    }
    if in_bss {
        flush(&mut results, &mut ssid, &mut signal_dbm, &mut secured);
    }
    results
}

fn parse_nmcli_scan(output: &str) -> Vec<Value> {
    output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() < 4 {
                return None;
            }
            let ssid = parts[0].trim();
            if ssid.is_empty() {
                return None;
            }
            let signal = parts[1].trim().parse::<u32>().unwrap_or(50).min(100);
            let security = if parts[2].trim().is_empty() || parts[2].trim() == "--" {
                "Open"
            } else {
                parts[2].trim()
            };
            let connected = parts[3].trim().eq_ignore_ascii_case("yes");
            Some(json!({
                "ssid": ssid,
                "signal": signal,
                "security": security,
                "connected": connected
            }))
        })
        .collect()
}

fn validate_interface_name(interface: &str) -> Result<String> {
    let trimmed = interface.trim();
    if trimmed.is_empty() || trimmed.len() > 15 {
        return Err(anyhow!("Interface name must be 1-15 characters"));
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(anyhow!("Invalid interface name `{trimmed}`"));
    }
    Ok(trimmed.to_string())
}

fn connect_wifi_in(
    state_dir: &Path,
    iface: Option<&str>,
    ssid: &str,
    password: Option<&str>,
) -> Result<Value> {
    let clean_ssid = ssid.trim();
    if clean_ssid.is_empty() || clean_ssid.len() > 64 {
        return Err(anyhow!("Wi-Fi SSID must be 1-64 characters"));
    }
    if clean_ssid.chars().any(|c| c.is_control()) {
        return Err(anyhow!("Wi-Fi SSID cannot contain control characters"));
    }
    // Both values reach nmcli's argv as the value after a keyword, but a
    // leading `-` is still refused: it reads as an option to anyone skimming
    // the call, and no real SSID or passphrase needs one.
    if clean_ssid.starts_with('-') {
        return Err(anyhow!("Wi-Fi SSID cannot start with `-`"));
    }
    if clean_ssid.contains(['\0', '\n', '\r']) {
        return Err(anyhow!("Wi-Fi SSID cannot contain NUL or newline"));
    }
    if let Some(pw) = password {
        if pw.starts_with('-') {
            return Err(anyhow!("Wi-Fi passphrase cannot start with `-`"));
        }
        if pw.contains(['\0', '\n', '\r']) {
            return Err(anyhow!("Wi-Fi passphrase cannot contain NUL or newline"));
        }
        if !pw.is_empty() && pw.chars().any(|c| c.is_control()) {
            return Err(anyhow!("Wi-Fi passphrase cannot contain control characters"));
        }
    }

    let profile_path = state_dir.join("wifi.json");
    let profile = json!({
        "ssid": clean_ssid,
        "hasPassword": password.map(|p| !p.is_empty()).unwrap_or(false),
        "autoConnect": true,
        "connected": iface.is_some(),
    });
    atomic_write(&profile_path, serde_json::to_string_pretty(&profile)?.as_bytes())?;

    let mut connected = false;
    let mut note = String::from("Saved Wi-Fi profile");
    if let Some(ifname) = iface {
        if let Ok(out) = Command::new("nmcli")
            .args(match password.filter(|p| !p.is_empty()) {
                Some(pw) => vec!["dev", "wifi", "connect", clean_ssid, "password", pw, "ifname", ifname],
                None => vec!["dev", "wifi", "connect", clean_ssid, "ifname", ifname],
            })
            .output()
        {
            if out.status.success() {
                connected = true;
                note = format!("Connected to {clean_ssid} on {ifname}");
            }
        }
        if !connected {
            let mut cmd = Command::new("wpa_cli");
            if cmd.args(["-i", ifname, "status"]).output().is_ok() {
                let _ = Command::new("wpa_cli").args(["-i", ifname, "reconfigure"]).output();
                note = format!("Saved profile and requested wpa_supplicant connect on {ifname}");
            }
        }
    }

    Ok(json!({
        "success": true,
        "ssid": clean_ssid,
        "interface": iface,
        "connected": connected,
        "saved": true,
        "message": note
    }))
}

fn disconnect_wifi_in(state_dir: &Path, iface: Option<&str>) -> Result<Value> {
    let profile_path = state_dir.join("wifi.json");
    if profile_path.exists() {
        if let Ok(raw) = fs::read_to_string(&profile_path) {
            if let Ok(mut doc) = serde_json::from_str::<Value>(&raw) {
                doc["connected"] = json!(false);
                doc["autoConnect"] = json!(false);
                let _ = atomic_write(&profile_path, serde_json::to_string_pretty(&doc)?.as_bytes());
            }
        }
    }
    if let Some(ifname) = iface {
        let _ = Command::new("wpa_cli").args(["-i", ifname, "disconnect"]).output();
        let _ = Command::new("nmcli").args(["dev", "disconnect", ifname]).output();
    }
    Ok(json!({
        "success": true,
        "interface": iface,
        "connected": false
    }))
}

fn set_dns_at(state_dir: &Path, resolv_path: &Path, nameservers: &[String]) -> Result<Value> {
    if nameservers.is_empty() {
        return Err(anyhow!("At least one DNS nameserver is required"));
    }
    let mut validated = Vec::with_capacity(nameservers.len());
    for raw in nameservers {
        let trimmed = raw.trim();
        let parsed: IpAddr = trimmed
            .parse()
            .map_err(|_| anyhow!("Invalid DNS IP address `{trimmed}`"))?;
        validated.push(parsed.to_string());
    }

    let persisted = json!({ "nameservers": validated });
    atomic_write(
        &state_dir.join("dns.json"),
        serde_json::to_string_pretty(&persisted)?.as_bytes(),
    )?;

    let mut resolv = String::from("# Generated by Pyxis NetworkService\n");
    for ns in &validated {
        resolv.push_str(&format!("nameserver {ns}\n"));
    }
    atomic_write(resolv_path, resolv.as_bytes())?;

    Ok(json!({
        "success": true,
        "nameservers": validated
    }))
}

fn set_hostname_at(hostname_path: &Path, hostname: &str) -> Result<Value> {
    let clean = hostname.trim();
    if clean.is_empty() || clean.len() > 63 {
        return Err(anyhow!("Hostname must be 1-63 characters"));
    }
    if clean.starts_with('-') || clean.ends_with('-') {
        return Err(anyhow!("Hostname cannot start or end with a hyphen"));
    }
    if !clean.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(anyhow!("Hostname may only contain letters, digits, and hyphens"));
    }
    atomic_write(hostname_path, format!("{clean}\n").as_bytes())?;
    Ok(json!({
        "success": true,
        "hostname": clean
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `/proc/net/route` from a QEMU boot: the default route is on the
    /// second data row, and `dummy0` (which sorts first and has no default
    /// route) must not win.
    const PROC_NET_ROUTE: &str = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
dummy0\t0000FEA9\t00000000\t0001\t0\t0\t1000\t0000FFFF\t0\t0\t0
enp0s3\t00000000\t0202000A\t0003\t0\t0\t100\t00000000\t0\t0\t0
enp0s3\t0000020A\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0";

    #[test]
    fn finds_the_interface_carrying_the_default_route() {
        assert_eq!(
            parse_default_route(PROC_NET_ROUTE).as_deref(),
            Some("enp0s3")
        );
    }

    #[test]
    fn reports_no_default_route_when_there_is_none() {
        let no_default = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
enp0s3\t0000020A\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0";

        assert_eq!(parse_default_route(no_default), None);
    }

    /// The header row names a column "Destination", which is not a route.
    #[test]
    fn ignores_the_header_row() {
        assert_eq!(parse_default_route("Iface\tDestination\tGateway"), None);
        assert_eq!(parse_default_route(""), None);
    }

    #[test]
    fn parses_iw_scan_entries() {
        let sample = "\
BSS 00:11:22:33:44:55(on wlan0)
\tsignal: -52.00 dBm
\tSSID: PyxisHome
\tRSN:\t * Version: 1
BSS aa:bb:cc:dd:ee:ff(on wlan0)
\tsignal: -80.00 dBm
\tSSID: CafeGuest
";
        let list = parse_iw_scan(sample);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["ssid"], "PyxisHome");
        assert_eq!(list[0]["security"], "WPA2/WPA3");
        assert_eq!(list[0]["signal"], 96);
        assert_eq!(list[1]["ssid"], "CafeGuest");
        assert_eq!(list[1]["security"], "Open");
    }

    #[test]
    fn wifi_refuses_option_like_or_control_text_before_writing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");

        for (ssid, pw) in [
            ("-evil", None),
            ("--ifname", Some("secret123")),
            ("Net\nwork", None),
            ("Net\0work", None),
            ("StudioNet", Some("-pass")),
            ("StudioNet", Some("pa\nss")),
            ("StudioNet", Some("pa\0ss")),
        ] {
            assert!(connect_wifi_in(&state, None, ssid, pw).is_err(), "should refuse {ssid:?} / {pw:?}");
        }
        // Refused before the profile is written.
        assert!(!state.join("wifi.json").exists());

        // Ordinary values still pass (no interface, so nmcli is not run).
        assert!(connect_wifi_in(&state, None, "Studio Net", Some("correct horse")).is_ok());
    }

    #[test]
    fn dns_and_hostname_validation_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let resolv = dir.path().join("resolv.conf");
        let hostname_file = dir.path().join("hostname");

        assert!(set_dns_at(&state, &resolv, &[]).is_err());
        assert!(set_dns_at(&state, &resolv, &["not-an-ip".into()]).is_err());
        let dns_res = set_dns_at(&state, &resolv, &["1.1.1.1".into(), "8.8.8.8".into()]).unwrap();
        assert_eq!(dns_res["nameservers"], json!(["1.1.1.1", "8.8.8.8"]));
        let resolv_text = fs::read_to_string(&resolv).unwrap();
        assert!(resolv_text.contains("nameserver 1.1.1.1\n"));
        assert!(resolv_text.contains("nameserver 8.8.8.8\n"));

        assert!(set_hostname_at(&hostname_file, "").is_err());
        assert!(set_hostname_at(&hostname_file, "-bad").is_err());
        assert!(set_hostname_at(&hostname_file, "bad host!").is_err());
        let host_res = set_hostname_at(&hostname_file, "pyxis-workstation").unwrap();
        assert_eq!(host_res["hostname"], "pyxis-workstation");
        assert_eq!(fs::read_to_string(&hostname_file).unwrap(), "pyxis-workstation\n");

        let wifi_res = connect_wifi_in(&state, None, "StudioNet", Some("secret123")).unwrap();
        assert_eq!(wifi_res["saved"], true);
        let saved: Value = serde_json::from_str(&fs::read_to_string(state.join("wifi.json")).unwrap()).unwrap();
        assert_eq!(saved["ssid"], "StudioNet");
        assert_eq!(saved["hasPassword"], true);
        assert_eq!(saved["autoConnect"], true);

        disconnect_wifi_in(&state, None).unwrap();
        let after: Value = serde_json::from_str(&fs::read_to_string(state.join("wifi.json")).unwrap()).unwrap();
        assert_eq!(after["autoConnect"], false);
    }
}
