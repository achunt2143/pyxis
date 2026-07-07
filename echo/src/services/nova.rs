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

/// One registry entry: manifest + where its webroot actually lives.
#[derive(Debug, Clone)]
struct RegistryEntry {
    manifest: WebAppManifest,
    webroot: PathBuf,
    app_dir: PathBuf,
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

    /// Scan both app roots (system first, then user — user entries win on
    /// id collision, matching "system-first-then-user" resolution from the
    /// plan) and rebuild the in-memory registry. Bad manifests are skipped
    /// with a warning, not a hard failure — one broken app can't wedge
    /// discovery for the rest.
    pub fn reload(&self) -> Result<usize> {
        let mut found: HashMap<String, RegistryEntry> = HashMap::new();
        for root in [Path::new(SYSTEM_APPS_ROOT), Path::new(USER_APPS_ROOT)] {
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
                match Self::load_one(&app_dir, &id) {
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
        self.persist_registry();
        self.emit("RegistryChanged", json!({ "count": count }));
        Ok(count)
    }

    fn load_one(app_dir: &Path, id: &str) -> Result<RegistryEntry> {
        let manifest_path = app_dir.join("manifest.webapp");
        let raw = fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let manifest = WebAppManifest::parse(&raw, id)?;
        let webroot = app_dir.join("webroot");
        if !webroot.is_dir() {
            return Err(anyhow!("no webroot/ directory under {}", app_dir.display()));
        }
        Ok(RegistryEntry {
            manifest,
            webroot,
            app_dir: app_dir.to_path_buf(),
        })
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
        let projected: Vec<Value> = reg
            .values()
            .map(|e| e.manifest.to_info_json(true))
            .collect();
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
        let mut apps: Vec<Value> = reg.values().map(|e| e.manifest.to_info_json(true)).collect();
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
        let mut info = entry.manifest.to_info_json(true);
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

        self.ensure_profile_dir(app_id)?;
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
        let profile_dir = Path::new(PROFILES_ROOT).join(&entry.manifest.id);
        let mut env = HashMap::new();
        env.insert("MOZ_ENABLE_WAYLAND".to_string(), "1".to_string());
        env.insert("PYXIS_APP_ID".to_string(), entry.manifest.id.clone());
        env.insert(
            "PYXIS_PROFILE_DIR".to_string(),
            profile_dir.to_string_lossy().to_string(),
        );
        env.insert(
            "PYXIS_DISPLAY_MODE".to_string(),
            entry.manifest.display.clone(),
        );
        if entry.manifest.background {
            env.insert("PYXIS_BACKGROUND".to_string(), "1".to_string());
        }

        let native = json!({
            "id": entry.manifest.id,
            "name": entry.manifest.name,
            "exec": VELA_BIN,
            "icon": entry.manifest.icons.get("64").cloned().unwrap_or_default(),
            "version": entry.manifest.version,
            "category": entry.manifest.category,
            "args": ["--profile", profile_dir.to_string_lossy(), "--pyxis-app", entry.manifest.id],
            "env": env,
        });

        // Native manifest.json is written next to manifest.webapp — AppHost
        // reads /usr/share/geckoOS/apps/<id>/manifest.json today (see the
        // apps_root() dual-root change alongside this), so writing it into
        // whichever root this app actually lives in (system or user) keeps
        // AppHost's existing lookup unchanged.
        let out_path = entry.app_dir.join("manifest.json");
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
        if let Some(session) = AppHost::global().find_session_by_app(app_id) {
            let _ = AppHost::global().close(&session.session_id);
        }
        let dir = {
            let reg = self
                .registry
                .read()
                .map_err(|_| anyhow!("registry lock poisoned"))?;
            reg.get(app_id)
                .map(|e| e.app_dir.clone())
                .ok_or_else(|| anyhow!("app '{}' is not installed", app_id))?
        };
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
}
