//! Nova — Pyxis web-app runtime host.
//!
//! Per `nova-runtime-host-plan.md`: Nova owns app *identity* for web apps —
//! `manifest.webapp` discovery/validation, `pyxis://` origin mapping,
//! install/uninstall with integrity verification, single-instance policy,
//! per-app profile layout, permission declarations, and launch composition
//! (turning a manifest into the concrete `vela --pyxis-app <id>`
//! invocation). It does **not** duplicate process supervision or Wayland
//! surface binding — [`crate::services::apphost::AppHost`] still owns that;
//! Nova generates the derived native `manifest.json` AppHost already knows
//! how to read and then calls `AppHost::launch` unchanged.
//!
//! Implemented **in-process** inside the `echo` daemon, exactly like
//! [`crate::services::apphost::AppHost`] — a process-global singleton whose
//! methods are wired to `org.geckoOS.nova` IPC handlers in
//! `echo/src/bin/daemon.rs`, rather than as a separate binary that would
//! need its own duplex Echo transport (a mechanism nothing else in this
//! codebase actually exercises yet — every real external integration point
//! either only calls out, like `gecko-shell`, or only emits/subscribes,
//! like evo's window signals). This mirrors the doc's own architecture
//! ("Nova decides what/how; appHost runs it") while reusing the one
//! request-dispatch mechanism already proven to work here.
//!
//! Registry storage is a flat JSON index (`registry.json`), not sqlite —
//! the plan explicitly allows this ("simpler if the app count stays tiny"),
//! and it avoids adding a new heavyweight dependency for what is, at this
//! scale, a handful of installed apps.
//!
//! Integrity verification uses a SHA-256 digest (`manifest.webapp`'s
//! `integrity.sha256`) rather than full `ed25519` package signing. This is
//! a deliberate v1 scope cut, not an oversight: it closes the actual gap
//! flagged in `appmanager.rs`/`CLAUDE.md` (installing a tarball with zero
//! integrity check at all) without pulling in a new signing/trust-root
//! dependency chain in the same pass that first stands Nova up. Real
//! signature verification (trust roots, key rotation) is the natural next
//! step once Nova has a real installed base to protect.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use crate::services::apphost::AppHost;
use crate::IPCBus;

/// Interface name for Nova IPC + signals.
pub const NOVA_IFACE: &str = "org.geckoOS.nova";

/// System (read-only, shipped with the image) app root.
const SYSTEM_APPS_ROOT: &str = "/usr/share/geckoOS/apps";
/// User-installed (writable data partition) app root.
const USER_APPS_ROOT: &str = "/var/lib/pyxis/apps";
/// opkg/ipk-installed app root (SDK/PDK packaging spec §5/§10:
/// `vroom/docs/pyxis-sdk-pdk-spec.md`). Apps here carry `appinfo.json`
/// (webOS-style), not `manifest.webapp`; both runtimes (`web`/`native`)
/// are allowed. Packages land here via `opkg install` of a Vroom-built
/// `.ipk`; their `postinst` calls `pyxis-appctl rescan` → `nova.Reload`.
const PYXIS_APPS_ROOT: &str = "/usr/pyxis/applications";
/// Nova-owned root for the derived native `manifest.json` of ipk apps.
/// Deliberately NOT the app dir itself: a web app's staged payload may
/// legitimately contain its own `manifest.json` (a PWA manifest), and
/// `/usr/pyxis/applications` is opkg-owned — Nova must not write into
/// either. AppHost scans this as a third root.
pub const NOVA_NATIVE_ROOT: &str = "/var/lib/pyxis/nova/native";
/// Per-app Gecko profile root.
const PROFILES_ROOT: &str = "/var/lib/pyxis/profiles";
/// Registry index file.
const REGISTRY_FILE: &str = "/var/lib/pyxis/nova/registry.json";
/// Origin map consumed by `PyxisOriginMap` (netwerk/protocol/pyxis/).
const ORIGINS_MAP_FILE: &str = "/run/pyxis/nova/origins.map";
/// Path to the real `vela` binary (matches `gecko/src/gecko.rs`'s
/// `DEFAULT_VELA_PATH` and `scripts/create-rootfs.sh`'s install
/// location).
const VELA_BIN: &str = "/usr/lib/pyxis-gecko/vela";

// --- Unprivileged launch (mirrors scripts/create-rootfs.sh's boot-script
// setpriv drop). AppHost runs inside echo-daemon as root, but Gecko/vela
// refuses to run as root, so Nova composes the launch command through
// `setpriv` to drop to the unprivileged geckouser before exec'ing vela. The
// `--groups video,render` membership is what grants DRI/GPU access (hardware
// GL instead of a software fallback). These MUST stay in lockstep with the
// user/group/paths create-rootfs.sh sets up.
const GECKO_UID: u32 = 1000;
const GECKO_GID: u32 = 1000;
const GECKO_HOME: &str = "/home/geckouser";
const SETPRIV_BIN: &str = "/usr/bin/setpriv";
/// Comma-separated supplementary groups for DRI/GPU device access.
const GECKO_GROUPS: &str = "video,render";
/// Session runtime dir + Wayland socket name Evo creates (see the boot
/// script, which chown's both to geckouser so the dropped vela can connect).
const XDG_RUNTIME_DIR: &str = "/run/user/1000";
const WAYLAND_DISPLAY: &str = "wayland-0";

/// The full capability vocabulary a `manifest.webapp` may declare in
/// `permissions[]`. Kept in lockstep with
/// `PyxisBridgeParent.sys.mjs`'s `_capabilityFor()` table — that table maps
/// bridge (service, method) pairs to one of these strings; this is the
/// vocabulary those strings must come from. Unknown permissions are a hard
/// validation error at parse time (fail loudly at install/discovery, not
/// silently at first use).
pub const KNOWN_CAPABILITIES: &[&str] = &[
    "filesystem:read",
    "filesystem:write",
    "power",
    "notifications",
    "appManager:install",
    "appManager:manage",
];

/// The privileged subset of [`KNOWN_CAPABILITIES`] — the colon-vocabulary
/// counterpart of [`PRIVILEGED_APPINFO_PERMISSIONS`]. Gated identically:
/// declaring is free, holding requires [`PRIVILEGED_APP_IDS`].
///
/// This matters for `manifest.webapp` apps and not just ipk ones, because
/// `USER_APPS_ROOT` is writable and fed by `install()` from plain tarballs.
/// Gating only the `appinfo.json` path would leave the identical escalation
/// reachable by shipping the same app as a `.tar.gz` instead of an `.ipk`.
pub const PRIVILEGED_CAPABILITIES: &[&str] = &["appManager:install", "appManager:manage"];

/// The permission vocabulary an `appinfo.json` may declare in
/// `requiredPermissions[]` (SDK/PDK spec §19.1, kept in lockstep with
/// `vroom/schemas/appinfo.schema.json` and vroom's `config::PERMISSIONS`).
/// This is the *declaration* vocabulary; only the subset with an entry in
/// [`appinfo_permission_to_capability`] maps onto a bridge capability
/// today — everything else is recorded but grants nothing (deny-by-default
/// at the bridge, per spec §19.2 "installation does not imply runtime
/// authorization").
pub const APPINFO_PERMISSIONS: &[&str] = &[
    "network",
    "notifications",
    "camera",
    "microphone",
    "location",
    "bluetooth",
    "filesystem.read",
    "filesystem.write",
    "ipc",
    "background",
    "power",
    "appmanager.install",
    "appmanager.manage",
];

/// The privileged subset of [`APPINFO_PERMISSIONS`]: app-management rights,
/// which let their holder install and remove *other* apps. Any `appinfo.json`
/// may declare these (declaration is not the grant — spec §19.2), but they
/// are translated into bridge capabilities only for apps on
/// [`PRIVILEGED_APP_IDS`]. Without that gate an ipk-installed app could
/// declare `appmanager.install` and self-elevate into the app-management
/// surface simply by being installed, which is precisely the escalation the
/// deny-by-default model exists to prevent.
pub const PRIVILEGED_APPINFO_PERMISSIONS: &[&str] = &["appmanager.install", "appmanager.manage"];

/// Platform trust list for [`PRIVILEGED_APPINFO_PERMISSIONS`].
///
/// This is the interim stand-in for a real signing trust root: the list is
/// compiled into Nova and therefore baked into the image, so an `.ipk`
/// cannot add itself to it (unlike a marker file under an opkg-writable
/// path). Being installed to the root-owned `/usr/pyxis/applications` is
/// *not* sufficient on its own — every ipk app lands there, so that path
/// would gate nothing.
///
/// When package signature verification lands (see this file's header
/// comment), this should become "the signing key chains to a platform trust
/// root" and the hard-coded id list should go away.
///
/// Must stay in lockstep with the `permissions[]` the staged app manifests in
/// `scripts/create-rootfs.sh` declare: an app that declares `appManager:*`
/// but is missing here silently loses the capability (the withholding is
/// logged, but the app just stops working). Currently:
/// - `bloom` — the desktop shell, which surfaces install/uninstall in the UI.
/// - `installer` — drives `org.geckoOS.installer` for `.ipk` install/remove.
pub const PRIVILEGED_APP_IDS: &[&str] = &["bloom", "installer"];

