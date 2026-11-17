//! Who may emit and who may hear a bus signal.
//!
//! Echo's socket is 0666, so any local process can connect. Signals used to go
//! to every connection and be accepted from any of them. The uid decides now:
//! uid 0, the daemon's own euid and uid 1000 (the shell) are privileged and
//! hear everything (the shell's bridge then filters per page, from
//! `BRIDGE_SIGNALS` in `PyxisBridgeParent.sys.mjs`); any other peer hears
//! only what `signal_rule` lets its app's declared permissions hear. Only root
//! (or the daemon's euid) may emit on an `org.geckoOS.*` interface.
//!
//! `signal_rule` mirrors `BRIDGE_SIGNALS`; keep them in step.

/// What hearing a signal needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalRule {
    /// Any peer.
    Public,
    /// A peer whose app holds this capability.
    Capability(&'static str),
    /// Root and the shell only (also: any `org.geckoOS.*` signal not listed).
    Privileged,
}

pub const SYSTEM_PREFIX: &str = "org.geckoOS.";

/// Root, the daemon's own user, or the shell (uid 1000).
pub fn is_privileged_uid(uid: u32, daemon_uid: u32) -> bool {
    uid == 0 || uid == 1000 || uid == daemon_uid
}

/// Root or the daemon's own user: the only legitimate emitters.
pub fn is_root_uid(uid: u32, daemon_uid: u32) -> bool {
    uid == 0 || uid == daemon_uid
}

/// May a peer with `uid` emit a signal on `interface`?
pub fn may_emit(uid: u32, daemon_uid: u32, interface: &str) -> bool {
    !interface.starts_with(SYSTEM_PREFIX) || is_root_uid(uid, daemon_uid)
}

pub fn signal_rule(interface: &str, signal: &str) -> SignalRule {
    use SignalRule::*;
    let Some(service) = interface.strip_prefix(SYSTEM_PREFIX) else {
        // Not a system interface: an external service's own signals.
        return Public;
    };
    match (service, signal) {
        ("notification", "NotificationShown" | "NotificationClosed") => {
            Capability("notifications:manage")
        }
        (
            "audio",
            "VolumeChanged" | "OutputChanged" | "InputVolumeChanged" | "InputChanged",
        ) => Capability("audio"),
        ("settings", "SettingChanged") => Public,
        ("time", "TimezoneChanged") => Public,
        ("session", "LockChanged" | "UnlockFailed") => Capability("windows:manage"),
        ("privilege", "PrivilegeRequested") => Capability("windows:manage"),
        ("privilege", "PrivilegeChanged") => Public,
        ("terminal", "TerminalOpened" | "TerminalOutput" | "TerminalClosed") => {
            Capability("terminal")
        }
        (
            "appHost",
            "AppStarting" | "AppRunning" | "AppSuspended" | "AppStopped" | "AppCrashed"
            | "AppSurfaceMapped" | "AppSurfaceUnmapped",
        ) => Capability("appManager:manage"),
        (
            "nova",
            "RegistryChanged" | "AppInstalled" | "AppUninstalled" | "AppLaunched"
            | "AppSuspended" | "AppResumed",
        ) => Capability("appManager:manage"),
        ("nova", "PermissionsChanged") => Capability("settings:write"),
        (
            "installer",
            "InstallStarted" | "InstallProgress" | "InstallCompleted" | "InstallFailed"
            | "RemoveCompleted" | "RemoveFailed",
        ) => Capability("appManager:install"),
        (
            "evo",
            "WindowCreated" | "WindowClosed" | "WindowMoved" | "WindowMovedToWorkspace"
            | "WindowFocusChanged" | "WindowMinimized" | "WindowMaximized"
            | "WindowFullscreen" | "ShortcutTriggered" | "OverlayFocusChanged"
            | "SwitcherChanged" | "MinimizeLanded" | "WorkspaceChanged"
            | "WorkspaceCreated" | "WorkspaceDeleted",
        ) => Capability("windows:manage"),
        ("evo", "OutputsChanged") => Capability("settings:write"),
        _ => Privileged,
    }
}

