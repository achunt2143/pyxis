//! Peer authentication and method-call authorization for the Echo IPC bus.
//!
//! Caller identity comes from the kernel via `SO_PEERCRED` on the accepted
//! Unix-domain socket (`PeerIdentity::from_stream`) — never from client-supplied
//! headers or message bodies.

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::net::UnixStream;

/// Privilege tier of a connected Unix-socket peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Root (0), the Echo daemon's own UID, or a system account below 1000.
    /// Passes every check.
    Root,
    /// The shell (`geckouser`, 1000). Passes every rule except
    /// [`AccessRule::RootOnly`] (process spawning, unscoped filesystem,
    /// reserved service registration).
    Shell,
    /// Non-shell interactive user (`1002..10_000`).
    User,
    /// Sandboxed application process (`1001` or per-app UIDs `10_000..60_000`).
    App,
}

impl Tier {
    /// Root or Shell: what [`AccessRule::SystemOnly`] admits.
    pub fn is_privileged(self) -> bool {
        matches!(self, Tier::Root | Tier::Shell)
    }
}

/// Kernel-verified identity of a connected Echo peer (`SO_PEERCRED`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub uid: u32,
    pub gid: u32,
    pub pid: u32,
    pub tier: Tier,
}

impl PeerIdentity {
    /// Reads `SO_PEERCRED` (`uid`, `gid`, `pid`) from an accepted Unix stream.
    pub fn from_stream(stream: &UnixStream) -> Result<Self> {
        let cred = stream
            .peer_cred()
            .context("read SO_PEERCRED from accepted Unix socket")?;
        let uid = cred.uid();
        let gid = cred.gid();
        let pid = cred.pid().and_then(|p| u32::try_from(p).ok()).unwrap_or(0);
        Ok(Self {
            uid,
            gid,
            pid,
            tier: classify_uid(uid),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(uid: u32, tier: Tier) -> Self {
        Self {
            uid,
            gid: uid,
            pid: 1234,
            tier,
        }
    }
}

/// Classifies a peer UID using the current daemon's effective UID.
pub fn classify_uid(uid: u32) -> Tier {
    let daemon_uid = rustix::process::geteuid().as_raw();
    classify_uid_with_daemon(uid, daemon_uid)
}

/// Classifies `uid` into [`Tier`] relative to `daemon_uid`:
/// - `Root`: `0`, `daemon_uid`, or a system account below `1000`
/// - `Shell`: `1000` (unless it is the daemon's own uid)
/// - `User`: `1002..10_000`
/// - `App`: `1001`, `10_000..60_000`, and anything else above (`nobody`,
///   65534, included): a uid nobody planned for gets the least.
pub fn classify_uid_with_daemon(uid: u32, daemon_uid: u32) -> Tier {
    if uid == 0 || uid == daemon_uid || uid < 1000 {
        Tier::Root
    } else if uid == 1000 {
        Tier::Shell
    } else if (1002..10_000).contains(&uid) {
        Tier::User
    } else {
        Tier::App
    }
}

/// Authorizes `org.geckoOS.Echo.RegisterService` for `service_name`.
/// Only [`Tier::Root`] peers may claim an `org.geckoOS.*` name (Evo runs as
/// root; the shell registers nothing).
pub fn check_service_registration(
    peer: &PeerIdentity,
    service_name: &str,
) -> std::result::Result<(), String> {
    let trimmed = service_name.trim();
    if trimmed.is_empty() {
        return Err("Service name cannot be empty".to_string());
    }
    if trimmed.starts_with("org.geckoOS.") && peer.tier != Tier::Root {
        return Err(format!(
            "tier {:?} (uid={}, pid={}) may not register reserved service '{}'",
            peer.tier, peer.uid, peer.pid, trimmed
        ));
    }
    Ok(())
}

/// Method authorization requirement enforced for non-System peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRule {
    /// Allowed for any connected peer (public read-only / self-service call).
    Public,
    /// Requires the named Nova capability in the peer's app manifest/grants.
    Capability(&'static str),
    /// Requires all named Nova capabilities in the peer's app manifest/grants.
    All(&'static [&'static str]),
    /// Restricted to privileged peers: `Tier::Root` or `Tier::Shell`.
    SystemOnly,
    /// Restricted to `Tier::Root` (uid 0, the daemon's uid, system accounts).
    /// Not even the shell may call it.
    RootOnly,
}

/// Optional verified calling-app context resolved from kernel `SO_PEERCRED` PID
/// via `AppHost` -> `Nova` manifest grants.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppCallContext<'a> {
    pub app_id: Option<&'a str>,
    pub granted_capabilities: Option<&'a [String]>,
}

const POWER_WRITE_CAPS: &[&str] = &["power", "settings:write"];

/// Canonical deny-by-default access rule table for `(destination, method)`.
/// Every `org.geckoOS.*` service/method must be explicitly mapped; anything
/// unlisted inside `org.geckoOS.*` defaults to [`AccessRule::SystemOnly`].
pub fn access_rule(destination: &str, method: &str) -> AccessRule {
    if destination.trim().is_empty() || method.trim().is_empty() {
        return AccessRule::SystemOnly;
    }

    match destination {
        "org.geckoOS.Echo" => match method {
            "ListServices" | "GetServiceInfo" | "RegisterService" | "Ping" => AccessRule::Public,
            // Re-broadcasts whatever signal the caller names, so it is the
            // emit gate (signal_policy::may_emit) by another door: Evo and
            // echo-cli, both root, are its only callers.
            "EmitSignal" => AccessRule::RootOnly,
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.system" => match method {
            "getInfo" | "ping" | "listServices" => AccessRule::Public,
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.deviceInfo" => match method {
            "getOS"
            | "getKernel"
            | "getModel"
            | "getManufacturer"
            | "getHardware"
            | "getHostname"
            | "getMemoryInfo"
            | "getStorageInfo"
            | "getCPUInfo" => AccessRule::Public,
            // Restrict stable device identifiers (`getSerial`, `getUUID`) and unknown calls.
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.settings" => match method {
            "get" => AccessRule::Public,
            // Dumps every key, apps' own included: shell or `settings:write`.
            "getAll" => AccessRule::Capability("settings:write"),
            "set" | "delete" | "clear" | "reset" => AccessRule::Capability("settings:write"),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.time" => match method {
            "getClock" | "listTimezones" | "getLocation" => AccessRule::Public,
            "setTimezone" => AccessRule::Capability("settings:write"),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.power" => match method {
            "getBatteryStatus" | "getScreenBrightness" | "getScreenTimeout" => AccessRule::Public,
            "sleep" | "wake" | "setScreenBrightness" | "setScreenTimeout" => {
                AccessRule::Capability("power")
            }
            "shutdown" | "reboot" => AccessRule::All(POWER_WRITE_CAPS),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.network" => match method {
            "getStatus" | "getType" | "getInterfaces" | "getIPAddress" | "getGateway"
            | "getDNS" | "scanWifi" => AccessRule::Capability("network:read"),
            "connectWifi" | "disconnectWifi" | "setInterfaceEnabled" | "setDNS"
            | "setHostname" => AccessRule::Capability("network:manage"),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.audio" => match method {
            "getVolume"
            | "setVolume"
            | "setMute"
            | "listOutputs"
            | "setDefaultOutput"
            | "getInputVolume"
            | "setInputVolume"
            | "setInputMute"
            | "listInputs"
            | "setDefaultInput" => AccessRule::Capability("audio"),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.sound" => match method {
            "play" | "list" => AccessRule::Capability("audio"),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.notification" => match method {
            "show" => AccessRule::Capability("notifications"),
            "list" | "close" | "closeAll" => AccessRule::Capability("notifications:manage"),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.privilege" => match method {
            "Request" | "request" | "GetState" | "getState" | "Revoke" | "revoke" => {
                AccessRule::Public
            }
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.session" => match method {
            "getState" => AccessRule::Public,
            "switchUser" | "lock" | "logout" => AccessRule::Capability("settings:write"),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.accounts" => match method {
            "list" | "create" | "delete" | "setPassword" | "setProfile" => {
                AccessRule::Capability("settings:write")
            }
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.filesystem" => match method {
            "readDirectory" | "readFile" | "exists" | "stat" => {
                AccessRule::Capability("filesystem:read")
            }
            "writeFile"
            | "appendFile"
            | "deleteFile"
            | "deleteDirectory"
            | "createDirectory"
            | "copy"
            | "move"
            | "chmod" => AccessRule::Capability("filesystem:write"),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.terminal" => match method {
            "Open" | "Write" | "Resize" | "Close" => AccessRule::Capability("terminal"),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.installer" => match method {
            "ListAvailable" | "Inspect" | "ListInstalled" | "Install" | "Remove" => {
                AccessRule::Capability("appManager:install")
            }
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.appManager" => match method {
            "list" | "getInfo" | "getActive" => AccessRule::Capability("appManager:manage"),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.appHost" => match method {
            "ListApps"
            | "ListRunningApps"
            | "GetAppState"
            | "ListSurfaceSessions"
            | "SubscribeAppLifecycle" => AccessRule::Capability("appManager:manage"),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.nova" => match method {
            "List" | "GetInfo" | "GetAppIcon" => AccessRule::Public,
            "Launch" | "Close" | "Focus" => AccessRule::Capability("appManager:manage"),
            "GetPermissions" | "SetPermission" => AccessRule::Capability("settings:write"),
            "OpenExternalBrowser" => AccessRule::Capability("browser"),
            _ => AccessRule::SystemOnly,
        },
        "org.geckoOS.evo" => match method {
            "GetOutputs" | "GetAnimationState" => AccessRule::Public,
            "SetOutputConfig" | "SetAnimationSettings" => AccessRule::Capability("settings:write"),
            _ => AccessRule::SystemOnly,
        },
        // Runs commands as root: Root only, every method.
        "org.geckoOS.process" => AccessRule::RootOnly,
        // Any other/unknown `org.geckoOS.*` interface stays SystemOnly (Root or Shell).
        dest if dest.starts_with("org.geckoOS.") => AccessRule::SystemOnly,
        // Third-party companion services (e.g. `com.example.clock.service`).
        _ => AccessRule::Public,
    }
}

fn has_capability(caps: Option<&[String]>, required: &str) -> bool {
    caps.is_some_and(|list| list.iter().any(|c| c == required))
}

fn check_body_confinement(
    peer: &PeerIdentity,
    destination: &str,
    method: &str,
    body: &Value,
    ctx: AppCallContext<'_>,
) -> std::result::Result<(), String> {
    if destination == "org.geckoOS.filesystem" {
        // Only Root may run unconfined (unscoped) filesystem calls.
        let scope = body
            .get("scope")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!(
                    "tier {:?} must supply a filesystem scope for {}.{}",
                    peer.tier, destination, method
                )
            })?;
        match scope {
            "user" => {
                if !has_capability(ctx.granted_capabilities, "filesystem:user") {
                    return Err(format!(
                        "tier {:?} lacks 'filesystem:user' for user filesystem scope",
                        peer.tier
                    ));
                }
            }
            "system" => {
                if !has_capability(ctx.granted_capabilities, "filesystem:system") {
                    return Err(format!(
                        "tier {:?} lacks 'filesystem:system' for system filesystem scope",
                        peer.tier
                    ));
                }
                if crate::services::filesystem::scope::WRITES.contains(&method) {
                    let caller = ctx.app_id.ok_or_else(|| {
                        "elevated system filesystem writes require an identified app session"
                            .to_string()
                    })?;
                    let claimed = body
                        .get("appId")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if claimed != caller {
                        return Err(format!(
                            "app '{}' may not claim appId '{}' on elevated system write",
                            caller, claimed
                        ));
                    }
                }
            }
            other => {
                let Some(target_app) = other.strip_prefix("app:") else {
                    return Err(format!("invalid filesystem scope '{}'", scope));
                };
                if target_app.is_empty() {
                    return Err("empty app filesystem scope".to_string());
                }
                if ctx.app_id != Some(target_app)
                    && !has_capability(ctx.granted_capabilities, "filesystem:apps")
                {
                    return Err(format!(
                        "peer (app={:?}) lacks 'filesystem:apps' for scope '{}'",
                        ctx.app_id, scope
                    ));
                }
            }
        }
    } else if destination == "org.geckoOS.privilege"
        && matches!(method, "Request" | "request" | "Revoke" | "revoke")
    {
        if !has_capability(ctx.granted_capabilities, "windows:manage") {
            if let Some(caller) = ctx.app_id {
                let claimed = body
                    .get("appId")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("");
                if claimed.is_empty() || claimed != caller {
                    return Err(format!(
                        "app '{}' may only invoke privilege.{} for its own appId (got '{}')",
                        caller, method, claimed
                    ));
                }
            } else {
                // The caller's app id did not resolve: it may not act for anyone.
                return Err(format!(
                    "tier {:?} with no resolved app may not invoke privilege.{}",
                    peer.tier, method
                ));
            }
        }
    } else if destination == "org.geckoOS.notification"
        && method == "show"
        && !has_capability(ctx.granted_capabilities, "notifications:manage")
    {
        if let Some(caller) = ctx.app_id {
            if let Some(claimed) = body
                .get("appId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if claimed != caller {
                    return Err(format!(
                        "app '{}' may not spoof notification appId '{}'",
                        caller, claimed
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Authorizes an incoming `MethodCall` for `(destination, method)` with an
/// explicit [`AppCallContext`] (resolved manifest grants + app identity) and
/// optional request `body` for scope/identity confinement.
pub fn check_method_call_with_context(
    peer: &PeerIdentity,
    destination: &str,
    method: &str,
    body: Option<&Value>,
    ctx: AppCallContext<'_>,
) -> std::result::Result<(), String> {
    if peer.tier == Tier::Root {
        return Ok(());
    }

    let rule = access_rule(destination, method);
    if peer.tier == Tier::Shell {
        if rule == AccessRule::RootOnly {
            return Err(format!(
                "tier {:?} may not invoke {}.{} (root only)",
                peer.tier, destination, method
            ));
        }
        if destination == "org.geckoOS.filesystem" {
            let scoped = body
                .and_then(|b| b.get("scope"))
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            if !scoped {
                return Err(format!(
                    "tier {:?} must supply a filesystem scope for {}.{}",
                    peer.tier, destination, method
                ));
            }
        }
        return Ok(());
    }

    match rule {
        AccessRule::Public => {}
        AccessRule::Capability(cap) => {
            if !has_capability(ctx.granted_capabilities, cap) {
                return Err(format!(
                    "tier {:?} (app={:?}) lacks capability '{}' for {}.{}",
                    peer.tier, ctx.app_id, cap, destination, method
                ));
            }
        }
        AccessRule::All(caps) => {
            for cap in caps {
                if !has_capability(ctx.granted_capabilities, cap) {
                    return Err(format!(
                        "tier {:?} (app={:?}) lacks capability '{}' for {}.{}",
                        peer.tier, ctx.app_id, cap, destination, method
                    ));
                }
            }
        }
        AccessRule::SystemOnly | AccessRule::RootOnly => {
            return Err(format!(
                "tier {:?} may not invoke {}.{}",
                peer.tier, destination, method
            ));
        }
    }

    let none = Value::Null;
    check_body_confinement(peer, destination, method, body.unwrap_or(&none), ctx)?;
    Ok(())
}

/// Authorizes an incoming `MethodCall` for `(destination, method)` against the
/// connection's kernel-authenticated [`PeerIdentity`] with no app manifest grants.
pub fn check_method_call(
    peer: &PeerIdentity,
    destination: &str,
    method: &str,
) -> std::result::Result<(), String> {
    check_method_call_with_context(peer, destination, method, None, AppCallContext::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_classification_matches_tier_ranges() {
        assert_eq!(classify_uid_with_daemon(0, 0), Tier::Root);
        assert_eq!(classify_uid_with_daemon(1000, 0), Tier::Shell);
        assert_eq!(classify_uid_with_daemon(1001, 0), Tier::App);
        assert_eq!(classify_uid_with_daemon(1002, 0), Tier::User);
        assert_eq!(classify_uid_with_daemon(9999, 0), Tier::User);
        assert_eq!(classify_uid_with_daemon(10_000, 0), Tier::App);
        assert_eq!(classify_uid_with_daemon(59_999, 0), Tier::App);
        assert_eq!(classify_uid_with_daemon(60_000, 0), Tier::App);
        assert_eq!(classify_uid_with_daemon(65_534, 0), Tier::App);
        assert_eq!(classify_uid_with_daemon(999, 0), Tier::Root);
        assert_eq!(classify_uid_with_daemon(65534, 0), Tier::App);
        // Same UID as daemon itself is Root even if inside another range.
        assert_eq!(classify_uid_with_daemon(1005, 1005), Tier::Root);
    }

    #[test]
    fn service_registration_blocks_reserved_prefix_for_non_system() {
        let sys = PeerIdentity::for_test(0, Tier::Root);
        let shell = PeerIdentity::for_test(1000, Tier::Shell);
        let user = PeerIdentity::for_test(1002, Tier::User);
        let app = PeerIdentity::for_test(1001, Tier::App);

        assert!(check_service_registration(&sys, "org.geckoOS.evo").is_ok());
        assert!(check_service_registration(&shell, "org.geckoOS.evo").is_err());
        assert!(check_service_registration(&shell, "com.example.custom").is_ok());
        assert!(check_service_registration(&app, "org.geckoOS.evo").is_err());
        assert!(check_service_registration(&user, "org.geckoOS.privilege").is_err());
        assert!(check_service_registration(&app, "com.example.custom").is_ok());
        assert!(check_service_registration(&sys, "  ").is_err());
    }

    #[test]
    fn method_call_enforces_tier_policy() {
        let sys = PeerIdentity::for_test(0, Tier::Root);
        let bloom = PeerIdentity::for_test(1000, Tier::Shell);
        let user = PeerIdentity::for_test(1002, Tier::User);
        let app = PeerIdentity::for_test(1001, Tier::App);

        // Root is allowed everywhere.
        for s in [&sys] {
            assert!(check_method_call(s, "org.geckoOS.process", "spawn").is_ok());
            assert!(check_method_call_with_context(s, "org.geckoOS.filesystem", "readFile", Some(&serde_json::json!({"path":"/etc/shadow"})), AppCallContext::default()).is_ok());
        }
        // The shell is allowed everything but the root-only set.
        for s in [&bloom] {
            assert!(check_method_call(s, "org.geckoOS.process", "spawn").is_err());
            assert!(check_method_call(s, "org.geckoOS.process", "kill").is_err());
            assert!(check_method_call_with_context(s, "org.geckoOS.filesystem", "readFile", Some(&serde_json::json!({"path":"/etc/shadow"})), AppCallContext::default()).is_err());
            assert!(check_method_call_with_context(s, "org.geckoOS.filesystem", "readFile", None, AppCallContext::default()).is_err());
            assert!(check_method_call_with_context(s, "org.geckoOS.filesystem", "readFile", Some(&serde_json::json!({"scope":"user","path":"~"})), AppCallContext::default()).is_ok());
            assert!(check_method_call(s, "org.geckoOS.evo", "SetOverlayFocus").is_ok());
            assert!(check_method_call(s, "org.geckoOS.session", "lock").is_ok());
            assert!(check_method_call(s, "org.geckoOS.settings", "getAll").is_ok());
            assert!(check_method_call(s, "org.geckoOS.settings", "set").is_ok());
            assert!(check_method_call(s, "org.geckoOS.privilege", "Authenticate").is_ok());
            assert!(check_method_call(s, "org.geckoOS.power", "shutdown").is_ok());
            assert!(check_method_call(s, "org.geckoOS.evo", "Lock").is_ok());
            assert!(check_method_call(s, "org.geckoOS.terminal", "Open").is_ok());
            assert!(check_method_call(s, "org.geckoOS.installer", "Install").is_ok());
            assert!(check_method_call(s, "org.geckoOS.accounts", "create").is_ok());
        }

        // Ungranted App & User peers are blocked from privileged/system/capability-gated controls.
        for peer in [&app, &user] {
            assert!(check_method_call(peer, "org.geckoOS.process", "spawn").is_err());
            assert!(check_method_call(peer, "org.geckoOS.process", "kill").is_err());
            assert!(check_method_call(peer, "org.geckoOS.power", "shutdown").is_err());
            assert!(check_method_call(peer, "org.geckoOS.power", "reboot").is_err());
            assert!(check_method_call(peer, "org.geckoOS.power", "sleep").is_err());
            assert!(check_method_call(peer, "org.geckoOS.privilege", "Authenticate").is_err());
            assert!(check_method_call(peer, "org.geckoOS.privilege", "authenticate").is_err());
            assert!(check_method_call(peer, "org.geckoOS.privilege", "Dismiss").is_err());
            assert!(check_method_call(peer, "org.geckoOS.privilege", "dismiss").is_err());
            assert!(check_method_call(peer, "org.geckoOS.session", "unlock").is_err());
            assert!(check_method_call(peer, "org.geckoOS.session", "lock").is_err());
            assert!(check_method_call(peer, "org.geckoOS.session", "logout").is_err());
            assert!(check_method_call(peer, "org.geckoOS.session", "switchUser").is_err());
            assert!(check_method_call(peer, "org.geckoOS.Echo", "EmitSignal").is_err());
            assert!(check_method_call(peer, "org.geckoOS.appManager", "install").is_err());
            assert!(check_method_call(peer, "org.geckoOS.appManager", "list").is_err());
            assert!(check_method_call(peer, "org.geckoOS.appHost", "LaunchApp").is_err());
            assert!(check_method_call(peer, "org.geckoOS.appHost", "ListRunningApps").is_err());
            assert!(check_method_call(peer, "org.geckoOS.evo", "SetOverlayFocus").is_err());
            assert!(check_method_call(peer, "org.geckoOS.evo", "SetOutputConfig").is_err());
            assert!(check_method_call(peer, "org.geckoOS.evo", "SetAnimationSettings").is_err());
            assert!(check_method_call(peer, "org.geckoOS.nova", "Install").is_err());
            assert!(check_method_call(peer, "org.geckoOS.nova", "Launch").is_err());
            assert!(check_method_call(peer, "org.geckoOS.nova", "GetPermissions").is_err());
            assert!(check_method_call(peer, "org.geckoOS.nova", "SetPermission").is_err());
            assert!(check_method_call(peer, "org.geckoOS.nova", "OpenExternalBrowser").is_err());
            assert!(check_method_call(peer, "org.geckoOS.accounts", "list").is_err());
            assert!(check_method_call(peer, "org.geckoOS.accounts", "create").is_err());
            assert!(check_method_call(peer, "org.geckoOS.installer", "ListAvailable").is_err());
            assert!(check_method_call(peer, "org.geckoOS.installer", "Install").is_err());
            assert!(check_method_call(peer, "org.geckoOS.terminal", "Open").is_err());
            assert!(check_method_call(peer, "org.geckoOS.terminal", "Write").is_err());
            assert!(check_method_call(peer, "org.geckoOS.settings", "set").is_err());
            assert!(check_method_call(peer, "org.geckoOS.settings", "getAll").is_err());
            assert!(check_method_call(peer, "org.geckoOS.settings", "reset").is_err());
            assert!(check_method_call(peer, "org.geckoOS.time", "setTimezone").is_err());
            assert!(check_method_call(peer, "org.geckoOS.notification", "show").is_err());
            assert!(check_method_call(peer, "org.geckoOS.notification", "list").is_err());
            assert!(check_method_call(peer, "org.geckoOS.network", "getStatus").is_err());
            assert!(check_method_call(peer, "org.geckoOS.audio", "setVolume").is_err());
            assert!(check_method_call(peer, "org.geckoOS.sound", "play").is_err());
            assert!(check_method_call(peer, "org.geckoOS.deviceInfo", "getSerial").is_err());
            assert!(check_method_call(peer, "org.geckoOS.deviceInfo", "getUUID").is_err());
            assert!(check_method_call(peer, "org.geckoOS.filesystem", "readFile").is_err());
            assert!(check_method_call(peer, "org.geckoOS.filesystem", "writeFile").is_err());
            assert!(check_method_call(peer, "org.geckoOS.unknownService", "doWork").is_err());
        }

        // Public read/self-service & custom app-service calls remain permitted.
        for peer in [&app, &user] {
            assert!(check_method_call(peer, "org.geckoOS.system", "getInfo").is_ok());
            assert!(check_method_call(peer, "org.geckoOS.privilege", "GetState").is_ok());
            // An unidentified caller may not revoke (or request) for anyone.
            assert!(check_method_call(peer, "org.geckoOS.privilege", "Revoke").is_err());
            assert!(check_method_call(peer, "org.geckoOS.session", "getState").is_ok());
            assert!(check_method_call(peer, "org.geckoOS.settings", "get").is_ok());
            assert!(check_method_call(peer, "org.geckoOS.time", "getClock").is_ok());
            assert!(check_method_call(peer, "org.geckoOS.nova", "List").is_ok());
            assert!(check_method_call(peer, "org.geckoOS.nova", "GetInfo").is_ok());
            assert!(check_method_call(peer, "org.geckoOS.nova", "GetAppIcon").is_ok());
            assert!(check_method_call(peer, "org.geckoOS.evo", "GetOutputs").is_ok());
            assert!(check_method_call(peer, "org.geckoOS.evo", "GetAnimationState").is_ok());
            assert!(check_method_call(peer, "org.geckoOS.power", "getBatteryStatus").is_ok());
            assert!(check_method_call(peer, "org.geckoOS.deviceInfo", "getOS").is_ok());
            assert!(check_method_call(peer, "com.example.custom", "ping").is_ok());
        }
    }

    #[test]
    fn only_root_may_emit_through_echo() {
        let shell = PeerIdentity { uid: 1000, gid: 1000, pid: 1, tier: classify_uid_with_daemon(1000, 0) };
        let root = PeerIdentity { uid: 0, gid: 0, pid: 1, tier: classify_uid_with_daemon(0, 0) };
        assert!(check_method_call(&shell, "org.geckoOS.Echo", "EmitSignal").is_err());
        assert!(check_method_call(&root, "org.geckoOS.Echo", "EmitSignal").is_ok());
    }

    #[test]
    fn unresolved_app_may_not_request_privilege() {
        let app = PeerIdentity::for_test(65534, Tier::App);
        let body = serde_json::json!({ "appId": "files" });
        for m in ["Request", "request", "Revoke", "revoke"] {
            assert!(check_method_call_with_context(&app, "org.geckoOS.privilege", m, Some(&body), AppCallContext::default()).is_err());
        }
        let ctx = AppCallContext { app_id: Some("files"), granted_capabilities: None };
        assert!(check_method_call_with_context(&app, "org.geckoOS.privilege", "Request", Some(&body), ctx).is_ok());
        let caps = vec!["settings:write".to_string()];
        let ctx = AppCallContext { app_id: Some("settings"), granted_capabilities: Some(&caps) };
        assert!(check_method_call_with_context(&app, "org.geckoOS.settings", "getAll", None, ctx).is_ok());
        assert!(Tier::Shell.is_privileged() && Tier::Root.is_privileged() && !Tier::App.is_privileged());
    }

    #[test]
    fn granted_app_contexts_allow_manifest_calls_and_enforce_confinement() {
        let app = PeerIdentity::for_test(1001, Tier::App);
        let cap_vec = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Settings app (power, audio, network, settings:write)
        let settings_caps = cap_vec(&["power", "audio", "network:read", "network:manage", "settings:write"]);
        let settings_ctx = AppCallContext {
            app_id: Some("settings"),
            granted_capabilities: Some(&settings_caps),
        };
        assert!(check_method_call_with_context(&app, "org.geckoOS.accounts", "list", None, settings_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.accounts", "create", None, settings_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.session", "switchUser", None, settings_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.evo", "SetOutputConfig", None, settings_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.evo", "SetAnimationSettings", None, settings_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.nova", "GetPermissions", None, settings_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.nova", "SetPermission", None, settings_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.settings", "set", None, settings_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.time", "setTimezone", None, settings_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.power", "shutdown", None, settings_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.audio", "setVolume", None, settings_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.network", "connectWifi", None, settings_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.terminal", "Open", None, settings_ctx).is_err());
        assert!(check_method_call_with_context(&app, "org.geckoOS.installer", "Install", None, settings_ctx).is_err());

        // network:read alone reads but cannot join an access point or repoint DNS.
        let reader_caps = cap_vec(&["network:read"]);
        let reader_ctx = AppCallContext {
            app_id: Some("com.example.reader"),
            granted_capabilities: Some(&reader_caps),
        };
        for m in ["getStatus", "getType", "getInterfaces", "getIPAddress", "getGateway", "getDNS", "scanWifi"] {
            assert!(check_method_call_with_context(&app, "org.geckoOS.network", m, None, reader_ctx).is_ok(), "{m}");
        }
        for m in ["connectWifi", "disconnectWifi", "setInterfaceEnabled", "setDNS", "setHostname"] {
            assert!(check_method_call_with_context(&app, "org.geckoOS.network", m, None, reader_ctx).is_err(), "{m}");
        }
        // The old plain `network` grant (never produced by Nova now) covers neither tier.
        let legacy_caps = cap_vec(&["network"]);
        let legacy_ctx = AppCallContext {
            app_id: Some("com.example.legacy"),
            granted_capabilities: Some(&legacy_caps),
        };
        assert!(check_method_call_with_context(&app, "org.geckoOS.network", "getStatus", None, legacy_ctx).is_err());
        assert!(check_method_call_with_context(&app, "org.geckoOS.network", "setDNS", None, legacy_ctx).is_err());

        // Installer app (appManager:install, appManager:manage)
        let installer_caps = cap_vec(&["appManager:install", "appManager:manage"]);
        let installer_ctx = AppCallContext {
            app_id: Some("installer"),
            granted_capabilities: Some(&installer_caps),
        };
        assert!(check_method_call_with_context(&app, "org.geckoOS.installer", "ListAvailable", None, installer_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.installer", "Install", None, installer_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.installer", "Remove", None, installer_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.nova", "Launch", None, installer_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.appHost", "ListRunningApps", None, installer_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.accounts", "list", None, installer_ctx).is_err());
        assert!(check_method_call_with_context(&app, "org.geckoOS.terminal", "Open", None, installer_ctx).is_err());

        // Terminal app (terminal)
        let term_caps = cap_vec(&["terminal"]);
        let term_ctx = AppCallContext {
            app_id: Some("terminal"),
            granted_capabilities: Some(&term_caps),
        };
        assert!(check_method_call_with_context(&app, "org.geckoOS.terminal", "Open", None, term_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.terminal", "Write", None, term_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.terminal", "Resize", None, term_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.terminal", "Close", None, term_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.installer", "Install", None, term_ctx).is_err());

        // Files app (filesystem:read, filesystem:write, filesystem:user, filesystem:system)
        let files_caps = cap_vec(&["filesystem:read", "filesystem:write", "filesystem:user", "filesystem:system"]);
        let files_ctx = AppCallContext {
            app_id: Some("files"),
            granted_capabilities: Some(&files_caps),
        };
        assert!(check_method_call_with_context(
            &app,
            "org.geckoOS.filesystem",
            "readDirectory",
            Some(&serde_json::json!({ "scope": "user", "path": "~" })),
            files_ctx
        ).is_ok());
        assert!(check_method_call_with_context(
            &app,
            "org.geckoOS.filesystem",
            "writeFile",
            Some(&serde_json::json!({ "scope": "system", "appId": "files", "path": "/etc/test" })),
            files_ctx
        ).is_ok());
        // Unscoped filesystem calls remain forbidden even for Files.
        assert!(check_method_call_with_context(
            &app,
            "org.geckoOS.filesystem",
            "readFile",
            Some(&serde_json::json!({ "path": "/etc/shadow" })),
            files_ctx
        ).is_err());
        // Cannot access another app's data without filesystem:apps.
        assert!(check_method_call_with_context(
            &app,
            "org.geckoOS.filesystem",
            "readFile",
            Some(&serde_json::json!({ "scope": "app:notes", "path": "~/notes.json" })),
            files_ctx
        ).is_err());

        // Third-party app (com.pyxis.hello): own data + notifications only.
        let hello_caps = cap_vec(&["filesystem:read", "filesystem:write", "notifications"]);
        let hello_ctx = AppCallContext {
            app_id: Some("com.pyxis.hello"),
            granted_capabilities: Some(&hello_caps),
        };
        assert!(check_method_call_with_context(
            &app,
            "org.geckoOS.filesystem",
            "readFile",
            Some(&serde_json::json!({ "scope": "app:com.pyxis.hello", "path": "~/data.json" })),
            hello_ctx
        ).is_ok());
        assert!(check_method_call_with_context(
            &app,
            "org.geckoOS.filesystem",
            "readFile",
            Some(&serde_json::json!({ "scope": "user", "path": "~" })),
            hello_ctx
        ).is_err());
        assert!(check_method_call_with_context(
            &app,
            "org.geckoOS.filesystem",
            "readFile",
            Some(&serde_json::json!({ "scope": "system", "path": "/etc/passwd" })),
            hello_ctx
        ).is_err());
        assert!(check_method_call_with_context(
            &app,
            "org.geckoOS.notification",
            "show",
            Some(&serde_json::json!({ "appId": "com.pyxis.hello", "title": "hi" })),
            hello_ctx
        ).is_ok());
        assert!(check_method_call_with_context(
            &app,
            "org.geckoOS.notification",
            "show",
            Some(&serde_json::json!({ "appId": "settings", "title": "spoof" })),
            hello_ctx
        ).is_err());
        assert!(check_method_call_with_context(
            &app,
            "org.geckoOS.privilege",
            "Revoke",
            Some(&serde_json::json!({ "appId": "files" })),
            hello_ctx
        ).is_err());
        assert!(check_method_call_with_context(
            &app,
            "org.geckoOS.privilege",
            "Revoke",
            Some(&serde_json::json!({ "appId": "com.pyxis.hello" })),
            hello_ctx
        ).is_ok());
        // Power without settings:write never allows shutdown.
        let power_only = cap_vec(&["power"]);
        let power_ctx = AppCallContext {
            app_id: Some("com.example.power"),
            granted_capabilities: Some(&power_only),
        };
        assert!(check_method_call_with_context(&app, "org.geckoOS.power", "sleep", None, power_ctx).is_ok());
        assert!(check_method_call_with_context(&app, "org.geckoOS.power", "shutdown", None, power_ctx).is_err());
    }

    #[tokio::test]
    async fn peer_identity_reads_from_unix_stream_pair() {
        let (a, _b) = UnixStream::pair().unwrap();
        let peer = PeerIdentity::from_stream(&a).unwrap();
        assert_eq!(peer.uid, rustix::process::geteuid().as_raw());
        assert_eq!(peer.gid, rustix::process::getegid().as_raw());
        assert_eq!(peer.pid, std::process::id());
        assert_eq!(peer.tier, classify_uid(peer.uid));
    }
}