/// Whether `app_id` is on the platform trust list. This is necessary but not
/// sufficient for a privileged grant — the app's tier must also permit it
/// (see `AppTier::may_hold_privileged`), which is what stops a user-installed
/// app from inheriting authority by claiming a trusted id.
pub fn app_id_is_privileged(app_id: &str) -> bool {
    PRIVILEGED_APP_IDS.contains(&app_id)
}

/// Map a declared appinfo permission onto Nova's bridge-capability
/// vocabulary ([`KNOWN_CAPABILITIES`]), or `None` when no enforcement
/// point exists for it yet.
///
/// This is a pure vocabulary translation and says nothing about whether the
/// capability is *granted*; for the privileged entries the caller must also
/// clear [`app_id_is_privileged`]. Use [`appinfo_capabilities_for`] to get
/// the gate and the translation together.
pub fn appinfo_permission_to_capability(perm: &str) -> Option<&'static str> {
    match perm {
        "filesystem.read" => Some("filesystem:read"),
        "filesystem.write" => Some("filesystem:write"),
        "notifications" => Some("notifications"),
        "power" => Some("power"),
        "appmanager.install" => Some("appManager:install"),
        "appmanager.manage" => Some("appManager:manage"),
        _ => None,
    }
}

/// Translate an app's declared `requiredPermissions[]` into the bridge
/// capabilities it is actually granted, dropping privileged entries the app
/// is not entitled to and logging each refusal.
///
/// A privileged grant needs *both* halves: the id must be on the trust list
/// and the tier must be root-mediated. Checking only the id would let a
/// user-installed app claim a trusted id and inherit its authority; checking
/// only the tier would grant app management to every ipk on the system.
///
/// Single choke point on purpose: the gate is only sound if *every* path
/// from a declaration to a capability goes through it.
pub fn appinfo_capabilities_for(app_id: &str, tier: AppTier, declared: &[String]) -> Vec<String> {
    let privileged_ok = app_id_is_privileged(app_id) && tier.may_hold_privileged();
    declared
        .iter()
        .filter_map(|p| {
            let cap = appinfo_permission_to_capability(p)?;
            if PRIVILEGED_APPINFO_PERMISSIONS.contains(&p.as_str()) && !privileged_ok {
                tracing::warn!(
                    "nova: app '{}' (tier {}) declares privileged permission '{}' but is \
                     not entitled to it; capability '{}' withheld",
                    app_id,
                    tier.as_str(),
                    p,
                    cap
                );
                return None;
            }
            Some(cap.to_string())
        })
        .collect()
}

// ---------- Manifest ----------

/// Declarative web-app manifest (`manifest.webapp`) — the app author's
/// contract. Richer than `AppHost`'s native `manifest.json`, which Nova
/// derives from this at launch time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebAppManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default = "default_type")]
    pub r#type: String,
    #[serde(default = "default_entry")]
    pub entry: String,
    #[serde(default)]
    pub icons: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default = "default_true")]
    pub single_instance: bool,
    #[serde(default = "default_display")]
    pub display: String,
    #[serde(default)]
    pub background: bool,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<Integrity>,
}

fn default_type() -> String {
    "web".to_string()
}
fn default_entry() -> String {
    "index.html".to_string()
}
fn default_true() -> bool {
    true
}
fn default_display() -> String {
    "windowed".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Integrity {
    #[serde(default)]
    pub sha256: Option<String>,
}

impl WebAppManifest {
    /// Parse + validate a `manifest.webapp` JSON blob. Enforces the schema
    /// rules `nova-runtime-host-plan.md` §3 calls out: `id` matches the
    /// install dir, `entry` has no path traversal, every declared
    /// permission is from the known vocabulary.
    fn parse(raw: &str, dir_id: &str) -> Result<Self> {
        let manifest: WebAppManifest =
            serde_json::from_str(raw).context("parse manifest.webapp")?;
        if manifest.id != dir_id {
            return Err(anyhow!(
                "manifest.webapp id '{}' does not match install dir '{}'",
                manifest.id,
                dir_id
            ));
        }
        if manifest.entry.contains("..") || manifest.entry.starts_with('/') {
            return Err(anyhow!(
                "manifest.webapp entry '{}' is not a safe relative path",
                manifest.entry
            ));
        }
        for perm in &manifest.permissions {
            if !KNOWN_CAPABILITIES.contains(&perm.as_str()) {
                return Err(anyhow!(
                    "manifest.webapp declares unknown permission '{}'",
                    perm
                ));
            }
        }
        Ok(manifest)
    }

    fn to_info_json(&self, installed: bool) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "version": self.version,
            "type": self.r#type,
            "entry": self.entry,
            "icons": self.icons,
            "category": self.category,
            "singleInstance": self.single_instance,
            "display": self.display,
            "background": self.background,
            "permissions": self.permissions,
            "installed": installed,
        })
    }
}

// ---------- appinfo.json (ipk apps) ----------

/// `appinfo.json` — the installed app manifest of the SDK/PDK packaging
/// spec (§7). Parsed leniently (unknown fields ignored): strict schema
/// validation is the toolchain's job at package time; here the launcher
/// contract (§10) is "validate before indexing, skip+log invalid".
#[derive(Debug, Clone, Deserialize)]
pub struct AppInfoManifest {
    pub id: String,
    pub version: String,
    #[allow(dead_code)]
    pub vendor: String,
    pub title: String,
    #[serde(rename = "type")]
    pub app_type: String,
    pub main: String,
    pub icon: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub fullscreen: bool,
    #[serde(default, rename = "requiredPermissions")]
    pub required_permissions: Vec<String>,
    #[serde(default, rename = "entryArgs")]
    pub entry_args: Vec<String>,
    #[serde(default)]
    pub services: Vec<String>,
}

impl AppInfoManifest {
    /// Parse + validate per the launcher contract (spec §10/§20): id must
    /// match the install dir, web `main` must be a safe relative path,
    /// native `main` must be absolute, permissions must come from the §19
    /// vocabulary.
    fn parse(raw: &str, dir_id: &str) -> Result<Self> {
        let info: AppInfoManifest = serde_json::from_str(raw).context("parse appinfo.json")?;
        if info.id != dir_id {
            return Err(anyhow!(
                "appinfo.json id '{}' does not match install dir '{}'",
                info.id,
                dir_id
            ));
        }
        match info.app_type.as_str() {
            "web" => {
                if info.main.starts_with('/') || info.main.contains("..") {
                    return Err(anyhow!(
                        "appinfo.json web main '{}' is not a safe relative path",
                        info.main
                    ));
                }
            }
            "native" => {
                if !info.main.starts_with('/') {
                    return Err(anyhow!(
                        "appinfo.json native main '{}' must be an absolute path",
                        info.main
                    ));
                }
            }
            other => return Err(anyhow!("appinfo.json type '{}' is not web|native", other)),
        }
        for perm in &info.required_permissions {
            if !APPINFO_PERMISSIONS.contains(&perm.as_str()) {
                return Err(anyhow!(
                    "appinfo.json declares unknown permission '{}'",
                    perm
                ));
            }
        }
        Ok(info)
    }
}

/// Where a registry entry's manifest came from — governs where the derived
/// native `manifest.json` may be written and who owns uninstall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestSource {
    /// `manifest.webapp` under the system/user app roots; Nova owns the
    /// full install/uninstall lifecycle.
    WebApp,
    /// `appinfo.json` under `/usr/pyxis/applications`; opkg owns the files,
    /// Nova only indexes/launches.
    AppInfo,
}

/// An app's *provenance*: which root it was discovered in, and therefore who
/// put it there and how much authority that implies.
///
/// This is a different axis from [`ManifestSource`], which only says what
/// file format the manifest was in. Conflating the two is what let a
/// user-installed app inherit a system app's authority: both live under
/// `manifest.webapp`, so nothing downstream could tell "shipped in the image"
/// apart from "a tarball someone dropped in the writable root".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTier {
    /// Shipped in the image under `SYSTEM_APPS_ROOT`. Present only because
    /// the image was built with it.
    System,
    /// Installed by `opkg` under `PYXIS_APPS_ROOT`. Getting here requires
    /// root, so an administrator made a deliberate decision.
    Opkg,
    /// Installed at runtime into `USER_APPS_ROOT` via `nova.Install`. The
    /// least trusted tier: an ordinary tarball, unsigned, no root required.
    User,
}

impl AppTier {
    /// Which manifest format this tier's apps carry.
    fn manifest_source(self) -> ManifestSource {
        match self {
            AppTier::System | AppTier::User => ManifestSource::WebApp,
            AppTier::Opkg => ManifestSource::AppInfo,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AppTier::System => "system",
            AppTier::Opkg => "opkg",
            AppTier::User => "user",
        }
    }

    /// Whether apps in this tier may hold privileged capabilities at all.
    ///
    /// Root-mediated tiers may (subject to the id trust list); the user tier
    /// never may, regardless of id. This is the check that makes the trust
    /// list safe to key on app id: without it, dropping a tarball named
    /// `installer` into the writable root would inherit the real installer's
    /// authority.
    pub fn may_hold_privileged(self) -> bool {
        matches!(self, AppTier::System | AppTier::Opkg)
    }