/// Decides delivery. `caps` is only called for a capability rule, and only for
/// an unprivileged peer; it returns that peer's app's permissions (empty when
/// no app resolves, which denies).
pub fn may_hear(
    rule: SignalRule,
    peer_uid: u32,
    daemon_uid: u32,
    caps: impl FnOnce() -> Vec<String>,
) -> bool {
    if is_privileged_uid(peer_uid, daemon_uid) {
        return true;
    }
    match rule {
        SignalRule::Public => true,
        SignalRule::Privileged => false,
        SignalRule::Capability(c) => caps().iter().any(|p| p == c),
    }
}

/// Permissions of the app behind `pid`, resolved as method calls resolve them.
pub fn app_permissions_for_pid(pid: u32) -> Vec<String> {
    match crate::services::AppHost::global().app_id_for_pid(pid) {
        Some(id) => crate::services::Nova::global()
            .declared_permissions(crate::services::Nova::normalize_app_id(&id)),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SignalRule::*;

    #[test]
    fn table_matches_bridge() {
        assert_eq!(signal_rule("org.geckoOS.terminal", "TerminalOutput"), Capability("terminal"));
        assert_eq!(signal_rule("org.geckoOS.notification", "NotificationShown"), Capability("notifications:manage"));
        assert_eq!(signal_rule("org.geckoOS.settings", "SettingChanged"), Public);
        assert_eq!(signal_rule("org.geckoOS.time", "TimezoneChanged"), Public);
        assert_eq!(signal_rule("org.geckoOS.privilege", "PrivilegeChanged"), Public);
        assert_eq!(signal_rule("org.geckoOS.privilege", "PrivilegeRequested"), Capability("windows:manage"));
        assert_eq!(signal_rule("org.geckoOS.evo", "OutputsChanged"), Capability("settings:write"));
        assert_eq!(signal_rule("org.geckoOS.nova", "PermissionsChanged"), Capability("settings:write"));
        assert_eq!(signal_rule("org.geckoOS.installer", "InstallFailed"), Capability("appManager:install"));
    }

    #[test]
    fn unlisted_system_signals_are_privileged() {
        assert_eq!(signal_rule("org.geckoOS.Echo", "Shutdown"), Privileged);
        assert_eq!(signal_rule("org.geckoOS.evo", "Nonsense"), Privileged);
        assert_eq!(signal_rule("org.geckoOS.terminal", "constructor"), Privileged);
        assert_eq!(signal_rule("com.example.thing", "Anything"), Public);
    }

    #[test]
    fn emit_policy() {
        assert!(may_emit(0, 0, "org.geckoOS.evo"));
        assert!(may_emit(500, 500, "org.geckoOS.evo"));
        assert!(!may_emit(1000, 0, "org.geckoOS.evo"));
        assert!(!may_emit(1001, 0, "org.geckoOS.session"));
        assert!(may_emit(1001, 0, "com.example.app"));
    }

    #[test]
    fn privileged_peers_hear_everything() {
        for uid in [0u32, 1000, 777] {
            assert!(may_hear(Privileged, uid, 777, Vec::new));
            assert!(may_hear(Capability("terminal"), uid, 777, Vec::new));
        }
    }

    #[test]
    fn app_without_terminal_does_not_hear_terminal_output() {
        let rule = signal_rule("org.geckoOS.terminal", "TerminalOutput");
        assert!(!may_hear(rule, 1001, 0, || vec!["audio".into()]));
        assert!(!may_hear(rule, 1001, 0, Vec::new));
        assert!(may_hear(rule, 1001, 0, || vec!["terminal".into()]));
        // public signals reach it, privileged ones never do
        assert!(may_hear(signal_rule("org.geckoOS.time", "TimezoneChanged"), 1001, 0, Vec::new));
        assert!(!may_hear(signal_rule("org.geckoOS.Echo", "Shutdown"), 1001, 0, || vec!["terminal".into()]));
    }

    #[test]
    fn caps_not_resolved_for_privileged_or_public() {
        let _ = may_hear(Capability("x"), 1000, 0, || panic!("resolved"));
        let _ = may_hear(Public, 1001, 0, || panic!("resolved"));
    }
}
