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
use std::path::Path;
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
}