    /// Whether this tier's ids are owned by a root-mediated installer, and so
    /// may not be claimed by a user-installed app.
    pub fn owns_its_ids(self) -> bool {
        self.may_hold_privileged()
    }

    /// Whether `nova.Uninstall` may delete this app's files. Nova removes
    /// only what Nova installed: image payloads belong to the image, and ipk
    /// payloads belong to opkg's status database.
    pub fn removable_by_nova(self) -> bool {
        matches!(self, AppTier::User)
    }
}

/// Whether an app arriving at `claimant` tier may take an id currently held
/// by `incumbent` (`None` when the id is free).
///
/// Root-mediated tiers may shadow anything — that is the documented ipk
/// update path. The user tier may not displace an id a root-mediated tier
/// owns, which is what stops a tarball from impersonating a shipped app.
///
/// Shared by `reload()` (discovery) and `install()` (admission) so the two
/// cannot drift: an install that passes admission must also survive the next
/// rescan, or it would report success and then vanish.
pub fn tier_may_claim_id(claimant: AppTier, incumbent: Option<AppTier>) -> bool {
    match incumbent {
        Some(held) => claimant.owns_its_ids() || !held.owns_its_ids(),
        None => true,
    }
}

/// One registry entry: manifest + where its webroot actually lives.
#[derive(Debug, Clone)]
struct RegistryEntry {
    manifest: WebAppManifest,
    webroot: PathBuf,
    app_dir: PathBuf,
    /// Provenance — governs privilege eligibility, id ownership, and
    /// uninstallability. The manifest format is derived from this
    /// (`RegistryEntry::source`) rather than stored, so the two cannot drift.
    tier: AppTier,
    /// `Some(absolute exec path)` for ipk native apps; `None` for web apps.
    native_main: Option<String>,
    /// `entryArgs` for ipk native apps.
    entry_args: Vec<String>,
    /// The verbatim `requiredPermissions[]` an appinfo declared (dot
    /// vocabulary). The bridge-capability subset lives in
    /// `manifest.permissions`; this is kept for display/tooling.
    raw_permissions: Vec<String>,
}

impl RegistryEntry {
    /// Which manifest format this entry was loaded from.
    fn source(&self) -> ManifestSource {
        self.tier.manifest_source()
    }

    fn is_native(&self) -> bool {
        self.native_main.is_some()
    }

    /// Manifest projection for List/GetInfo, decorated with the
    /// registry-level fields the plain `WebAppManifest` doesn't carry.
    fn info_json(&self) -> Value {
        let mut info = self.manifest.to_info_json(true);
        info["source"] = json!(match self.source() {
            ManifestSource::WebApp => "manifest.webapp",
            ManifestSource::AppInfo => "appinfo.json",
        });
        info["tier"] = json!(self.tier.as_str());
        // So the launcher/installer UI can hide or disable "Remove" instead
        // of offering it and surfacing an error when it's pressed.
        info["removable"] = json!(self.tier.removable_by_nova());
        if self.is_native() {
            info["type"] = json!("native");
        }
        if !self.raw_permissions.is_empty() {
            info["requiredPermissions"] = json!(self.raw_permissions);
        }
        info
    }
}

// ---------- Nova ----------

pub struct Nova {
    /// Keyed by app id.
    registry: RwLock<HashMap<String, RegistryEntry>>,
    bus: RwLock<Option<Arc<IPCBus>>>,
}

impl Nova {
    fn new() -> Self {
        Self {
            registry: RwLock::new(HashMap::new()),
            bus: RwLock::new(None),
        }
    }

