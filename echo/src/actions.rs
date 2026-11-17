//! Root actions Echo asks the user about (Wave 3 of the hardening plan).
//!
//! `action_for` maps a bus call to a named action, polkit-style; `policy`
//! says how long an approval lasts. `details` are built only from the call
//! body's structural fields (a path's file stem, an SSID, ...), stripped of
//! control characters and capped, and are what the shell's one dialog shows.
//! Root callers never ask: that decision is the daemon's, not made here.

use std::time::Duration;

use serde_json::{Map, Value};

pub const PACKAGE_INSTALL: &str = "package.install";
pub const PACKAGE_REMOVE: &str = "package.remove";
pub const NETWORK_MANAGE: &str = "network.manage";
pub const ACCOUNTS_MANAGE: &str = "accounts.manage";
pub const FILESYSTEM_SYSTEM_WRITE: &str = "filesystem.system-write";

/// How long an approved `network.manage` stays valid per (app, action).
pub const NETWORK_CACHE_SECS: u64 = 300;
const DETAIL_MAX_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq)]
pub struct ActionRequest {
    pub action: &'static str,
    pub details: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    AskEveryTime,
    AskThenCache(Duration),
}

pub fn policy(action: &str) -> Policy {
    match action {
        NETWORK_MANAGE => Policy::AskThenCache(Duration::from_secs(NETWORK_CACHE_SECS)),
        // The filesystem flow's own grant length (the shell may override it per Authenticate).
        FILESYSTEM_SYSTEM_WRITE => Policy::AskThenCache(Duration::from_secs(
            crate::services::privilege::DEFAULT_GRANT_SECS,
        )),
        _ => Policy::AskEveryTime,
    }
}

/// Control characters removed, trimmed, at most 80 characters.
pub fn sanitize(s: &str) -> String {
    let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
    cleaned.trim().chars().take(DETAIL_MAX_CHARS).collect()
}

fn text(body: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| body.get(*k).and_then(|v| v.as_str()))
        .map(sanitize)
        .filter(|s| !s.is_empty())
}

fn details(pairs: Vec<(&str, Option<String>)>) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        if let Some(v) = v {
            m.insert(k.to_string(), Value::String(v));
        }
    }
    Value::Object(m)
}

/// File name without directories or the `.ipk`/`.tar.gz`/... extension.
fn package_stem(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut stem = name;
    for ext in [".tar.gz", ".ipk", ".tgz", ".tar"] {
        if let Some(s) = stem.strip_suffix(ext) {
            stem = s;
            break;
        }
    }
    let s = sanitize(stem);
    (!s.is_empty()).then_some(s)
}

/// An .ipk file is named `<package>_<version>_<arch>` (opkg's convention), so
/// the name alone gives the dialog the package and version without Echo
/// reading the file. Anything else is left whole.
fn split_ipk_stem(stem: &str) -> (&str, Option<&str>) {
    let parts: Vec<&str> = stem.split('_').collect();
    if parts.len() == 3 && parts.iter().all(|p| !p.is_empty()) {
        (parts[0], Some(parts[1]))
    } else {
        (stem, None)
    }
}