    pub fn global() -> &'static Arc<Nova> {
        static NOVA: OnceLock<Arc<Nova>> = OnceLock::new();
        NOVA.get_or_init(|| Arc::new(Nova::new()))
    }

    pub fn set_bus(&self, bus: Arc<IPCBus>) {
        if let Ok(mut slot) = self.bus.write() {
            *slot = Some(bus);
        }
    }

    fn bus_snapshot(&self) -> Option<Arc<IPCBus>> {
        self.bus.read().ok().and_then(|g| g.clone())
    }

    fn emit(&self, signal_name: &'static str, payload: Value) {
        let Some(bus) = self.bus_snapshot() else {
            return;
        };
        tokio::spawn(async move {
            if let Err(e) = bus.emit_signal(NOVA_IFACE, signal_name, payload).await {
                tracing::warn!("Nova signal '{}' failed: {}", signal_name, e);
            }
        });
    }

    // ---------- Registry / discovery ----------

    /// Scan all app roots (system first, then ipk-installed, then user —
    /// later entries win on id collision, matching "system-first-then-user"
    /// resolution from the plan) and rebuild the in-memory registry.
    ///
    /// The id-collision order is load-bearing, not incidental: the
    /// first-party apps (`notes`, `contacts`, …) ship in the image under
    /// `SYSTEM_APPS_ROOT` *and* can be re-packaged as `.ipk` under the same
    /// id, so an `opkg install notes_0.2.0_all.ipk` deliberately shadows the
    /// ROM copy — updating a shipped app without rebuilding the image, the
    /// same "updated app shadows system app" model webOS/Android use. Each
    /// root keeps its own payload layout (system/user: `<dir>/webroot/`;
    /// ipk: the app dir itself), so shadowing swaps the whole entry cleanly.
    ///
    /// Shadowing is *not* symmetric, though. Only root-mediated tiers may
    /// claim an id: a user-installed app whose id collides with a system or
    /// opkg app is rejected outright rather than shadowing it. Otherwise
    /// dropping a tarball named `installer` into the writable user root would
    /// both impersonate the real installer in the launcher and, before the
    /// tier check existed, inherit its app-management authority.
    ///
    /// The system/user roots hold `manifest.webapp` apps; the applications root holds
    /// opkg-installed `appinfo.json` apps (SDK/PDK spec §10's launcher
    /// contract). Bad manifests are skipped with a warning, not a hard
    /// failure — one broken app can't wedge discovery for the rest.
    pub fn reload(&self) -> Result<usize> {
        let mut found: HashMap<String, RegistryEntry> = HashMap::new();
        for (root, tier) in [
            (Path::new(SYSTEM_APPS_ROOT), AppTier::System),
            (Path::new(PYXIS_APPS_ROOT), AppTier::Opkg),
            (Path::new(USER_APPS_ROOT), AppTier::User),
        ] {
            if !root.exists() {
                continue;
            }
            let entries = match fs::read_dir(root) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("nova: read_dir {} failed: {}", root.display(), e);
                    continue;
                }
            };
            for entry in entries.flatten() {
                let app_dir = entry.path();
                if !app_dir.is_dir() {
                    continue;
                }
                let id = match app_dir.file_name().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                // A user-tier app may not take an id a root-mediated tier
                // already owns. Roots are scanned in trust order, so any
                // incumbent here was installed with at least as much
                // authority as the app now claiming the id.
                let incumbent = found.get(&id).map(|e| e.tier);
                if !tier_may_claim_id(tier, incumbent) {
                    tracing::warn!(
                        "nova: refusing app at {}: id '{}' is owned by a {} app",
                        app_dir.display(),
                        id,
                        incumbent.map_or("?", AppTier::as_str)
                    );
                    continue;
                }
                let loaded = match tier.manifest_source() {
                    ManifestSource::WebApp => Self::load_one(&app_dir, &id, tier),
                    ManifestSource::AppInfo => Self::load_one_appinfo(&app_dir, &id, tier),
                };
                match loaded {
                    Ok(entry) => {
                        found.insert(id, entry);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "nova: skipping invalid app at {}: {}",
                            app_dir.display(),
                            e
                        );
                    }
                }
            }
        }
        let count = found.len();
        if let Ok(mut reg) = self.registry.write() {
            *reg = found;
        }
        self.write_origins_map()?;
        // Pre-generate the derived native manifest.json for every registered
        // app. AppHost lists and launches from manifest.json (not
        // manifest.webapp), and the shell's launcher reads AppHost.ListApps —
        // so without generating these up front, only apps that had already
        // been launched once (nova.Launch generates lazily too) would ever
        // appear in the dock/launcher. Best-effort per app: a read-only
        // system root or one unwritable app must not wedge the whole reload.
        if let Ok(reg) = self.registry.read() {
            for entry in reg.values() {
                if let Err(e) = self.write_native_manifest(entry) {
                    tracing::warn!(
                        "nova: native manifest generation for '{}' failed: {}",
                        entry.manifest.id,
                        e
                    );
                }
                // Pre-create the geckouser-owned profile dir too, so a launch
                // that reaches AppHost directly (rather than through
                // nova.Launch, which also ensures this) doesn't fail on a
                // missing/root-owned profile. nova.Launch remains the
                // preferred path — it additionally enforces single-instance.
                // Native ipk apps don't run under vela and need no profile.
                if !entry.is_native() {
                    if let Err(e) = self.ensure_profile_dir(&entry.manifest.id) {
                        tracing::warn!(
                            "nova: profile dir setup for '{}' failed: {}",
                            entry.manifest.id,
                            e
                        );
                    }
                }
            }
        }
        self.cleanup_stale_native_manifests();
        self.persist_registry();
        self.emit("RegistryChanged", json!({ "count": count }));
        Ok(count)
    }

    fn load_one(app_dir: &Path, id: &str, tier: AppTier) -> Result<RegistryEntry> {
        let manifest_path = app_dir.join("manifest.webapp");
        let raw = fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let mut manifest = WebAppManifest::parse(&raw, id)?;
        // Same trust gate the appinfo path applies, on the colon vocabulary:
        // a manifest.webapp app under the writable USER_APPS_ROOT must not
        // reach app-management just by declaring it.
        if !(app_id_is_privileged(id) && tier.may_hold_privileged()) {
            manifest.permissions.retain(|p| {
                let privileged = PRIVILEGED_CAPABILITIES.contains(&p.as_str());
                if privileged {
                    tracing::warn!(
                        "nova: app '{}' (tier {}) declares privileged capability '{}' but \
                         is not entitled to it; withheld",
                        id,
                        tier.as_str(),
                        p
                    );
                }
                !privileged
            });
        }
        let webroot = app_dir.join("webroot");
        if !webroot.is_dir() {
            return Err(anyhow!("no webroot/ directory under {}", app_dir.display()));
        }
        Ok(RegistryEntry {
            manifest,
            webroot,
            app_dir: app_dir.to_path_buf(),
            tier,
            native_main: None,
            entry_args: Vec::new(),
            raw_permissions: Vec::new(),
        })
    }

    /// Load an ipk-installed app: parse+validate `appinfo.json`, then
    /// project it into the internal `WebAppManifest` shape the rest of Nova
    /// (origins map, launch composition, info projection) already speaks.
    /// Declared permissions are translated to bridge capabilities via
    /// [`appinfo_capabilities_for`]; unmapped ones — and privileged ones the
    /// app is not entitled to — are kept only in `raw_permissions` and grant
    /// nothing.
    fn load_one_appinfo(app_dir: &Path, id: &str, tier: AppTier) -> Result<RegistryEntry> {
        let manifest_path = app_dir.join("appinfo.json");
        let raw = fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let info = AppInfoManifest::parse(&raw, id)?;

        let capabilities = appinfo_capabilities_for(id, tier, &info.required_permissions);

        let is_native = info.app_type == "native";
        let mut icons = HashMap::new();
        icons.insert(
            "64".to_string(),
            app_dir.join(&info.icon).to_string_lossy().to_string(),
        );

        let manifest = WebAppManifest {
            id: info.id.clone(),
            name: info.title.clone(),
            version: info.version.clone(),
            r#type: info.app_type.clone(),
            // For web apps `entry` is the relative main document; native
            // apps carry their exec path in `native_main` instead and this
            // field is unused.
            entry: if is_native {
                default_entry()
            } else {
                info.main.clone()
            },
            icons,
            category: info.category.clone(),
            // GUI apps launched from the shell: one card per app.
            single_instance: true,
            display: if info.fullscreen {
                "fullscreen".to_string()
            } else {
                "windowed".to_string()
            },
            background: info.required_permissions.iter().any(|p| p == "background"),
            permissions: capabilities,
            integrity: None,
        };

        Ok(RegistryEntry {
            manifest,
            // The ipk staging layout puts the web payload flat in the app
            // root (spec §14.5), so the app dir itself is the webroot.
            webroot: app_dir.to_path_buf(),
            app_dir: app_dir.to_path_buf(),
            tier,
            native_main: is_native.then(|| info.main.clone()),
            entry_args: info.entry_args,
            raw_permissions: info.required_permissions,
        })
    }

    /// The tier of the currently-registered app with this id, if any.
    fn tier_of(&self, app_id: &str) -> Option<AppTier> {
        self.registry.read().ok()?.get(app_id).map(|e| e.tier)
    }

    /// Drop derived manifests under [`NOVA_NATIVE_ROOT`] whose app id is no
    /// longer in the registry — without this, an `opkg remove`d app would
    /// keep appearing in `AppHost::list_apps` forever.
    fn cleanup_stale_native_manifests(&self) {
        let root = Path::new(NOVA_NATIVE_ROOT);
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        let reg = match self.registry.read() {
            Ok(r) => r,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            let Some(id) = dir.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if !reg.contains_key(id) {
                if let Err(e) = fs::remove_dir_all(&dir) {
                    tracing::warn!("nova: removing stale {} failed: {}", dir.display(), e);
                }
            }
        }
    }

    /// Write `/run/pyxis/nova/origins.map` in the `<app-id>\t<webroot>\n`
    /// format `PyxisOriginMap` (netwerk/protocol/pyxis/) reads.
    fn write_origins_map(&self) -> Result<()> {
        let path = Path::new(ORIGINS_MAP_FILE);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let reg = self
            .registry
            .read()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        let mut out = String::new();
        for entry in reg.values() {
            // Native apps have no web origin — nothing for vela to map.
            if entry.is_native() {
                continue;
            }
            out.push_str(&entry.manifest.id);
            out.push('\t');
            out.push_str(&entry.webroot.to_string_lossy());
            out.push('\n');
        }
        fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    fn persist_registry(&self) {
        let path = Path::new(REGISTRY_FILE);
        let Some(parent) = path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let reg = match self.registry.read() {
            Ok(r) => r,
            Err(_) => return,
        };
        let projected: Vec<Value> = reg.values().map(|e| e.info_json()).collect();
        if let Ok(text) = serde_json::to_string_pretty(&projected) {
            let _ = fs::write(path, text);
        }
    }

    // ---------- Queries ----------

    pub fn list(&self) -> Result<Value> {
        let reg = self
            .registry
            .read()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        let mut apps: Vec<Value> = reg.values().map(|e| e.info_json()).collect();
        apps.sort_by(|a, b| {
            a["id"]
                .as_str()
                .unwrap_or("")
                .cmp(b["id"].as_str().unwrap_or(""))
        });
        Ok(json!({ "apps": apps }))
    }

    pub fn get_info(&self, app_id: &str) -> Result<Value> {
        let reg = self
            .registry
            .read()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        let entry = reg
            .get(app_id)
            .ok_or_else(|| anyhow!("app '{}' is not installed", app_id))?;
        let mut info = entry.info_json();
        let running = AppHost::global().find_session_by_app(app_id);
        info["running"] = json!(running.is_some());
        if let Some(s) = running {
            info["sessionId"] = json!(s.session_id);
        }
        Ok(info)
    }

    /// Declared `permissions[]` for an app id, or an empty vec if unknown —
    /// deny-by-default, matching `PyxisBridgeParent.sys.mjs`'s existing
    /// conservative placeholder. This is the real backing store that
    /// placeholder's doc comment says to wire up once Nova exists.
    pub fn declared_permissions(&self, app_id: &str) -> Vec<String> {
        self.registry
            .read()
            .ok()
            .and_then(|reg| reg.get(app_id).map(|e| e.manifest.permissions.clone()))
            .unwrap_or_default()
    }

    // ---------- Launch composition ----------

    /// `nova.Launch(appId, params)` — resolve the manifest, enforce
    /// single-instance policy, ensure the runtime prerequisites (profile
    /// dir, derived native manifest, origins map) exist, then delegate to
    /// `AppHost::launch` unchanged. See `nova-runtime-host-plan.md` §6.
    pub fn launch(&self, app_id: &str, params: Option<Value>) -> Result<Value> {
        let entry = {
            let reg = self
                .registry
                .read()
                .map_err(|_| anyhow!("registry lock poisoned"))?;
            reg.get(app_id)
                .cloned_entry()
                .ok_or_else(|| anyhow!("app '{}' is not installed", app_id))?
        };

        // Single-instance: raise existing session instead of spawning.
        if entry.manifest.single_instance {
            if let Some(existing) = AppHost::global().find_session_by_app(app_id) {
                AppHost::global().set_active(&existing.session_id)?;
                return Ok(json!({
                    "sessionId": existing.session_id,
                    "appId": app_id,
                    "raised": true,
                }));
            }
        }

        if !entry.is_native() {
            self.ensure_profile_dir(app_id)?;
        }
        self.write_native_manifest(&entry)?;

        let session = AppHost::global().launch(app_id, params)?;
        self.emit(
            "AppLaunched",
            json!({ "appId": app_id, "sessionId": session.session_id }),
        );
        Ok(json!({
            "sessionId": session.session_id,
            "appId": app_id,
            "raised": false,
        }))
    }

    pub fn close(&self, app_id: &str) -> Result<Value> {
        let session = AppHost::global()
            .find_session_by_app(app_id)
            .ok_or_else(|| anyhow!("app '{}' is not running", app_id))?;
        let closed = AppHost::global().close(&session.session_id)?;
        Ok(json!({ "appId": app_id, "sessionId": closed.session_id }))
    }

    /// `nova.Focus(appId)` — raise and give keyboard focus to an already-running
    /// app's compositor window. This is the real counterpart to `launch`'s
    /// single-instance raise path: that path only updates `AppHost::set_active`
    /// bookkeeping (which session is "the active one"), it never told Evo to
    /// actually move input focus or restack the window — there was no incoming
    /// Echo IPC command for that at all (see evo/src/enhancements/echo_ipc.rs's
    /// `FocusWindow`, added alongside this). Bloom's dock (an "orphan session"
    /// icon in BottomDock.tsx) calls this when clicking an already-running
    /// app, instead of re-issuing `Launch` and hoping the bookkeeping-only
    /// raise is enough.
    pub async fn focus(&self, app_id: &str) -> Result<Value> {
        let session = AppHost::global()
            .find_session_by_app(app_id)
            .ok_or_else(|| anyhow!("app '{}' is not running", app_id))?;
        let window_id = *session
            .surfaces
            .first()
            .ok_or_else(|| anyhow!("app '{}' has no mapped compositor window yet", app_id))?;

        AppHost::global().set_active(&session.session_id)?;

        let bus = self
            .bus_snapshot()
            .ok_or_else(|| anyhow!("nova: no IPC bus available"))?;
        bus.call_method(
            "org.geckoOS.evo",
            "org.geckoOS.evo",
            "FocusWindow",
            json!({ "window_id": window_id }),
        )
        .await?;

        Ok(json!({
            "appId": app_id,
            "sessionId": session.session_id,
            "windowId": window_id,
        }))
    }

    /// Pseudo app-id used to track external-browser sessions in AppHost
    /// (surfaces, lifecycle signals, suspend/resume) even though there's no
    /// real `manifest.webapp`/`manifest.json` for it — see `open_external_browser`.
    const EXTERNAL_BROWSER_APP_ID: &str = "browser-window";

    /// `nova.OpenExternalBrowser(url)` — open `url` in a real, independent
    /// vela browser window instead of Bloom's `BrowserApp` trying to embed
    /// it in an `<iframe>`.
    ///
    /// The iframe approach cannot work for most real sites: X-Frame-Options/
    /// CSP `frame-ancestors` are a browser-enforced security boundary against
    /// being *framed*, and no amount of client-side code in Bloom can lift
    /// that. A real top-level browsing context isn't framing anything, so
    /// it's never subject to those headers at all — this is the fix the
    /// project's own "apps are independent Wayland clients, Evo composites
    /// everything" architecture already implies (see CLAUDE.md), just
    /// applied to ad hoc external URLs instead of an installed `pyxis://`
    /// app.
    ///
    /// Deliberately does NOT go through `launch`/`write_native_manifest`:
    /// those are keyed on an installed app's manifest (fixed `exec`/`args`),
    /// but here the "args" (the URL) are different on every call and there's
    /// no manifest.webapp/webroot to register. Composes the vela command
    /// directly and hands it to `AppHost::launch_raw`, added alongside this
    /// for exactly this case. No `--pyxis-app`/`--app-entry` (those are for
    /// the internal `pyxis://<id>` origin mapping) and no kiosk flag — this
    /// is a plain `vela <url>` invocation, so the window gets vela's own
    /// normal GTK chrome (titlebar, address bar, tabs) rather than Bloom's.
    /// The profile dir is shared and NOT wiped between calls (unlike the
    /// kiosk boot app's profile) so login sessions/cookies persist across
    /// navigations, matching what a user expects from "a real browser".
    pub fn open_external_browser(&self, url: &str) -> Result<Value> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(anyhow!(
                "refusing to open external browser for non-http(s) url: {}",
                url
            ));
        }

        self.ensure_profile_dir(Self::EXTERNAL_BROWSER_APP_ID)?;
        let profile_dir = Path::new(PROFILES_ROOT)
            .join(Self::EXTERNAL_BROWSER_APP_ID)
            .to_string_lossy()
            .to_string();

        let args = vec![
            format!("--reuid={}", GECKO_UID),
            format!("--regid={}", GECKO_GID),
            "--groups".to_string(),
            GECKO_GROUPS.to_string(),
            VELA_BIN.to_string(),
            "--profile".to_string(),
            profile_dir,
            url.to_string(),
        ];

        let mut env = HashMap::new();
        env.insert("MOZ_ENABLE_WAYLAND".to_string(), "1".to_string());
        env.insert("GDK_BACKEND".to_string(), "wayland".to_string());
        env.insert("XDG_RUNTIME_DIR".to_string(), XDG_RUNTIME_DIR.to_string());
        env.insert("WAYLAND_DISPLAY".to_string(), WAYLAND_DISPLAY.to_string());
        env.insert("HOME".to_string(), GECKO_HOME.to_string());
        // Same decoration handoff every other vela launch uses (Evo draws
        // no SSD; GTK draws its own titlebar) — see evo-firefox-decoration
        // notes / write_native_manifest above.
        env.insert(
            "MOZ_GTK_TITLEBAR_DECORATION".to_string(),
            "none".to_string(),
        );
        env.insert("GTK_CSD".to_string(), "0".to_string());

        let session = AppHost::global().launch_raw(
            Self::EXTERNAL_BROWSER_APP_ID,
            Path::new(SETPRIV_BIN),
            &args,
            &env,
        )?;
        self.emit(
            "AppLaunched",
            json!({ "appId": Self::EXTERNAL_BROWSER_APP_ID, "sessionId": session.session_id }),
        );
        Ok(json!({
            "sessionId": session.session_id,
            "appId": Self::EXTERNAL_BROWSER_APP_ID,
            "url": url,
        }))
    }

    // ---------- Lifecycle: suspend/resume driven by Evo visibility ----------
    //
    // Ports gaia's `suspend_app`/`resume_app` intent (a real gap called out
    // in CLAUDE.md as "worth porting into Nova/appHost rather than
    // discarding") — but gaia's version only flipped an in-memory status
    // enum with no actual process effect. This drives `AppHost::suspend`/
    // `resume` (real SIGSTOP/SIGCONT, see apphost.rs) off Evo's real
    // `WindowMinimized` signal (subscribed in
    // `daemon.rs::register_nova_handlers`), and re-emits app-identity-
    // correlated `AppSuspended`/`AppResumed` signals on `org.geckoOS.nova`,
    // per nova-runtime-host-plan.md §5's "Nova subscribes to evo visibility/
    // focus events and adds app-identity context the shell wants."

    /// Called when Evo's `WindowMinimized{window_id, minimized: true}`
    /// fires. Resolves the window to its owning session via
    /// `AppHost::session_for_surface` and suspends it. Errors (unknown
    /// window, already suspended) are logged, not propagated — this runs
    /// from a signal handler with no caller to return them to.
    pub fn handle_window_minimized(&self, window_id: u32) {
        let Some(session) = AppHost::global().session_for_surface(window_id) else {
            return;
        };
        match AppHost::global().suspend(&session.session_id) {
            Ok(_) => {
                self.emit(
                    "AppSuspended",
                    json!({ "appId": session.app_id, "sessionId": session.session_id }),
                );
            }
            Err(e) => tracing::debug!(
                "nova: suspend for window {} (app '{}') skipped: {}",
                window_id,
                session.app_id,
                e
            ),
        }
    }

    /// Called when Evo's `WindowMinimized{window_id, minimized: false}`
    /// (i.e. restored) fires.
    pub fn handle_window_restored(&self, window_id: u32) {
        let Some(session) = AppHost::global().session_for_surface(window_id) else {
            return;
        };
        match AppHost::global().resume(&session.session_id) {
            Ok(_) => {
                self.emit(
                    "AppResumed",
                    json!({ "appId": session.app_id, "sessionId": session.session_id }),
                );
            }
            Err(e) => tracing::debug!(
                "nova: resume for window {} (app '{}') skipped: {}",
                window_id,
                session.app_id,
                e
            ),
        }
    }

    fn ensure_profile_dir(&self, app_id: &str) -> Result<()> {
        let dir = Path::new(PROFILES_ROOT).join(app_id);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dir)?.permissions();
            perms.set_mode(0o700);
            fs::set_permissions(&dir, perms)?;
            // vela runs as geckouser (uid/gid 1000) via the setpriv drop the
            // generated manifest encodes; the profile dir must be owned by
            // that user or Gecko can't write its profile (prefs, storage,
            // startupCache) — the same ownership the boot script's
            // `chown 1000:1000` establishes for the kiosk app's profile.
            let _ = std::os::unix::fs::chown(&dir, Some(GECKO_UID), Some(GECKO_GID));
        }
        self.write_profile_prefs(&dir)?;
        Ok(())
    }

    /// Write the rendering `user.js` into an app's profile dir. This is the
    /// exact pref set `scripts/create-rootfs.sh` writes for the BOOT app's
    /// profile, replicated here so Nova-*launched* apps (files, browser,
    /// settings, …) get it too — previously ONLY the boot app did, so every
    /// shell-launched app fell back to WebRender's SWGL software compositor
    /// under QEMU's virtio-gpu/virgl (which Firefox blocklists), the exact
    /// "RenderCompositorSWGL failed … no dt" path that renders app content
    /// garbled/blank. Forcing hardware WebRender on + disabling the GPU
    /// process (single-process rendering is more robust in this constrained
    /// VM) matches what already made the boot app render correctly. Written
    /// as `user.js` (re-applied every startup) owned by geckouser so the
    /// unprivileged vela process can read it. Kept in lockstep with
    /// create-rootfs.sh's PREFS heredoc.
    fn write_profile_prefs(&self, dir: &Path) -> Result<()> {
        let prefs = r#"// Pyxis rendering prefs -- kept in lockstep with scripts/create-rootfs.sh.
user_pref("gfx.webrender.all", true);
user_pref("gfx.webrender.software", false);
user_pref("gfx.webrender.compositor", false);
user_pref("gfx.webrender.compositor.force-enabled", false);
user_pref("layers.gpu-process.enabled", false);
user_pref("media.gpu-process-decoder", false);
user_pref("dom.ipc.processCount", 1);
user_pref("browser.tabs.remote.autostart", false);
user_pref("accessibility.force_disabled", 1);
// Single-purpose app window: never resurrect a previous session as a normal
// browser window. A vela that didn't exit cleanly (e.g. the window was closed
// out from under it) would otherwise crash-restore on next launch, opening a
// chromed browser window that bypasses the --pyxis-app chromeless path -- one
// half of the "subsequent launch is a regular window" bug (the other half,
// remoting, is handled by MOZ_NO_REMOTE in write_native_manifest).
user_pref("browser.sessionstore.resume_from_crash", false);
user_pref("browser.sessionstore.max_resumed_crashes", 0);
user_pref("browser.startup.page", 0);
"#;
        let path = dir.join("user.js");
        fs::write(&path, prefs).with_context(|| format!("write {}", path.display()))?;
        #[cfg(unix)]
        {
            // Readable by the geckouser vela runs as (the profile dir is 0700
            // and owned by geckouser, so the file must be too or vela can't
            // read its own prefs).
            let _ = std::os::unix::fs::chown(&path, Some(GECKO_UID), Some(GECKO_GID));
        }
        Ok(())
    }

    /// Generate the derived native `manifest.json` `AppHost::launch` reads,
    /// matching `gecko/src/gecko.rs`'s `compose_launch` env contract
    /// (`MOZ_ENABLE_WAYLAND`, `PYXIS_APP_ID`, `PYXIS_PROFILE_DIR`,
    /// `PYXIS_DISPLAY_MODE`) and the `--pyxis-app` flag (not `--app`,
    /// which collides with Gecko's own native flag — see
    /// `gecko/BUILD_RUNBOOK.md`).
    fn write_native_manifest(&self, entry: &RegistryEntry) -> Result<()> {
        if entry.is_native() {
            return self.write_native_manifest_for_binary(entry);
        }
        let profile_dir = Path::new(PROFILES_ROOT).join(&entry.manifest.id);
        let profile_arg = profile_dir.to_string_lossy().to_string();
        let mut env = HashMap::new();
        env.insert("MOZ_ENABLE_WAYLAND".to_string(), "1".to_string());
        env.insert("GDK_BACKEND".to_string(), "wayland".to_string());
        // Session env vela needs to find Evo's Wayland socket and its own
        // $HOME. setpriv preserves the environment (no --clear-env), so these
        // — applied by AppHost via Command::env — are inherited by vela after
        // the privilege drop. The boot script sets the identical set for the
        // kiosk app; shell-launched apps need them too since AppHost's own
        // environment isn't guaranteed to carry HOME=/home/geckouser.
        env.insert("XDG_RUNTIME_DIR".to_string(), XDG_RUNTIME_DIR.to_string());
        env.insert("WAYLAND_DISPLAY".to_string(), WAYLAND_DISPLAY.to_string());
        env.insert("HOME".to_string(), GECKO_HOME.to_string());
        env.insert("PYXIS_APP_ID".to_string(), entry.manifest.id.clone());
        env.insert("PYXIS_PROFILE_DIR".to_string(), profile_arg.clone());
        env.insert(
            "PYXIS_DISPLAY_MODE".to_string(),
            entry.manifest.display.clone(),
        );
        // Defer window decorations to Evo's server-side title bar rather than
        // letting Firefox draw its own CSD titlebar. Kept in lockstep with
        // the kiosk launch env in scripts/create-rootfs.sh; see that file and
        // evo/src/shell/xdg.rs for the full rationale (Firefox hardcodes CSD
        // on Wayland unless MOZ_GTK_TITLEBAR_DECORATION overrides it; GTK_CSD=0
        // makes gdk request server-side decorations Evo then draws).
        env.insert(
            "MOZ_GTK_TITLEBAR_DECORATION".to_string(),
            "none".to_string(),
        );
        env.insert("GTK_CSD".to_string(), "0".to_string());
        // Force a fresh vela per launch. Without this, relaunching the same
        // app (or a lingering previous vela holding the same profile) is
        // *remoted* into the running instance, which opens a plain browser
        // window instead of re-running the `--pyxis-app` chromeless
        // `openPyxisAppWindow` path — the "first launch is kiosk, subsequent
        // launches are a regular window" bug. Safe because Nova enforces
        // single-instance (it raises a live session rather than spawning a
        // second), so two vela never contend for one profile at once.
        // NOT set for the shared-profile external browser (open_external_browser),
        // which *wants* remoting so extra URLs land in the one real browser.
        env.insert("MOZ_NO_REMOTE".to_string(), "1".to_string());
        if entry.manifest.background {
            env.insert("PYXIS_BACKGROUND".to_string(), "1".to_string());
        }

        // exec = setpriv (as root) which drops to geckouser + grants the
        // video/render groups, then execs vela. See the GECKO_* consts above
        // and scripts/create-rootfs.sh for why this indirection is required
        // (Gecko refuses to run as root; GPU access needs group membership).
        let native = json!({
            "id": entry.manifest.id,
            "name": entry.manifest.name,
            "exec": SETPRIV_BIN,
            "icon": entry.manifest.icons.get("64").cloned().unwrap_or_default(),
            "version": entry.manifest.version,
            "category": entry.manifest.category,
            "args": [
                format!("--reuid={}", GECKO_UID),
                format!("--regid={}", GECKO_GID),
                "--groups".to_string(),
                GECKO_GROUPS.to_string(),
                VELA_BIN.to_string(),
                "--profile".to_string(),
                profile_arg,
                "--pyxis-app".to_string(),
                entry.manifest.id.clone(),
                "--app-entry".to_string(),
                entry.manifest.entry.clone(),
            ],
            "env": env,
        });

        self.write_manifest_json(entry, native)
    }

    /// Derived `manifest.json` for an ipk *native* app: no vela, no Gecko
    /// profile — exec the appinfo `main` binary directly, still through the
    /// setpriv drop to geckouser (packages must not run GUI apps as root)
    /// and with the same Wayland session env, so Evo's PYXIS_APP_ID-based
    /// window-claim contract works for native clients too.
    fn write_native_manifest_for_binary(&self, entry: &RegistryEntry) -> Result<()> {
        let main = entry
            .native_main
            .as_deref()
            .ok_or_else(|| anyhow!("app '{}' has no native main", entry.manifest.id))?;

        let mut env = HashMap::new();
        env.insert("XDG_RUNTIME_DIR".to_string(), XDG_RUNTIME_DIR.to_string());
        env.insert("WAYLAND_DISPLAY".to_string(), WAYLAND_DISPLAY.to_string());
        env.insert("GDK_BACKEND".to_string(), "wayland".to_string());
        env.insert("QT_QPA_PLATFORM".to_string(), "wayland".to_string());
        env.insert("HOME".to_string(), GECKO_HOME.to_string());
        env.insert("PYXIS_APP_ID".to_string(), entry.manifest.id.clone());
        env.insert(
            "PYXIS_DISPLAY_MODE".to_string(),
            entry.manifest.display.clone(),
        );

        let mut args = vec![
            format!("--reuid={}", GECKO_UID),
            format!("--regid={}", GECKO_GID),
            "--groups".to_string(),
            GECKO_GROUPS.to_string(),
            main.to_string(),
        ];
        args.extend(entry.entry_args.iter().cloned());

        let native = json!({
            "id": entry.manifest.id,
            "name": entry.manifest.name,
            "exec": SETPRIV_BIN,
            "icon": entry.manifest.icons.get("64").cloned().unwrap_or_default(),
            "version": entry.manifest.version,
            "category": entry.manifest.category,
            "args": args,
            "env": env,
        });
        self.write_manifest_json(entry, native)
    }

    /// Write a derived `manifest.json` where AppHost will find it.
    fn write_manifest_json(&self, entry: &RegistryEntry, native: Value) -> Result<()> {
        // For manifest.webapp apps it is written next to manifest.webapp —
        // AppHost reads /usr/share/geckoOS/apps/<id>/manifest.json today
        // (see the apps_root() dual-root change alongside this), so writing
        // it into whichever root this app actually lives in (system or
        // user) keeps AppHost's existing lookup unchanged. For ipk apps the
        // app dir is opkg-owned (and a web payload may carry its own PWA
        // manifest.json), so the derived manifest goes to the Nova-owned
        // NOVA_NATIVE_ROOT, which AppHost scans as a third root.
        let out_path = match entry.source() {
            ManifestSource::WebApp => entry.app_dir.join("manifest.json"),
            ManifestSource::AppInfo => {
                let dir = Path::new(NOVA_NATIVE_ROOT).join(&entry.manifest.id);
                fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
                dir.join("manifest.json")
            }
        };
        let text = serde_json::to_string_pretty(&native)?;
        fs::write(&out_path, text).with_context(|| format!("write {}", out_path.display()))?;
        Ok(())
    }

    // ---------- Install / uninstall ----------

    /// Install a packaged app (a `.tar.gz`/`.tar` containing
    /// `manifest.webapp` + `webroot/` at its root) into the user apps root.
    /// Verifies `integrity.sha256` if the manifest declares one *before*
    /// touching disk further than staging; unsigned/unhashed packages are
    /// accepted (dev policy) but logged loudly, matching the "reject unless
    /// --allow-unsigned" spirit without yet having a signing trust-root
    /// story (see this file's header comment).
    pub fn install(&self, package_path: &str) -> Result<Value> {
        let package = Path::new(package_path);
        if !package.exists() {
            return Err(anyhow!("install source not found: {}", package_path));
        }

        let user_root = Path::new(USER_APPS_ROOT);
        fs::create_dir_all(user_root)
            .with_context(|| format!("create {}", user_root.display()))?;

        let staging = user_root.join(format!(".staging-{}", std::process::id()));
        fs::create_dir_all(&staging)?;
        let cleanup_staging = || {
            let _ = fs::remove_dir_all(&staging);
        };

        let status = std::process::Command::new("tar")
            .arg("-xf")
            .arg(package)
            .arg("-C")
            .arg(&staging)
            .status()
            .with_context(|| format!("invoke tar on {}", package.display()))?;
        if !status.success() {
            cleanup_staging();
            return Err(anyhow!("tar exited with {} extracting {}", status, package_path));
        }

        let manifest_path = staging.join("manifest.webapp");
        let raw = fs::read_to_string(&manifest_path).with_context(|| {
            cleanup_staging();
            format!("read {} after extraction", manifest_path.display())
        })?;
        let manifest: WebAppManifest =
            serde_json::from_str(&raw).context("parse manifest.webapp (pre-id-check)")?;

        if let Some(integrity) = &manifest.integrity {
            if let Some(expected) = &integrity.sha256 {
                let actual = sha256_hex(package)?;
                if &actual != expected {
                    cleanup_staging();
                    return Err(anyhow!(
                        "integrity check failed for {}: expected sha256 {}, got {}",
                        package_path,
                        expected,
                        actual
                    ));
                }
            } else {
                tracing::warn!(
                    "nova: installing '{}' with no integrity.sha256 declared (unverified package)",
                    manifest.id
                );
            }
        } else {
            tracing::warn!(
                "nova: installing '{}' with no integrity block at all (unverified package)",
                manifest.id
            );
        }

        // Re-validate with the real id check now that we trust the bytes.
        let manifest = WebAppManifest::parse(&raw, &manifest.id)?;
        if !staging.join("webroot").is_dir() {
            cleanup_staging();
            return Err(anyhow!("package for '{}' has no webroot/", manifest.id));
        }

        // A user-installed package may not claim an id a root-mediated tier
        // already owns. `reload()` enforces this too, but refusing here means
        // the caller gets a real error instead of an install that reports
        // success and then silently never appears.
        let incumbent = self.tier_of(&manifest.id);
        if !tier_may_claim_id(AppTier::User, incumbent) {
            cleanup_staging();
            return Err(anyhow!(
                "cannot install '{}': that id belongs to a {} app; \
                 update it through the channel that installed it",
                manifest.id,
                incumbent.map_or("?", AppTier::as_str)
            ));
        }

        let final_dir = user_root.join(&manifest.id);
        if final_dir.exists() {
            fs::remove_dir_all(&final_dir)
                .with_context(|| format!("remove previous install at {}", final_dir.display()))?;
        }
        fs::rename(&staging, &final_dir).with_context(|| {
            format!(
                "atomic move {} -> {}",
                staging.display(),
                final_dir.display()
            )
        })?;

        let count = self.reload()?;
        self.emit(
            "AppInstalled",
            json!({ "appId": manifest.id, "version": manifest.version }),
        );
        tracing::info!("nova: installed '{}' ({} apps registered)", manifest.id, count);
        Ok(json!({ "appId": manifest.id, "version": manifest.version }))
    }

    /// Uninstall: stop any live session, remove the app dir, optionally
    /// purge its profile (storage/cookies/service-worker cache) — never
    /// silently, only when `purge_data` is explicitly true.
    pub fn uninstall(&self, app_id: &str, purge_data: bool) -> Result<Value> {
        let (dir, tier) = {
            let reg = self
                .registry
                .read()
                .map_err(|_| anyhow!("registry lock poisoned"))?;
            reg.get(app_id)
                .map(|e| (e.app_dir.clone(), e.tier))
                .ok_or_else(|| anyhow!("app '{}' is not installed", app_id))?
        };
        // Nova removes only what Nova installed. opkg owns the files (and its
        // status database) for ipk apps — deleting them behind its back would
        // leave the package "installed" with no payload; the path for those is
        // `opkg remove <id>`, whose postrm rescans the registry. System apps
        // are part of the image: `/usr` is writable at runtime, so without
        // this check an `Uninstall` call would really delete a shipped app's
        // files and no rescan could bring them back.
        if !tier.removable_by_nova() {
            return match tier {
                AppTier::Opkg => Err(anyhow!(
                    "app '{}' was installed via opkg; use `opkg remove {}` instead",
                    app_id,
                    app_id
                )),
                _ => Err(anyhow!(
                    "app '{}' ships with the system image and cannot be uninstalled",
                    app_id
                )),
            };
        }
        if let Some(session) = AppHost::global().find_session_by_app(app_id) {
            let _ = AppHost::global().close(&session.session_id);
        }
        fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
        if purge_data {
            let profile = Path::new(PROFILES_ROOT).join(app_id);
            if profile.exists() {
                fs::remove_dir_all(&profile)
                    .with_context(|| format!("purge {}", profile.display()))?;
            }
        }
        let count = self.reload()?;
        self.emit("AppUninstalled", json!({ "appId": app_id }));
        tracing::info!("nova: uninstalled '{}' ({} apps remain)", app_id, count);
        Ok(json!({ "appId": app_id }))
    }
}

/// `sha256sum <path>` — shells out rather than adding a new crate
/// dependency (`sha2`) purely for this one call site; acceptable since
/// install is not a hot path.
fn sha256_hex(path: &Path) -> Result<String> {
    let output = std::process::Command::new("sha256sum")
        .arg(path)
        .output()
        .context("invoke sha256sum")?;
    if !output.status.success() {
        return Err(anyhow!("sha256sum failed on {}", path.display()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let hex = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("unexpected sha256sum output: {}", stdout))?;
    Ok(hex.to_string())
}

/// Small helper trait so `Option<&RegistryEntry>` can be cloned out of the
/// read-lock guard without an extra `match`/`if let` at each call site.
trait CloneEntry {
    fn cloned_entry(self) -> Option<RegistryEntry>;
}
impl CloneEntry for Option<&RegistryEntry> {
    fn cloned_entry(self) -> Option<RegistryEntry> {
        self.cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_permission() {
        let raw = r#"{"id":"com.test.app","name":"Test","permissions":["not-a-real-perm"]}"#;
        let err = WebAppManifest::parse(raw, "com.test.app").unwrap_err();
        assert!(err.to_string().contains("unknown permission"));
    }

    #[test]
    fn rejects_id_mismatch() {
        let raw = r#"{"id":"com.test.app","name":"Test"}"#;
        let err = WebAppManifest::parse(raw, "com.other.app").unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }

    #[test]
    fn rejects_entry_traversal() {
        let raw = r#"{"id":"com.test.app","name":"Test","entry":"../../etc/passwd"}"#;
        let err = WebAppManifest::parse(raw, "com.test.app").unwrap_err();
        assert!(err.to_string().contains("safe relative path"));
    }

    #[test]
    fn accepts_valid_manifest_with_defaults() {
        let raw = r#"{"id":"com.test.app","name":"Test"}"#;
        let m = WebAppManifest::parse(raw, "com.test.app").unwrap();
        assert_eq!(m.entry, "index.html");
        assert_eq!(m.r#type, "web");
        assert!(m.single_instance);
        assert_eq!(m.display, "windowed");
    }

    // ---------- appinfo.json (ipk apps) ----------

    fn appinfo_raw(app_type: &str, main: &str) -> String {
        format!(
            r#"{{"id":"com.test.app","version":"1.0.0","vendor":"T","title":"Test",
                "type":"{app_type}","main":"{main}","icon":"icon.png"}}"#
        )
    }

    #[test]
    fn appinfo_accepts_valid_web() {
        let m = AppInfoManifest::parse(&appinfo_raw("web", "index.html"), "com.test.app").unwrap();
        assert_eq!(m.app_type, "web");
        assert_eq!(m.main, "index.html");
    }

    #[test]
    fn appinfo_rejects_absolute_web_main() {
        let err =
            AppInfoManifest::parse(&appinfo_raw("web", "/etc/passwd"), "com.test.app").unwrap_err();
        assert!(err.to_string().contains("safe relative path"));
    }

    #[test]
    fn appinfo_rejects_relative_native_main() {
        let err =
            AppInfoManifest::parse(&appinfo_raw("native", "bin/app"), "com.test.app").unwrap_err();
        assert!(err.to_string().contains("absolute path"));
    }

    #[test]
    fn appinfo_rejects_id_mismatch() {
        let err =
            AppInfoManifest::parse(&appinfo_raw("web", "index.html"), "com.other.app").unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }

    #[test]
    fn appinfo_rejects_unknown_permission() {
        let raw = r#"{"id":"com.test.app","version":"1.0.0","vendor":"T","title":"Test",
            "type":"web","main":"index.html","icon":"icon.png",
            "requiredPermissions":["root-access"]}"#;
        let err = AppInfoManifest::parse(raw, "com.test.app").unwrap_err();
        assert!(err.to_string().contains("unknown permission"));
    }

    /// The exact `appinfo.json` Vroom emits for the first-party PIM apps:
    /// a single-segment id (`notes` — the `pyxis://` origin authority, not a
    /// packaging choice) and spec §19 dot-vocabulary permissions. Pins that
    /// the toolchain's real output parses and that the dot→colon capability
    /// translation lands on the bridge vocabulary.
    #[test]
    fn appinfo_accepts_first_party_pim_app() {
        let raw = r#"{
            "id": "notes",
            "version": "0.1.0",
            "vendor": "Pyxis Project",
            "title": "Notes",
            "type": "web",
            "main": "index.html",
            "icon": "icon.png",
            "uiRevision": 1,
            "requiredPermissions": ["filesystem.read", "filesystem.write"]
        }"#;
        let info = AppInfoManifest::parse(raw, "notes").unwrap();
        assert_eq!(info.id, "notes");
        let caps: Vec<&str> = info
            .required_permissions
            .iter()
            .filter_map(|p| appinfo_permission_to_capability(p))
            .collect();
        assert_eq!(caps, vec!["filesystem:read", "filesystem:write"]);
    }

    #[test]
    fn appinfo_permission_mapping_covers_bridge_vocabulary() {
        // Every mapped capability must exist in KNOWN_CAPABILITIES —
        // otherwise declared_permissions would emit strings the bridge's
        // _capabilityFor table can never match.
        for perm in APPINFO_PERMISSIONS {
            if let Some(cap) = appinfo_permission_to_capability(perm) {
                assert!(
                    KNOWN_CAPABILITIES.contains(&cap),
                    "mapped capability '{cap}' missing from KNOWN_CAPABILITIES"
                );
            }
        }
    }

    #[test]
    fn every_bridge_capability_is_reachable_from_appinfo() {
        // The other direction: an ipk app must be able to request every
        // bridge capability, or the dot vocabulary has a gap and ipk-packaged
        // apps are second-class next to in-image manifest.webapp ones. This
        // is the regression that motivated adding appmanager.*/power.
        for cap in KNOWN_CAPABILITIES {
            assert!(
                APPINFO_PERMISSIONS
                    .iter()
                    .any(|p| appinfo_permission_to_capability(p) == Some(cap)),
                "bridge capability '{cap}' has no appinfo.json permission that maps to it"
            );
        }
    }

    #[test]
    fn privileged_vocabularies_correspond() {
        // The dot and colon privileged sets must denote the same rights,
        // otherwise the two gates (load_one_appinfo / load_one) disagree.
        let mapped: Vec<&str> = PRIVILEGED_APPINFO_PERMISSIONS
            .iter()
            .map(|p| appinfo_permission_to_capability(p).expect("privileged perm must map"))
            .collect();
        assert_eq!(mapped, PRIVILEGED_CAPABILITIES);
        for perm in PRIVILEGED_APPINFO_PERMISSIONS {
            assert!(
                APPINFO_PERMISSIONS.contains(perm),
                "privileged permission '{perm}' missing from APPINFO_PERMISSIONS"
            );
        }
        for cap in PRIVILEGED_CAPABILITIES {
            assert!(
                KNOWN_CAPABILITIES.contains(cap),
                "privileged capability '{cap}' missing from KNOWN_CAPABILITIES"
            );
        }
    }

    fn app_mgmt_declaration() -> Vec<String> {
        ["filesystem.read", "appmanager.install", "appmanager.manage"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn privileged_permissions_withheld_from_untrusted_id() {
        // A third-party ipk may *declare* app-management, but gets only the
        // unprivileged remainder even at a root-mediated tier.
        assert!(!app_id_is_privileged("com.example.evil"));
        assert_eq!(
            appinfo_capabilities_for("com.example.evil", AppTier::Opkg, &app_mgmt_declaration()),
            vec!["filesystem:read"]
        );
    }

    #[test]
    fn privileged_permissions_granted_to_trusted_id_at_root_tiers() {
        assert!(app_id_is_privileged("installer"));
        for tier in [AppTier::System, AppTier::Opkg] {
            assert_eq!(
                appinfo_capabilities_for("installer", tier, &app_mgmt_declaration()),
                vec![
                    "filesystem:read",
                    "appManager:install",
                    "appManager:manage"
                ],
                "trusted id should keep app-management at tier {}",
                tier.as_str()
            );
        }
    }

    #[test]
    fn in_image_apps_declaring_app_management_are_trusted() {
        // Regression guard for the trust list itself. `bloom` (the shell) and
        // `installer` are staged by scripts/create-rootfs.sh with
        // appManager:* in their manifest.webapp permissions[]. An id missing
        // from PRIVILEGED_APP_IDS would have its capabilities silently
        // stripped at load and the app would break on device, which is a
        // failure mode no other test would catch.
        for id in ["bloom", "installer"] {
            assert!(
                app_id_is_privileged(id),
                "'{id}' declares appManager:* in its staged manifest but is not on \
                 the trust list; it would lose the capability at load"
            );
        }
    }

    #[test]
    fn privileged_permissions_withheld_from_user_tier_even_for_trusted_id() {
        // The shadowing defence: a tarball claiming the installer's id must
        // not inherit the installer's authority. Belt-and-braces with the
        // reload()/install() id-reservation rule — if either check regresses,
        // the other still denies the capability.
        assert!(app_id_is_privileged("installer"));
        assert_eq!(
            appinfo_capabilities_for("installer", AppTier::User, &app_mgmt_declaration()),
            vec!["filesystem:read"]
        );
    }

    #[test]
    fn unprivileged_permissions_are_not_gated_by_tier() {
        // power is in the dot vocabulary but deliberately NOT privileged, so
        // even the least-trusted tier keeps it.
        let declared = vec!["power".to_string(), "notifications".to_string()];
        assert_eq!(
            appinfo_capabilities_for("com.example.anything", AppTier::User, &declared),
            vec!["power", "notifications"]
        );
    }

    #[test]
    fn tier_policy_matrix() {
        // System and opkg are root-mediated: they own their ids and may hold
        // privilege, but Nova never deletes their files. Only the user tier
        // is Nova's to remove — and it may hold no privilege at all.
        for tier in [AppTier::System, AppTier::Opkg] {
            assert!(tier.may_hold_privileged(), "{}", tier.as_str());
            assert!(tier.owns_its_ids(), "{}", tier.as_str());
            assert!(!tier.removable_by_nova(), "{}", tier.as_str());
        }
        assert!(!AppTier::User.may_hold_privileged());
        assert!(!AppTier::User.owns_its_ids());
        assert!(AppTier::User.removable_by_nova());
    }

    #[test]
    fn user_tier_cannot_claim_a_root_mediated_id() {
        // The shadowing fix, at the rule both reload() and install() consult.
        for held in [AppTier::System, AppTier::Opkg] {
            assert!(
                !tier_may_claim_id(AppTier::User, Some(held)),
                "user app must not be able to take a {} app's id",
                held.as_str()
            );
        }
        // Free ids and plain user-over-user replacement stay allowed —
        // reinstalling/upgrading a user app must keep working.
        assert!(tier_may_claim_id(AppTier::User, None));
        assert!(tier_may_claim_id(AppTier::User, Some(AppTier::User)));
    }

    #[test]
    fn opkg_may_still_shadow_a_system_app() {
        // The documented update path: `opkg install notes_0.2.0_all.ipk`
        // deliberately shadows the ROM copy. Locking down the user tier must
        // not have broken this.
        assert!(tier_may_claim_id(AppTier::Opkg, Some(AppTier::System)));
        assert!(tier_may_claim_id(AppTier::System, Some(AppTier::Opkg)));
    }

    #[test]
    fn tier_manifest_formats() {
        assert_eq!(AppTier::System.manifest_source(), ManifestSource::WebApp);
        assert_eq!(AppTier::User.manifest_source(), ManifestSource::WebApp);
        assert_eq!(AppTier::Opkg.manifest_source(), ManifestSource::AppInfo);
    }

    #[test]
    fn appinfo_accepts_privileged_permission_declaration() {
        // Declaration must validate for everyone — the gate is at grant
        // time, not parse time (spec §19.2). A hard parse error here would
        // make an installer .ipk unloadable rather than merely unprivileged.
        let raw = r#"{
            "id": "installer",
            "version": "0.1.0",
            "vendor": "Pyxis Project",
            "title": "App Installer",
            "type": "web",
            "main": "index.html",
            "icon": "icon.png",
            "requiredPermissions": ["appmanager.install", "appmanager.manage"]
        }"#;
        let info = AppInfoManifest::parse(raw, "installer").unwrap();
        assert_eq!(info.required_permissions.len(), 2);
    }
}