/// The action a call needs the user's approval for, if any. `destination` is
/// the service interface (`org.geckoOS.installer`, ...).
pub fn action_for(destination: &str, method: &str, body: &Value) -> Option<ActionRequest> {
    let (action, details) = match (destination, method) {
        ("org.geckoOS.installer", "Install") | ("org.geckoOS.nova", "Install") => {
            let stem = text(body, &["packagePath", "path"]).and_then(|p| package_stem(&p));
            let (package, version) = match stem.as_deref().map(split_ipk_stem) {
                Some((name, Some(version))) => (Some(name.to_string()), Some(version.to_string())),
                _ => (stem, None),
            };
            (PACKAGE_INSTALL, details(vec![("package", package), ("version", version)]))
        }
        ("org.geckoOS.installer", "Remove") => {
            (PACKAGE_REMOVE, details(vec![("package", text(body, &["package"]))]))
        }
        ("org.geckoOS.nova", "Uninstall") => {
            (PACKAGE_REMOVE, details(vec![("package", text(body, &["appId", "package"]))]))
        }
        ("org.geckoOS.network", "connectWifi") | ("org.geckoOS.network", "disconnectWifi") => {
            (NETWORK_MANAGE, details(vec![("ssid", text(body, &["ssid"]))]))
        }
        ("org.geckoOS.network", "setInterfaceEnabled") => {
            (NETWORK_MANAGE, details(vec![("interface", text(body, &["interface"]))]))
        }
        ("org.geckoOS.network", "setHostname") => {
            (NETWORK_MANAGE, details(vec![("hostname", text(body, &["hostname"]))]))
        }
        ("org.geckoOS.network", "setDNS") => {
            let dns = body
                .get("nameservers")
                .or_else(|| body.get("dns"))
                .map(|v| match v {
                    Value::Array(a) => a
                        .iter()
                        .filter_map(|x| x.as_str())
                        .map(sanitize)
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(", "),
                    Value::String(s) => s.clone(),
                    _ => String::new(),
                })
                .map(|s| sanitize(&s))
                .filter(|s| !s.is_empty());
            (NETWORK_MANAGE, details(vec![("dns", dns)]))
        }
        ("org.geckoOS.accounts", "create") | ("org.geckoOS.accounts", "delete") => {
            (ACCOUNTS_MANAGE, details(vec![("username", text(body, &["user", "username"]))]))
        }
        _ => return None,
    };
    Some(ActionRequest { action, details })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn a(d: &str, m: &str, b: Value) -> Option<ActionRequest> {
        action_for(d, m, &b)
    }

    #[test]
    fn packages() {
        for d in ["org.geckoOS.installer", "org.geckoOS.nova"] {
            let r = a(d, "Install", json!({"packagePath": "/home/x/Weather_1.2_all.ipk"})).unwrap();
            assert_eq!(r.action, "package.install");
            assert_eq!(r.details, json!({"package": "Weather", "version": "1.2"}));
        }
        // A file not named the opkg way is shown whole.
        let r = a("org.geckoOS.nova", "Install", json!({"packagePath": "/home/x/weather-app.tar"})).unwrap();
        assert_eq!(r.details, json!({"package": "weather-app"}));
        {
        }
        let r = a("org.geckoOS.installer", "Remove", json!({"package": "notes"})).unwrap();
        assert_eq!((r.action, r.details), ("package.remove", json!({"package": "notes"})));
        let r = a("org.geckoOS.nova", "Uninstall", json!({"appId": "com.x.notes"})).unwrap();
        assert_eq!((r.action, r.details), ("package.remove", json!({"package": "com.x.notes"})));
    }

    #[test]
    fn network() {
        let r = a("org.geckoOS.network", "connectWifi", json!({"ssid": "Home", "password": "secret"})).unwrap();
        assert_eq!((r.action, r.details), ("network.manage", json!({"ssid": "Home"})));
        let r = a("org.geckoOS.network", "disconnectWifi", json!({"ssid": "Home"})).unwrap();
        assert_eq!(r.action, "network.manage");
        let r = a("org.geckoOS.network", "setInterfaceEnabled", json!({"interface": "wlan0", "enabled": false})).unwrap();
        assert_eq!(r.details, json!({"interface": "wlan0"}));
        let r = a("org.geckoOS.network", "setDNS", json!({"nameservers": ["1.1.1.1", "9.9.9.9"]})).unwrap();
        assert_eq!(r.details, json!({"dns": "1.1.1.1, 9.9.9.9"}));
        let r = a("org.geckoOS.network", "setHostname", json!({"hostname": "pyxis"})).unwrap();
        assert_eq!((r.action, r.details), ("network.manage", json!({"hostname": "pyxis"})));
    }

    #[test]
    fn accounts() {
        for m in ["create", "delete"] {
            let r = a("org.geckoOS.accounts", m, json!({"user": "alex", "password": "p"})).unwrap();
            assert_eq!((r.action, r.details), ("accounts.manage", json!({"username": "alex"})));
        }
    }

    #[test]
    fn unmapped_methods() {
        for (d, m) in [
            ("org.geckoOS.time", "setTimezone"),
            ("org.geckoOS.power", "shutdown"),
            ("org.geckoOS.power", "sleep"),
            ("org.geckoOS.accounts", "setPassword"),
            ("org.geckoOS.accounts", "setProfile"),
            ("org.geckoOS.accounts", "list"),
            ("org.geckoOS.session", "lock"),
            ("org.geckoOS.session", "unlock"),
            ("org.geckoOS.network", "getStatus"),
            ("org.geckoOS.installer", "List"),
            ("org.geckoOS.nova", "List"),
            ("org.geckoOS.settings", "set"),
        ] {
            assert!(a(d, m, json!({})).is_none(), "{d}.{m}");
        }
        // Same method name on another service does not map.
        assert!(a("org.geckoOS.other", "Install", json!({})).is_none());
    }

    #[test]
    fn details_are_sanitised() {
        let long = "x".repeat(200);
        let r = a("org.geckoOS.network", "connectWifi", json!({"ssid": format!("a\n\u{1b}[2Jb{long}")})).unwrap();
        let s = r.details["ssid"].as_str().unwrap();
        assert!(!s.chars().any(|c| c.is_control()));
        assert_eq!(s.chars().count(), 80);
        // Missing fields give an empty object, still an action.
        let r = a("org.geckoOS.installer", "Install", json!({})).unwrap();
        assert_eq!(r.details, json!({}));
    }

    #[test]
    fn policies() {
        assert_eq!(policy("network.manage"), Policy::AskThenCache(Duration::from_secs(300)));
        for x in ["package.install", "package.remove", "accounts.manage", "unknown"] {
            assert_eq!(policy(x), Policy::AskEveryTime);
        }
        assert_eq!(
            policy("filesystem.system-write"),
            Policy::AskThenCache(Duration::from_secs(300))
        );
    }
}
