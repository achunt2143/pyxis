//! Privileged access grants and audit logging (`org.geckoOS.privilege`).
//!
//! An app that holds `filesystem:system` may read the root filesystem, while
//! writes stay refused until the user explicitly unlocks privilege via the
//! shell's overlay prompt:
//!
//! 1. App calls `privilege.Request({ reason })` -> queued in `pending`,
//!    emitting `PrivilegeRequested` so Bloom's overlay prompts the user.
//! 2. Shell calls `privilege.Authenticate({ password, requestId (required), durationSecs? })` -> verifies the active session user's password via
//!    `session::verify_active_user_password` (never callable by apps).
//! 3. Echo stores an in-memory `Grant` (default 300s, never persisted to disk).
//!    While active, `filesystem::scope` upgrades `system` scope for that
//!    `appId` to `Scope::SystemElevated` and logs every write to memory (capped
//!    at 100 entries) and best-effort to `/var/log/pyxis-privileged.log`.
//! 4. Grants end when expired, revoked via `Revoke`, when the app closes or is
//!    reaped, or when the session locks (`session::lock`).

use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use tokio::sync::oneshot;

use crate::actions::{self, ActionRequest, Policy, FILESYSTEM_SYSTEM_WRITE};
use crate::services::session::PasswordCheckFailure;
use crate::IPCBus;

pub const PRIVILEGE_SERVICE: &str = "org.geckoOS.privilege";
pub const PRIVILEGE_IFACE: &str = "org.geckoOS.privilege";
pub const DEFAULT_GRANT_SECS: u64 = 300;
pub const REQUEST_TTL_MS: u64 = 120_000;
/// How long a held call waits for the user before failing `timeout`.
pub const HELD_CALL_TIMEOUT: Duration = Duration::from_secs(120);
pub const PRIVILEGED_LOG_PATH: &str = "/var/log/pyxis-privileged.log";

pub const SIGNAL_PRIVILEGE_REQUESTED: &str = "PrivilegeRequested";
pub const SIGNAL_PRIVILEGE_CHANGED: &str = "PrivilegeChanged";

const AUDIT_LOG_CAP: usize = 100;
const REASON_MAX_CHARS: usize = 200;
const MAX_GRANT_SECS: u64 = 86_400;
const SWEEP_INTERVAL_SECS: u64 = 15;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Grant {
    #[serde(rename = "appId")]
    pub app_id: String,
    #[serde(rename = "appName")]
    pub app_name: String,
    pub reason: String,
    #[serde(rename = "grantedBy")]
    pub granted_by: String,
    #[serde(rename = "grantedAtMs")]
    pub granted_at_ms: u64,
    #[serde(rename = "expiresAtMs")]
    pub expires_at_ms: u64,
    #[serde(default = "fs_action")]
    pub action: String,
}

fn fs_action() -> String {
    FILESYSTEM_SYSTEM_WRITE.to_string()
}

fn empty_details() -> Value {
    json!({})
}

impl Grant {
    pub fn to_json_with_remaining(&self, now: u64) -> Value {
        let remaining_secs = self.expires_at_ms.saturating_sub(now).div_ceil(1000);
        json!({
            "appId": self.app_id,
            "appName": self.app_name,
            "action": self.action,
            "reason": self.reason,
            "grantedBy": self.granted_by,
            "grantedAt": self.granted_at_ms,
            "grantedAtMs": self.granted_at_ms,
            "expiresAt": self.expires_at_ms,
            "expiresAtMs": self.expires_at_ms,
            "remainingSecs": remaining_secs,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrivilegeRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "appId")]
    pub app_id: String,
    #[serde(rename = "appName")]
    pub app_name: String,
    pub reason: String,
    #[serde(rename = "requestedAtMs")]
    pub requested_at_ms: u64,
    #[serde(default = "fs_action")]
    pub action: String,
    #[serde(default = "empty_details")]
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEntry {
    #[serde(rename = "timestampMs")]
    pub timestamp_ms: u64,
    pub timestamp: String,
    #[serde(rename = "appId")]
    pub app_id: String,
    pub method: String,
    pub path: String,
}

/// Why a held call did not get its approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionDenied {
    Denied,
    Timeout,
    NoPassword,
}

impl ActionDenied {
    pub fn reason(&self) -> &'static str {
        match self {
            ActionDenied::Denied => "denied",
            ActionDenied::Timeout => "timeout",
            ActionDenied::NoPassword => "no-password",
        }
    }

    /// `{"reason": ..., "error": ...}`, the error text a held call fails with.
    pub fn to_error_json(&self) -> String {
        let error = match self {
            ActionDenied::Denied => "The request was cancelled.",
            ActionDenied::Timeout => "No answer in time.",
            ActionDenied::NoPassword => "Set a password first (Settings, Users & Lock).",
        };
        json!({ "reason": self.reason(), "error": error }).to_string()
    }
}

type Waiter = oneshot::Sender<Result<(), ActionDenied>>;

#[derive(Default)]
struct PrivilegeState {
    grants: HashMap<String, Grant>,
    /// Approvals of general actions, keyed by (app, action). Never holds a
    /// one-shot approval: those resolve the waiting call and are gone.
    action_grants: HashMap<(String, String), Grant>,
    /// Calls held in `authorize`, by request id.
    waiters: HashMap<String, Waiter>,
    pending: Vec<PrivilegeRequest>,
    audit_log: VecDeque<AuditEntry>,
    next_seq: u64,
}

static STATE: OnceLock<Mutex<PrivilegeState>> = OnceLock::new();

type SignalSender = Arc<dyn Fn(&str, Value) + Send + Sync>;
static SIGNAL_SENDER: OnceLock<RwLock<Option<SignalSender>>> = OnceLock::new();

fn state_lock() -> &'static Mutex<PrivilegeState> {
    STATE.get_or_init(|| Mutex::new(PrivilegeState::default()))
}

fn signal_slot() -> &'static RwLock<Option<SignalSender>> {
    SIGNAL_SENDER.get_or_init(|| RwLock::new(None))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn format_timestamp(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let nanos = ((ms % 1000) * 1_000_000) as u32;
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nanos)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| ms.to_string())
}

fn emit_signal_global(signal_name: &str, payload: Value) {
    if let Some(sender) = signal_slot().read().clone() {
        sender(signal_name, payload);
    }
}

fn install_bus_signal_sender(bus: Arc<IPCBus>) {
    let sender: SignalSender = Arc::new(move |signal_name: &str, payload: Value| {
        let bus = bus.clone();
        let name = signal_name.to_string();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(e) = bus.emit_signal(PRIVILEGE_SERVICE, &name, payload).await {
                    tracing::warn!("privilege signal '{}' failed: {}", name, e);
                }
            });
        }
    });
    *signal_slot().write() = Some(sender);
}

impl PrivilegeState {
    fn prune_expired(&mut self, now: u64) -> (Vec<Grant>, Vec<PrivilegeRequest>) {
        let expired_ids: Vec<String> = self
            .grants
            .iter()
            .filter(|(_, g)| g.expires_at_ms <= now)
            .map(|(id, _)| id.clone())
            .collect();
        let mut removed_grants = Vec::with_capacity(expired_ids.len());
        for id in expired_ids {
            if let Some(g) = self.grants.remove(&id) {
                removed_grants.push(g);
            }
        }

        let expired_keys: Vec<(String, String)> = self
            .action_grants
            .iter()
            .filter(|(_, g)| g.expires_at_ms <= now)
            .map(|(k, _)| k.clone())
            .collect();
        for k in expired_keys {
            if let Some(g) = self.action_grants.remove(&k) {
                removed_grants.push(g);
            }
        }

        let mut removed_pending = Vec::new();
        let mut kept_pending = Vec::with_capacity(self.pending.len());
        for req in self.pending.drain(..) {
            if req.requested_at_ms.saturating_add(REQUEST_TTL_MS) <= now {
                if let Some(w) = self.waiters.remove(&req.request_id) {
                    let _ = w.send(Err(ActionDenied::Timeout));
                }
                removed_pending.push(req);
            } else {
                kept_pending.push(req);
            }
        }
        self.pending = kept_pending;
        (removed_grants, removed_pending)
    }

    fn resolve_waiter(&mut self, request_id: &str, outcome: Result<(), ActionDenied>) {
        if let Some(w) = self.waiters.remove(request_id) {
            let _ = w.send(outcome);
        }
    }

    fn grants_list(&self) -> Vec<Grant> {
        let mut list: Vec<Grant> = self.grants.values().cloned().collect();
        list.sort_by(|a, b| a.app_id.cmp(&b.app_id));
        list
    }

    fn request_with_pruned_at(
        &mut self,
        app_id: &str,
        reason: Option<&str>,
        now: u64,
    ) -> Result<(PrivilegeRequest, Option<Grant>, Vec<Grant>, Vec<PrivilegeRequest>)> {
        let app_id = app_id.trim();
        if app_id.is_empty() {
            return Err(anyhow!("privilege.Request requires a non-empty appId"));
        }
        let (expired_grants, expired_pending) = self.prune_expired(now);
        if let Some(existing) = self.grants.get(app_id).cloned() {
            let req = PrivilegeRequest {
                request_id: String::new(),
                app_id: existing.app_id.clone(),
                app_name: existing.app_name.clone(),
                reason: reason.map(sanitize_reason).unwrap_or_else(|| existing.reason.clone()),
                requested_at_ms: now,
                action: fs_action(),
                details: empty_details(),
            };
            return Ok((req, Some(existing), expired_grants, expired_pending));
        }

        // The prompt's name is Nova's manifest name for the app id, never the
        // caller's own text (an app could otherwise call itself "System Update").
        let resolved_name = resolve_app_name(app_id);
        let reason_str = reason.map(sanitize_reason).unwrap_or_default();

        // Replace any stale pending request from the same app so the prompt
        // reflects the latest reason without accumulating duplicates.
        self.pending.retain(|r| !(r.app_id == app_id && r.action == FILESYSTEM_SYSTEM_WRITE));
        self.next_seq += 1;
        let req = PrivilegeRequest {
            request_id: format!("priv-{}-{}", now, self.next_seq),
            app_id: app_id.to_string(),
            app_name: resolved_name,
            reason: reason_str,
            requested_at_ms: now,
            action: fs_action(),
            details: empty_details(),
        };
        self.pending.push(req.clone());
        Ok((req, None, expired_grants, expired_pending))
    }

    #[cfg(test)]
    fn request_at(
        &mut self,
        app_id: &str,
        reason: Option<&str>,
        now: u64,
    ) -> Result<(PrivilegeRequest, Option<Grant>)> {
        let (req, existing, _, _) = self.request_with_pruned_at(app_id, reason, now)?;
        Ok((req, existing))
    }

    fn grant_at<F>(
        &mut self,
        password: &str,
        request_id: Option<&str>,
        app_id: Option<&str>,
        duration_secs: Option<u64>,
        now: u64,
        verify: F,
    ) -> Result<Grant>
    where
        F: FnOnce(&str) -> std::result::Result<String, String>,
    {
        self.prune_expired(now);

        // Locate matching pending request or explicit appId before verifying so
        // bogus requests fail early without burning rate-limit attempts.
        let req_id = request_id.map(str::trim).filter(|s| !s.is_empty());
        let target_app = app_id.map(str::trim).filter(|s| !s.is_empty());
        let matched_idx = if let Some(rid) = req_id {
            self.pending.iter().position(|r| r.request_id == rid)
        } else if let Some(aid) = target_app {
            self.pending.iter().position(|r| r.app_id == aid)
        } else {
            // No request named: nothing is guessed (no "first pending").
            None
        };

        if matched_idx.is_none() && target_app.is_none() {
            return Err(anyhow!(
                "No pending privilege request found and no appId specified"
            ));
        }
        if req_id.is_some() && matched_idx.is_none() && target_app.is_none() {
            return Err(anyhow!("Privilege request '{}' not found", req_id.unwrap()));
        }

        let granted_by = verify(password).map_err(|e| anyhow!("{e}"))?;

        if let Some(idx) = matched_idx {
            if self.pending[idx].action != FILESYSTEM_SYSTEM_WRITE {
                let req = self.pending.remove(idx);
                return Ok(self.approve_action(req, granted_by, now));
            }
        }

        let (resolved_id, resolved_name, resolved_reason) = if let Some(idx) = matched_idx {
            let req = self.pending.remove(idx);
            (req.app_id, req.app_name, req.reason)
        } else {
            let aid = target_app.unwrap().to_string();
            self.pending.retain(|r| r.app_id != aid);
            let name = resolve_app_name(&aid);
            (aid, name, String::new())
        };

        let secs = duration_secs
            .filter(|&d| d > 0)
            .unwrap_or(DEFAULT_GRANT_SECS)
            .min(MAX_GRANT_SECS);
        let expires_at_ms = now.saturating_add(secs.saturating_mul(1000));
        let grant = Grant {
            app_id: resolved_id.clone(),
            app_name: resolved_name,
            reason: resolved_reason,
            granted_by,
            granted_at_ms: now,
            expires_at_ms,
            action: fs_action(),
        };
        self.grants.insert(resolved_id, grant.clone());
        Ok(grant)
    }

    /// The user approved a general-action request: wake the held call, and
    /// keep a grant only when the action's policy caches one.
    fn approve_action(&mut self, req: PrivilegeRequest, granted_by: String, now: u64) -> Grant {
        let (expires_at_ms, cache) = match actions::policy(&req.action) {
            Policy::AskThenCache(d) => (now.saturating_add(d.as_millis() as u64), true),
            Policy::AskEveryTime => (now, false),
        };
        let grant = Grant {
            app_id: req.app_id.clone(),
            app_name: req.app_name.clone(),
            reason: req.reason.clone(),
            granted_by,
            granted_at_ms: now,
            expires_at_ms,
            action: req.action.clone(),
        };
        if cache {
            self.action_grants
                .insert((req.app_id.clone(), req.action.clone()), grant.clone());
        }
        self.resolve_waiter(&req.request_id, Ok(()));
        grant
    }

    fn dismiss_by(&mut self, request_id: Option<&str>, app_id: Option<&str>) -> Vec<PrivilegeRequest> {
        let req_id = request_id.map(str::trim).filter(|s| !s.is_empty());
        let target_app = app_id.map(str::trim).filter(|s| !s.is_empty());
        let mut removed = Vec::new();
        let mut kept = Vec::with_capacity(self.pending.len());
        for req in self.pending.drain(..) {
            let matches = match (req_id, target_app) {
                (Some(rid), _) => req.request_id == rid,
                (None, Some(aid)) => req.app_id == aid,
                (None, None) => true,
            };
            if matches {
                if let Some(w) = self.waiters.remove(&req.request_id) {
                    let _ = w.send(Err(ActionDenied::Denied));
                }
                removed.push(req);
            } else {
                kept.push(req);
            }
        }
        self.pending = kept;
        removed
    }

    #[cfg(test)]
    fn has_grant_at(&mut self, app_id: &str, now: u64) -> (bool, Vec<Grant>) {
        let (expired, _) = self.prune_expired(now);
        let active = self.grants.contains_key(app_id);
        (active, expired)
    }

    fn revoke_app_at(&mut self, app_id: &str, now: u64) -> (bool, Vec<Grant>, Vec<PrivilegeRequest>) {
        let (expired_grants, expired_pending) = self.prune_expired(now);
        let mut removed_grant = self.grants.remove(app_id).is_some();
        let before = self.action_grants.len();
        self.action_grants.retain(|(a, _), _| a != app_id);
        removed_grant |= self.action_grants.len() != before;
        let had_pending = self.pending.iter().any(|r| r.app_id == app_id);
        let ids: Vec<String> = self
            .pending
            .iter()
            .filter(|r| r.app_id == app_id)
            .map(|r| r.request_id.clone())
            .collect();
        for id in ids {
            self.resolve_waiter(&id, Err(ActionDenied::Denied));
        }
        self.pending.retain(|r| r.app_id != app_id);
        (removed_grant || had_pending, expired_grants, expired_pending)
    }

    fn revoke_all_at(&mut self, now: u64) -> (Vec<String>, Vec<Grant>, Vec<PrivilegeRequest>) {
        let (expired_grants, expired_pending) = self.prune_expired(now);
        let mut apps: Vec<String> = self.grants.keys().cloned().collect();
        self.grants.clear();
        for (app, _) in self.action_grants.keys() {
            if !apps.contains(app) {
                apps.push(app.clone());
            }
        }
        self.action_grants.clear();
        for req in self.pending.drain(..) {
            if let Some(w) = self.waiters.remove(&req.request_id) {
                let _ = w.send(Err(ActionDenied::Denied));
            }
            if !apps.contains(&req.app_id) {
                apps.push(req.app_id);
            }
        }
        apps.sort();
        (apps, expired_grants, expired_pending)
    }

    fn record_audit(&mut self, app_id: &str, method: &str, path: &str, now: u64, log_path: Option<&Path>) {
        let timestamp = format_timestamp(now);
        // Paths and ids are caller text: escape line breaks and other control
        // characters so one call can't write a forged second log line.
        let (app_id, method, path) = (escape_control(app_id), escape_control(method), escape_control(path));
        let entry = AuditEntry {
            timestamp_ms: now,
            timestamp: timestamp.clone(),
            app_id: app_id.clone(),
            method: method.clone(),
            path: path.clone(),
        };
        while self.audit_log.len() >= AUDIT_LOG_CAP {
            self.audit_log.pop_front();
        }
        self.audit_log.push_back(entry);

        if let Some(p) = log_path {
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).mode(0o600).open(p) {
                let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
                let _ = writeln!(
                    file,
                    "{} app={} method={} path={}",
                    timestamp, app_id, method, path
                );
            }
        }
    }

    fn snapshot_value(&self, app_id: Option<&str>, now: u64) -> Value {
        let grants_list = self.grants_list();
        let grants_json: Vec<Value> = grants_list
            .iter()
            .map(|g| g.to_json_with_remaining(now))
            .collect();
        let grant = match app_id {
            Some(id) => self.grants.get(id).cloned(),
            None => grants_list.first().cloned(),
        };
        let granted = grant.is_some();
        let active = !grants_list.is_empty();
        let audit_log: Vec<AuditEntry> = self.audit_log.iter().cloned().collect();
        json!({
            "active": active,
            "granted": granted,
            "grant": grant.map(|g| g.to_json_with_remaining(now)),
            "grants": grants_json,
            "actionGrants": self
                .action_grants
                .values()
                .map(|g| g.to_json_with_remaining(now))
                .collect::<Vec<_>>(),
            "pending": self.pending,
            "auditLog": audit_log,
            "defaultDurationSecs": DEFAULT_GRANT_SECS,
            "logPath": PRIVILEGED_LOG_PATH,
        })
    }
}

fn merge_signal_fields(mut base: Value, extra: Value) -> Value {
    if let (Some(base_obj), Some(extra_obj)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            base_obj.insert(k.clone(), v.clone());
        }
    }
    base
}

/// A reason as shown in the prompt: control characters removed, trimmed, at
/// most 200 characters.
fn sanitize_reason(reason: &str) -> String {
    let cleaned: String = reason.chars().filter(|c| !c.is_control()).collect();
    cleaned.trim().chars().take(REASON_MAX_CHARS).collect()
}

/// `s` with backslashes and control characters (line breaks included) written
/// as escapes, so it stays on one log line.
fn escape_control(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:04x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn resolve_app_name(app_id: &str) -> String {
    crate::services::nova::Nova::global()
        .get_info(app_id)
        .ok()
        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| app_id.to_string())
}

fn emit_expired_items(
    expired_grants: Vec<Grant>,
    expired_pending: Vec<PrivilegeRequest>,
    snapshot: &Value,
) {
    for g in expired_grants {
        let payload = merge_signal_fields(
            snapshot.clone(),
            json!({
                "appId": g.app_id,
                "granted": false,
                "reason": "expired",
                "grant": Value::Null,
            }),
        );
        emit_signal_global(SIGNAL_PRIVILEGE_CHANGED, payload);
    }
    for req in expired_pending {
        let payload = merge_signal_fields(
            snapshot.clone(),
            json!({
                "appId": req.app_id,
                "requestId": req.request_id,
                "granted": false,
                "reason": "expired",
                "grant": Value::Null,
            }),
        );
        emit_signal_global(SIGNAL_PRIVILEGE_CHANGED, payload);
    }
}

/// Prunes any expired active grants or abandoned pending privilege requests
/// (past `REQUEST_TTL_MS`), emitting `PrivilegeChanged` if anything expired.
pub fn sweep_expired() -> (usize, usize) {
    let now = now_ms();
    let (expired_grants, expired_pending, snapshot) = {
        let mut st = state_lock().lock();
        let (expired_grants, expired_pending) = st.prune_expired(now);
        let snapshot = if expired_grants.is_empty() && expired_pending.is_empty() {
            Value::Null
        } else {
            st.snapshot_value(None, now)
        };
        (expired_grants, expired_pending, snapshot)
    };
    let counts = (expired_grants.len(), expired_pending.len());
    if counts != (0, 0) {
        emit_expired_items(expired_grants, expired_pending, &snapshot);
    }
    counts
}

fn start_expiry_sweeper() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if STARTED.set(()).is_err() {
            return;
        }
        handle.spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(SWEEP_INTERVAL_SECS));
            interval.tick().await;
            loop {
                interval.tick().await;
                let (grants, pending) = sweep_expired();
                if grants > 0 || pending > 0 {
                    tracing::debug!(
                        "privilege sweeper pruned {} expired grant(s) and {} timed-out pending prompt(s)",
                        grants,
                        pending
                    );
                }
            }
        });
    }
}

/// Returns `true` if `app_id` currently holds a non-expired privilege grant.
pub fn has_grant(app_id: &str) -> bool {
    let now = now_ms();
    let (active, expired_grants, expired_pending, snapshot) = {
        let mut st = state_lock().lock();
        let (expired_grants, expired_pending) = st.prune_expired(now);
        let active = st.grants.contains_key(app_id);
        let snapshot = if expired_grants.is_empty() && expired_pending.is_empty() {
            Value::Null
        } else {
            st.snapshot_value(None, now)
        };
        (active, expired_grants, expired_pending, snapshot)
    };
    if !expired_grants.is_empty() || !expired_pending.is_empty() {
        emit_expired_items(expired_grants, expired_pending, &snapshot);
    }
    active
}

/// Revokes any active grant or pending request for `app_id`, emitting
/// `PrivilegeChanged` if anything changed. Returns `true` if a grant or
/// pending request existed.
pub fn revoke_for_app(app_id: &str, reason: &str) -> bool {
    let now = now_ms();
    let (revoked, expired_grants, expired_pending, snapshot) = {
        let mut st = state_lock().lock();
        let (revoked, expired_grants, expired_pending) = st.revoke_app_at(app_id, now);
        let snapshot = st.snapshot_value(None, now);
        (revoked, expired_grants, expired_pending, snapshot)
    };
    if !expired_grants.is_empty() || !expired_pending.is_empty() {
        emit_expired_items(expired_grants, expired_pending, &snapshot);
    }
    if revoked {
        let payload = merge_signal_fields(
            snapshot,
            json!({
                "appId": app_id,
                "granted": false,
                "reason": reason,
                "grant": Value::Null,
            }),
        );
        emit_signal_global(SIGNAL_PRIVILEGE_CHANGED, payload);
    }
    revoked
}

/// Revokes all active grants and clears pending requests (e.g. on session
/// lock), emitting `PrivilegeChanged` for each affected app.
pub fn revoke_all(reason: &str) -> Vec<String> {
    let now = now_ms();
    let (revoked_apps, expired_grants, expired_pending, snapshot) = {
        let mut st = state_lock().lock();
        let (apps, expired_grants, expired_pending) = st.revoke_all_at(now);
        let snapshot = st.snapshot_value(None, now);
        (apps, expired_grants, expired_pending, snapshot)
    };
    if !expired_grants.is_empty() || !expired_pending.is_empty() {
        emit_expired_items(expired_grants, expired_pending, &snapshot);
    }
    for app_id in &revoked_apps {
        let payload = merge_signal_fields(
            snapshot.clone(),
            json!({
                "appId": app_id,
                "granted": false,
                "reason": reason,
                "grant": Value::Null,
            }),
        );
        emit_signal_global(SIGNAL_PRIVILEGE_CHANGED, payload);
    }
    revoked_apps
}

/// Appends an elevated filesystem operation to the in-memory ring buffer (max
/// 100 entries) and best-effort appends to `/var/log/pyxis-privileged.log`.
pub fn log_elevated_op(app_id: &str, method: &str, path: &str) {
    let now = now_ms();
    let mut st = state_lock().lock();
    st.record_audit(app_id, method, path, now, Some(Path::new(PRIVILEGED_LOG_PATH)));
}

#[cfg(test)]
pub(crate) fn grant_for_test(app_id: &str) {
    let now = now_ms();
    let mut st = state_lock().lock();
    st.grant_at("pw", None, Some(app_id), Some(DEFAULT_GRANT_SECS), now, |_| {
        Ok("geckouser".to_string())
    })
    .expect("grant_for_test");
}

#[derive(Clone, Default)]
pub struct PrivilegeService {
    bus: Option<Arc<IPCBus>>,
}

impl PrivilegeService {
    pub fn new() -> Self {
        Self { bus: None }
    }

    pub fn with_bus(bus: Arc<IPCBus>) -> Self {
        install_bus_signal_sender(bus.clone());
        start_expiry_sweeper();
        Self { bus: Some(bus) }
    }

    fn emit(&self, signal_name: &str, payload: Value) {
        if let Some(bus) = &self.bus {
            let bus = bus.clone();
            let name = signal_name.to_string();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    if let Err(e) = bus.emit_signal(PRIVILEGE_SERVICE, &name, payload).await {
                        tracing::warn!("privilege signal '{}' failed: {}", name, e);
                    }
                });
                return;
            }
        }
        emit_signal_global(signal_name, payload);
    }

    /// Holds a root action until the user answers in the shell's dialog.
    ///
    /// A live grant for (app, action) returns at once. Otherwise a pending
    /// request goes out as `PrivilegeRequested` (with `action` and `details`)
    /// and this resolves on `Authenticate` (Ok), `Dismiss` (Denied), a missing
    /// password (NoPassword) or after 120 s (Timeout). No lock is held across
    /// the wait. Callers decide who is exempt (root); this always asks.
    pub async fn authorize(&self, app_id: &str, req: ActionRequest) -> Result<(), ActionDenied> {
        self.authorize_with_timeout(app_id, req, HELD_CALL_TIMEOUT).await
    }

    pub async fn authorize_with_timeout(
        &self,
        app_id: &str,
        req: ActionRequest,
        timeout: Duration,
    ) -> Result<(), ActionDenied> {
        let app_id = app_id.trim().to_string();
        let action = req.action;
        let details_text = req.details.to_string();
        let app_name = resolve_app_name(&app_id);
        let now = now_ms();

        let (request_id, rx, pending_req, pending, snapshot, expired_grants, expired_pending, expired_snapshot) = {
            let mut st = state_lock().lock();
            let (expired_grants, expired_pending) = st.prune_expired(now);
            let key = (app_id.clone(), action.to_string());
            if st.action_grants.contains_key(&key) {
                st.record_audit(
                    &app_id,
                    "Authorize",
                    &format!("{action} allowed (grant) {details_text}"),
                    now,
                    Some(Path::new(PRIVILEGED_LOG_PATH)),
                );
                drop(st);
                return Ok(());
            }
            st.next_seq += 1;
            let request_id = format!("priv-{}-{}", now, st.next_seq);
            let pending_req = PrivilegeRequest {
                request_id: request_id.clone(),
                app_id: app_id.clone(),
                app_name,
                reason: String::new(),
                requested_at_ms: now,
                action: action.to_string(),
                details: req.details.clone(),
            };
            let (tx, rx) = oneshot::channel();
            st.waiters.insert(request_id.clone(), tx);
            st.pending.push(pending_req.clone());
            let snapshot = st.snapshot_value(None, now);
            let expired_snapshot = snapshot.clone();
            (
                request_id,
                rx,
                pending_req,
                st.pending.clone(),
                snapshot,
                expired_grants,
                expired_pending,
                expired_snapshot,
            )
        };
        if !expired_grants.is_empty() || !expired_pending.is_empty() {
            emit_expired_items(expired_grants, expired_pending, &expired_snapshot);
        }

        let duration_secs = match actions::policy(action) {
            Policy::AskThenCache(d) => d.as_secs(),
            Policy::AskEveryTime => 0,
        };
        self.emit(
            SIGNAL_PRIVILEGE_REQUESTED,
            json!({
                "request": pending_req,
                "requestId": request_id,
                "appId": pending_req.app_id,
                "appName": pending_req.app_name,
                "reason": pending_req.reason,
                "durationSecs": duration_secs,
                "action": pending_req.action,
                "details": pending_req.details,
                "requestedAtMs": pending_req.requested_at_ms,
                "pending": pending,
                "state": snapshot,
            }),
        );
        self.emit(SIGNAL_PRIVILEGE_CHANGED, snapshot);

        // If the caller is dropped mid-wait (its connection closed), the
        // request must not linger in the dialog.
        struct Cleanup(Option<String>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(id) = self.0.take() {
                    let mut st = state_lock().lock();
                    st.waiters.remove(&id);
                    st.pending.retain(|r| r.request_id != id);
                }
            }
        }
        let mut cleanup = Cleanup(Some(request_id.clone()));

        let outcome = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ActionDenied::Denied),
            Err(_) => Err(ActionDenied::Timeout),
        };
        // Authenticate/Dismiss already removed the request; a timeout hasn't.
        let timed_out = outcome == Err(ActionDenied::Timeout);
        if timed_out {
            drop(cleanup);
            self.emit(
                SIGNAL_PRIVILEGE_CHANGED,
                merge_signal_fields(
                    state_lock().lock().snapshot_value(None, now_ms()),
                    json!({
                        "appId": app_id,
                        "requestId": request_id,
                        "granted": false,
                        "reason": "timeout",
                        "grant": Value::Null,
                    }),
                ),
            );
        } else {
            cleanup.0 = None;
        }
        let verdict = match outcome {
            Ok(()) => "allowed",
            Err(d) => d.reason(),
        };
        state_lock().lock().record_audit(
            &app_id,
            "Authorize",
            &format!("{action} {verdict} {details_text}"),
            now_ms(),
            Some(Path::new(PRIVILEGED_LOG_PATH)),
        );
        outcome
    }

    pub fn call(&self, method: &str, body: &Value) -> Result<Value> {
        match method {
            "Request" | "request" => self.request(body),
            "Authenticate" | "authenticate" => self.authenticate(body),
            "Dismiss" | "dismiss" => self.dismiss(body),
            "Revoke" | "revoke" => self.revoke(body),
            "GetState" | "getState" => self.get_state(body),
            _ => Err(anyhow!("Unknown method '{}' on {}", method, PRIVILEGE_SERVICE)),
        }
    }

    pub fn request(&self, body: &Value) -> Result<Value> {
        let app_id = body
            .get("appId")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let reason = body.get("reason").and_then(|v| v.as_str());
        let now = now_ms();

        let (req, existing, expired_grants, expired_pending, pending, app_snapshot, full_snapshot) = {
            let mut st = state_lock().lock();
            let (req, existing, expired_grants, expired_pending) =
                st.request_with_pruned_at(app_id, reason, now)?;
            let app_snapshot = st.snapshot_value(Some(&req.app_id), now);
            let full_snapshot = st.snapshot_value(None, now);
            (
                req,
                existing,
                expired_grants,
                expired_pending,
                st.pending.clone(),
                app_snapshot,
                full_snapshot,
            )
        };

        if !expired_grants.is_empty() || !expired_pending.is_empty() {
            emit_expired_items(expired_grants, expired_pending, &full_snapshot);
        }

        if let Some(grant) = existing {
            let grant_json = grant.to_json_with_remaining(now);
            return Ok(json!({
                "alreadyGranted": true,
                "granted": true,
                "pending": false,
                "requestId": Value::Null,
                "appId": grant.app_id,
                "appName": grant.app_name,
                "grant": grant_json,
                "state": app_snapshot,
            }));
        }

        self.emit(
            SIGNAL_PRIVILEGE_REQUESTED,
            json!({
                "request": req,
                "requestId": req.request_id,
                "appId": req.app_id,
                "appName": req.app_name,
                "reason": req.reason,
                "durationSecs": DEFAULT_GRANT_SECS,
                "action": req.action,
                "details": req.details,
                "requestedAtMs": req.requested_at_ms,
                "pending": pending,
                "state": full_snapshot,
            }),
        );
        self.emit(SIGNAL_PRIVILEGE_CHANGED, full_snapshot);

        Ok(json!({
            "alreadyGranted": false,
            "granted": false,
            "pending": true,
            "requestId": req.request_id,
            "appId": req.app_id,
            "appName": req.app_name,
            "reason": req.reason,
            "requestedAtMs": req.requested_at_ms,
            "request": req,
            "state": app_snapshot,
        }))
    }

    pub fn authenticate(&self, body: &Value) -> Result<Value> {
        self.authenticate_with_detailed(
            body,
            crate::services::session::verify_active_user_password_detailed,
        )
    }

    pub fn authenticate_with<F>(&self, body: &Value, verify: F) -> Result<Value>
    where
        F: FnOnce(&str) -> std::result::Result<String, String>,
    {
        self.authenticate_with_detailed(body, |pw| {
            verify(pw).map_err(|msg| parse_password_failure(&msg))
        })
    }

    pub fn authenticate_with_detailed<F>(&self, body: &Value, verify: F) -> Result<Value>
    where
        F: FnOnce(&str) -> std::result::Result<String, PasswordCheckFailure>,
    {
        let password = body
            .get("password")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("privilege.Authenticate requires string `password`"))?;
        let request_id = body
            .get("requestId")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("privilege.Authenticate requires a `requestId` naming a pending request"))?;
        let request_id = Some(request_id);
        // `appId` is ignored: the request names the app.
        let app_id: Option<&str> = None;
        let duration_secs = body.get("durationSecs").and_then(|v| v.as_u64());
        let now = now_ms();

        let (auth_outcome, expired_grants, expired_pending, full_snapshot) = {
            let mut st = state_lock().lock();
            let (expired_grants, expired_pending) = st.prune_expired(now);
            // Capture target appId for audit logging even if password verification fails.
            let inferred_app = request_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .and_then(|rid| st.pending.iter().find(|r| r.request_id == rid).map(|r| r.app_id.clone()))
                .unwrap_or_default();

            let mut verify_failure: Option<PasswordCheckFailure> = None;
            let res = st.grant_at(
                password,
                request_id,
                app_id,
                duration_secs,
                now,
                |pw| match verify(pw) {
                    Ok(user) => Ok(user),
                    Err(failure) => {
                        let msg = failure.message.clone();
                        verify_failure = Some(failure);
                        Err(msg)
                    }
                },
            );
            match res {
                Ok(grant) => {
                    let full_snapshot = st.snapshot_value(None, now);
                    (Ok(grant), expired_grants, expired_pending, full_snapshot)
                }
                Err(err) => {
                    if let Some(failure) = verify_failure {
                        if failure.reason == "no-password" {
                            if let Some(rid) = request_id {
                                if st.waiters.contains_key(rid) {
                                    st.pending.retain(|r| r.request_id != rid);
                                    st.resolve_waiter(rid, Err(ActionDenied::NoPassword));
                                }
                            }
                        }
                        if !inferred_app.is_empty() {
                            st.record_audit(
                                &inferred_app,
                                "Authenticate",
                                "denied: bad password",
                                now,
                                Some(Path::new(PRIVILEGED_LOG_PATH)),
                            );
                        }
                        let full_snapshot = st.snapshot_value(None, now);
                        (Err(failure), expired_grants, expired_pending, full_snapshot)
                    } else {
                        if !expired_grants.is_empty() || !expired_pending.is_empty() {
                            let snap = st.snapshot_value(None, now);
                            drop(st);
                            emit_expired_items(expired_grants, expired_pending, &snap);
                        }
                        return Err(err);
                    }
                }
            }
        };

        if !expired_grants.is_empty() || !expired_pending.is_empty() {
            emit_expired_items(expired_grants, expired_pending, &full_snapshot);
        }

        match auth_outcome {
            Ok(grant) => {
                let grant_json = grant.to_json_with_remaining(now);
                let signal_payload = merge_signal_fields(
                    full_snapshot.clone(),
                    json!({
                        "appId": grant.app_id,
                        "granted": true,
                        "reason": "authenticated",
                        "grant": grant_json,
                    }),
                );
                self.emit(SIGNAL_PRIVILEGE_CHANGED, signal_payload);

                Ok(json!({
                    "ok": true,
                    "granted": true,
                    "appId": grant.app_id,
                    "appName": grant.app_name,
                    "expiresAtMs": grant.expires_at_ms,
                    "grant": grant_json,
                    "state": full_snapshot,
                }))
            }
            Err(failure) => {
                let mut out = json!({
                    "ok": false,
                    "granted": false,
                    "reason": failure.reason,
                    "error": failure.message,
                    "retry_at_ms": failure.retry_at_ms,
                    "state": full_snapshot,
                });
                if let Some(left) = failure.attempts_left {
                    out["attempts_left"] = json!(left);
                }
                Ok(out)
            }
        }
    }

    pub fn dismiss(&self, body: &Value) -> Result<Value> {
        let request_id = body.get("requestId").and_then(|v| v.as_str());
        let app_id = body.get("appId").and_then(|v| v.as_str());
        let now = now_ms();

        let (removed, expired_grants, expired_pending, pending, snapshot) = {
            let mut st = state_lock().lock();
            let (expired_grants, expired_pending) = st.prune_expired(now);
            let removed = st.dismiss_by(request_id, app_id);
            let pending = st.pending.clone();
            let snapshot = st.snapshot_value(None, now);
            (removed, expired_grants, expired_pending, pending, snapshot)
        };

        if !expired_grants.is_empty() || !expired_pending.is_empty() {
            emit_expired_items(expired_grants, expired_pending, &snapshot);
        }

        for req in &removed {
            let payload = merge_signal_fields(
                snapshot.clone(),
                json!({
                    "appId": req.app_id,
                    "granted": false,
                    "reason": "dismissed",
                    "grant": Value::Null,
                }),
            );
            self.emit(SIGNAL_PRIVILEGE_CHANGED, payload);
        }

        Ok(json!({
            "ok": true,
            "dismissed": !removed.is_empty(),
            "count": removed.len(),
            "pending": pending,
            "state": snapshot,
        }))
    }

    pub fn revoke(&self, body: &Value) -> Result<Value> {
        let app_id = body
            .get("appId")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let now = now_ms();
        if let Some(id) = app_id {
            let revoked = revoke_for_app(id, "revoked");
            let state = state_lock().lock().snapshot_value(Some(id), now);
            Ok(json!({ "ok": true, "revoked": revoked, "appId": id, "state": state }))
        } else {
            let apps = revoke_all("revoked");
            let state = state_lock().lock().snapshot_value(None, now);
            Ok(json!({ "ok": true, "revoked": !apps.is_empty(), "revokedApps": apps, "state": state }))
        }
    }

    pub fn get_state(&self, body: &Value) -> Result<Value> {
        let app_id = body
            .get("appId")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let now = now_ms();

        let (expired_grants, expired_pending, full_snapshot, requested_snapshot) = {
            let mut st = state_lock().lock();
            let (expired_grants, expired_pending) = st.prune_expired(now);
            let full_snapshot = st.snapshot_value(None, now);
            let requested_snapshot = if app_id.is_some() {
                st.snapshot_value(app_id, now)
            } else {
                full_snapshot.clone()
            };
            (expired_grants, expired_pending, full_snapshot, requested_snapshot)
        };

        if !expired_grants.is_empty() || !expired_pending.is_empty() {
            emit_expired_items(expired_grants, expired_pending, &full_snapshot);
        }

        Ok(requested_snapshot)
    }
}

fn parse_password_failure(msg: &str) -> PasswordCheckFailure {
    let lower = msg.to_lowercase();
    let is_wait =
        lower.contains("wait") || lower.contains("too many") || lower.contains("try again");

    let retry_at_ms = extract_tagged_u64(&lower, "until ", "ms")
        .or_else(|| extract_tagged_u64(&lower, "after ", "ms"));
    let attempts_left = extract_tagged_u32(&lower, "(", " attempts left").or_else(|| {
        if retry_at_ms.is_some() {
            Some(0)
        } else {
            None
        }
    });
    let reason = if is_wait && !lower.contains("incorrect password") {
        "wait"
    } else if is_wait && attempts_left == Some(0) && lower.starts_with("too many") {
        "wait"
    } else if lower.contains("wait") || lower.contains("too many") || lower.contains("try again") {
        if lower.starts_with("incorrect password") {
            "wrong"
        } else {
            "wait"
        }
    } else {
        "wrong"
    };

    PasswordCheckFailure {
        reason,
        message: msg.to_string(),
        attempts_left,
        retry_at_ms,
    }
}

fn extract_tagged_u64(haystack: &str, prefix: &str, suffix: &str) -> Option<u64> {
    let start = haystack.find(prefix)? + prefix.len();
    let rest = &haystack[start..];
    let end = rest.find(suffix)?;
    rest[..end].trim().parse::<u64>().ok()
}

fn extract_tagged_u32(haystack: &str, prefix: &str, suffix: &str) -> Option<u32> {
    let start = haystack.find(prefix)? + prefix.len();
    let rest = &haystack[start..];
    let end = rest.find(suffix)?;
    rest[..end].trim().parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_authenticate_and_expiry_lifecycle() {
        let mut st = PrivilegeState::default();
        let (req, existing) = st
            .request_at("files", Some("Edit /etc/hosts"), 1_000)
            .unwrap();
        assert!(existing.is_none());
        assert_eq!(req.app_id, "files");
        // Nova has no such app here: the name falls back to the id, never the caller's text.
        assert_eq!(req.app_name, "files");
        assert_eq!(req.reason, "Edit /etc/hosts");
        assert_eq!(st.pending.len(), 1);

        // Replacing pending request from same app does not duplicate.
        let (req2, _) = st
            .request_at("files", Some("Write config"), 1_500)
            .unwrap();
        assert_eq!(st.pending.len(), 1);
        assert_ne!(req2.request_id, req.request_id);

        // Wrong password leaves pending request intact and does not grant.
        let err = st
            .grant_at("bad", Some(&req2.request_id), None, Some(10), 2_000, |_| {
                Err("Incorrect password".to_string())
            })
            .unwrap_err();
        assert!(err.to_string().contains("Incorrect password"));
        assert_eq!(st.pending.len(), 1);
        assert!(!st.has_grant_at("files", 2_000).0);

        // Correct password grants for 10s (until 12_000ms) and clears pending.
        let grant = st
            .grant_at("good", Some(&req2.request_id), None, Some(10), 2_000, |_| {
                Ok("geckouser".to_string())
            })
            .unwrap();
        assert_eq!(grant.app_id, "files");
        assert_eq!(grant.granted_by, "geckouser");
        assert_eq!(grant.expires_at_ms, 12_000);
        assert!(st.pending.is_empty());

        // Active before expiry, expired at/after expiry.
        assert!(st.has_grant_at("files", 11_999).0);
        let (still_active, expired) = st.has_grant_at("files", 12_000);
        assert!(!still_active);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].app_id, "files");
    }

    #[test]
    fn dismiss_and_revoke_behavior() {
        let mut st = PrivilegeState::default();
        let (r1, _) = st.request_at("files", Some("reason 1"), 100).unwrap();
        let (r2, _) = st.request_at("notes", Some("reason 2"), 200).unwrap();
        assert_eq!(st.pending.len(), 2);

        let dismissed = st.dismiss_by(Some(&r1.request_id), None);
        assert_eq!(dismissed.len(), 1);
        assert_eq!(dismissed[0].app_id, "files");
        assert_eq!(st.pending.len(), 1);

        st.grant_at("pw", Some(&r2.request_id), None, None, 300, |_| {
            Ok("geckouser".to_string())
        })
        .unwrap();
        assert!(st.has_grant_at("notes", 300).0);

        let (revoked, _, _) = st.revoke_app_at("notes", 400);
        assert!(revoked);
        assert!(!st.has_grant_at("notes", 400).0);

        // Grant two apps then revoke_all clears both grants and pending.
        st.grant_at("pw", None, Some("files"), Some(60), 500, |_| {
            Ok("geckouser".to_string())
        })
        .unwrap();
        st.grant_at("pw", None, Some("settings"), Some(60), 500, |_| {
            Ok("geckouser".to_string())
        })
        .unwrap();
        st.request_at("terminal", None, 500).unwrap();

        let (apps, _, _) = st.revoke_all_at(600);
        assert_eq!(apps, vec!["files", "settings", "terminal"]);
        assert!(st.grants.is_empty());
        assert!(st.pending.is_empty());
    }

    #[test]
    fn audit_log_caps_at_100_and_writes_best_effort_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_file = dir.path().join("pyxis-privileged.log");
        let mut st = PrivilegeState::default();
        for i in 0..125u64 {
            st.record_audit(
                "files",
                "writeFile",
                &format!("/etc/test-{i}"),
                1_000 + i,
                Some(&log_file),
            );
        }
        assert_eq!(st.audit_log.len(), AUDIT_LOG_CAP);
        assert_eq!(st.audit_log.front().unwrap().path, "/etc/test-25");
        assert_eq!(st.audit_log.back().unwrap().path, "/etc/test-124");

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert_eq!(written.lines().count(), 125);
        assert!(written.contains("app=files method=writeFile path=/etc/test-124"));
    }

    #[test]
    fn global_service_dispatch_and_public_helpers_work() {
        let svc = PrivilegeService::new();
        let app_id = "com.test.privilege.global";

        let req_res = svc
            .call(
                "request",
                &json!({ "appId": app_id, "reason": "Edit config" }),
            )
            .unwrap();
        assert_eq!(req_res["alreadyGranted"], false);
        assert_eq!(req_res["pending"], true);
        assert_eq!(req_res["appId"], app_id);
        let req_id = req_res["requestId"].as_str().unwrap().to_string();

        let state = svc
            .call("getState", &json!({ "appId": app_id }))
            .unwrap();
        assert_eq!(state["granted"], false);
        assert_eq!(state["active"], false);
        assert!(state["pending"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["requestId"] == req_id));

        // Bad password returns structured refusal instead of IPC error.
        let wrong = svc
            .authenticate_with(
                &json!({ "password": "wrong", "requestId": req_id }),
                |_| Err("Incorrect password".to_string()),
            )
            .unwrap();
        assert_eq!(wrong["ok"], false);
        assert_eq!(wrong["granted"], false);
        assert_eq!(wrong["reason"], "wrong");

        let wait = svc
            .authenticate_with(
                &json!({ "password": "wrong", "requestId": req_id }),
                |_| Err("Too many failed attempts. Try again in 30s".to_string()),
            )
            .unwrap();
        assert_eq!(wait["ok"], false);
        assert_eq!(wait["granted"], false);
        assert_eq!(wait["reason"], "wait");

        let dis = svc
            .call("dismiss", &json!({ "requestId": req_id }))
            .unwrap();
        assert_eq!(dis["ok"], true);
        assert_eq!(dis["dismissed"], true);

        // Insert a grant through internal state using a unique app id to test
        // public helpers without touching other parallel tests' grants.
        {
            let mut st = state_lock().lock();
            st.grant_at("pw", None, Some(app_id), Some(60), now_ms(), |_| {
                Ok("geckouser".to_string())
            })
            .unwrap();
        }
        assert!(has_grant(app_id));

        let state2 = svc.call("GetState", &json!({ "appId": app_id })).unwrap();
        assert_eq!(state2["granted"], true);
        assert_eq!(state2["active"], true);
        assert!(state2["grant"]["grantedAt"].as_u64().is_some());
        assert!(state2["grant"]["expiresAt"].as_u64().is_some());
        assert!(state2["grant"]["remainingSecs"].as_u64().unwrap() > 0);

        log_elevated_op(app_id, "writeFile", "/etc/example.conf");
        let state3 = svc.call("GetState", &json!({ "appId": app_id })).unwrap();
        assert!(state3["auditLog"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["appId"] == app_id && e["path"] == "/etc/example.conf"));

        let rev = svc.call("revoke", &json!({ "appId": app_id })).unwrap();
        assert_eq!(rev["ok"], true);
        assert_eq!(rev["revoked"], true);
        assert!(!has_grant(app_id));
    }

    #[test]
    fn pending_requests_expire_after_ttl() {
        let mut st = PrivilegeState::default();
        let (req, _) = st
            .request_at("com.test.ttl", Some("Reason"), 10_000)
            .unwrap();
        assert_eq!(st.pending.len(), 1);

        // Still live 1ms before TTL.
        let (grants, pending) = st.prune_expired(10_000 + REQUEST_TTL_MS - 1);
        assert!(grants.is_empty());
        assert!(pending.is_empty());
        assert_eq!(st.pending.len(), 1);

        // Pruned once REQUEST_TTL_MS has elapsed.
        let (grants2, pending2) = st.prune_expired(10_000 + REQUEST_TTL_MS);
        assert!(grants2.is_empty());
        assert_eq!(pending2.len(), 1);
        assert_eq!(pending2[0].request_id, req.request_id);
        assert!(st.pending.is_empty());
    }

    #[test]
    fn authenticate_returns_attempts_left_and_retry_at_ms_on_failure_and_cooldown() {
        let svc = PrivilegeService::new();
        let app_id = "com.test.privilege.cooldown";
        let req_res = svc
            .call(
                "request",
                &json!({ "appId": app_id, "reason": "Edit system config" }),
            )
            .unwrap();
        let req_id = req_res["requestId"].as_str().unwrap().to_string();

        // String-based wrong try with attempts left parsed via authenticate_with.
        let wrong_attempt = svc
            .authenticate_with(
                &json!({ "password": "bad", "requestId": req_id }),
                |_| Err("Incorrect password (4 attempts left)".to_string()),
            )
            .unwrap();
        assert_eq!(wrong_attempt["ok"], false);
        assert_eq!(wrong_attempt["granted"], false);
        assert_eq!(wrong_attempt["reason"], "wrong");
        assert_eq!(wrong_attempt["attempts_left"], 4);
        assert_eq!(wrong_attempt["retry_at_ms"], Value::Null);

        // Structured lockout failure via authenticate_with_detailed.
        let lockout = svc
            .authenticate_with_detailed(
                &json!({ "password": "bad", "requestId": req_id }),
                |_| {
                    Err(PasswordCheckFailure {
                        reason: "wait",
                        message: "Too many failed attempts; wait until 999999ms".to_string(),
                        attempts_left: Some(0),
                        retry_at_ms: Some(999_999),
                    })
                },
            )
            .unwrap();
        assert_eq!(lockout["ok"], false);
        assert_eq!(lockout["granted"], false);
        assert_eq!(lockout["reason"], "wait");
        assert_eq!(lockout["attempts_left"], 0);
        assert_eq!(lockout["retry_at_ms"], 999_999);

        let _ = svc.call("dismiss", &json!({ "requestId": req_id }));
    }

    #[test]
    fn caller_text_is_not_trusted_in_the_prompt() {
        let mut st = PrivilegeState::default();
        let long = format!("Edit\n{}\u{7}{}", "x".repeat(300), "y");
        let (req, _) = st.request_at("com.test.sanitize", Some(&long), 1).unwrap();
        assert!(!req.reason.chars().any(|c| c.is_control()));
        assert_eq!(req.reason.chars().count(), 200);
        assert!(req.reason.starts_with("Editxxx"));
        assert_eq!(req.app_name, "com.test.sanitize");

        // `appName` in the body is ignored.
        let svc = PrivilegeService::new();
        let res = svc
            .call("request", &json!({"appId": "com.test.spoof", "appName": "System Update", "reason": "hi"}))
            .unwrap();
        assert_eq!(res["appName"], "com.test.spoof");
        let _ = svc.call("dismiss", &json!({"requestId": res["requestId"]}));
    }

    #[test]
    fn authenticate_needs_a_request_id() {
        let svc = PrivilegeService::new();
        let res = svc
            .call("request", &json!({"appId": "com.test.needsid", "reason": "x"}))
            .unwrap();
        let rid = res["requestId"].as_str().unwrap().to_string();
        for body in [
            json!({"password": "p"}),
            json!({"password": "p", "appId": "com.test.needsid"}),
            json!({"password": "p", "requestId": "  "}),
        ] {
            let mut verified = false;
            let e = svc
                .authenticate_with(&body, |_| {
                    verified = true;
                    Ok("geckouser".into())
                })
                .unwrap_err();
            assert!(e.to_string().contains("requestId"), "{e}");
            assert!(!verified, "the password must not be checked");
        }
        // A pending request is not picked up by guessing.
        let mut st = PrivilegeState::default();
        st.request_at("com.test.guess", None, 1).unwrap();
        assert!(st.grant_at("p", None, None, None, 2, |_| Ok("u".into())).is_err());
        let _ = svc.call("dismiss", &json!({"requestId": rid}));
    }

    #[test]
    fn a_no_password_failure_reaches_the_reply_as_a_reason() {
        let svc = PrivilegeService::new();
        let res = svc.call("request", &json!({"appId": "com.test.nopw", "reason": "x"})).unwrap();
        let rid = res["requestId"].as_str().unwrap().to_string();
        let out = svc
            .authenticate_with_detailed(&json!({"password": "", "requestId": rid}), |_| {
                Err(PasswordCheckFailure {
                    reason: "no-password",
                    message: "set a password first".into(),
                    attempts_left: None,
                    retry_at_ms: None,
                })
            })
            .unwrap();
        assert_eq!(out["ok"], false);
        assert_eq!(out["reason"], "no-password");
        assert_eq!(out["error"], "set a password first");
        let _ = svc.call("dismiss", &json!({"requestId": rid}));
    }

    #[test]
    fn the_audit_log_escapes_control_characters_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let log_file = dir.path().join("priv.log");
        let mut st = PrivilegeState::default();
        st.record_audit("files", "writeFile", "/etc/a\n2030-01-01 app=root method=x path=/etc/shadow\r\u{1b}[2J", 5, Some(&log_file));
        let written = std::fs::read_to_string(&log_file).unwrap();
        assert_eq!(written.lines().count(), 1, "{written:?}");
        assert!(written.contains("\\n2030-01-01"));
        assert!(!written.contains('\u{1b}'));
        assert!(!st.audit_log[0].path.contains('\n'));
        let mode = std::fs::metadata(&log_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // ---- general actions (`authorize`) ----

    use crate::actions::ActionRequest;

    fn act(action: &'static str) -> ActionRequest {
        ActionRequest { action, details: json!({"package": "weather"}) }
    }

    async fn pending_id(app: &str) -> String {
        for _ in 0..200 {
            if let Some(id) = state_lock()
                .lock()
                .pending
                .iter()
                .find(|r| r.app_id == app)
                .map(|r| r.request_id.clone())
            {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("no pending request for {app}");
    }

    fn put_grant(app: &str, action: &str) {
        let now = now_ms();
        state_lock().lock().action_grants.insert(
            (app.to_string(), action.to_string()),
            Grant {
                app_id: app.into(),
                app_name: app.into(),
                reason: String::new(),
                granted_by: "geckouser".into(),
                granted_at_ms: now,
                expires_at_ms: now + 60_000,
                action: action.into(),
            },
        );
    }

    #[tokio::test]
    async fn a_live_grant_returns_at_once() {
        put_grant("com.test.a1", "network.manage");
        let svc = PrivilegeService::new();
        assert_eq!(svc.authorize("com.test.a1", act("network.manage")).await, Ok(()));
        // A grant is per action.
        assert!(state_lock().lock().action_grants.contains_key(&("com.test.a1".into(), "network.manage".into())));
        assert!(!state_lock().lock().pending.iter().any(|r| r.app_id == "com.test.a1"));
    }

    #[tokio::test]
    async fn dismiss_denies() {
        let svc = PrivilegeService::new();
        let s2 = svc.clone();
        let h = tokio::spawn(async move { s2.authorize("com.test.a2", act("package.install")).await });
        let id = pending_id("com.test.a2").await;
        svc.call("Dismiss", &json!({"requestId": id})).unwrap();
        assert_eq!(h.await.unwrap(), Err(ActionDenied::Denied));
        assert!(!state_lock().lock().pending.iter().any(|r| r.app_id == "com.test.a2"));
    }

    #[tokio::test]
    async fn authenticate_approves_once_for_ask_every_time() {
        let svc = PrivilegeService::new();
        let s2 = svc.clone();
        let h = tokio::spawn(async move { s2.authorize("com.test.a3", act("package.remove")).await });
        let id = pending_id("com.test.a3").await;
        let out = svc
            .authenticate_with(&json!({"requestId": id, "password": "pw"}), |_| Ok("geckouser".into()))
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(h.await.unwrap(), Ok(()));
        // Not reusable: the next call asks again.
        let s3 = svc.clone();
        let h2 = tokio::spawn(async move { s3.authorize("com.test.a3", act("package.remove")).await });
        let id2 = pending_id("com.test.a3").await;
        assert_ne!(id, id2);
        svc.call("Dismiss", &json!({"requestId": id2})).unwrap();
        assert_eq!(h2.await.unwrap(), Err(ActionDenied::Denied));
    }

    #[tokio::test]
    async fn ask_then_cache_reuses_within_the_window() {
        let svc = PrivilegeService::new();
        let s2 = svc.clone();
        let h = tokio::spawn(async move { s2.authorize("com.test.a4", act("network.manage")).await });
        let id = pending_id("com.test.a4").await;
        svc.authenticate_with(&json!({"requestId": id, "password": "pw"}), |_| Ok("geckouser".into()))
            .unwrap();
        assert_eq!(h.await.unwrap(), Ok(()));
        // Reused at once, no new pending request.
        assert_eq!(svc.authorize("com.test.a4", act("network.manage")).await, Ok(()));
        // After the window it asks again.
        {
            let mut st = state_lock().lock();
            let g = st.action_grants.get_mut(&("com.test.a4".into(), "network.manage".into())).unwrap();
            g.expires_at_ms = now_ms() - 1;
        }
        let s3 = svc.clone();
        let h2 = tokio::spawn(async move { s3.authorize("com.test.a4", act("network.manage")).await });
        let id2 = pending_id("com.test.a4").await;
        svc.call("Dismiss", &json!({"requestId": id2})).unwrap();
        assert_eq!(h2.await.unwrap(), Err(ActionDenied::Denied));
    }

    #[tokio::test]
    async fn a_held_call_times_out_and_leaves_nothing_pending() {
        // tokio's `test-util` (paused time) is not enabled for this crate, so
        // the wait is shortened through the injectable duration instead.
        let svc = PrivilegeService::new();
        let r = svc
            .authorize_with_timeout("com.test.a5", act("accounts.manage"), Duration::from_millis(40))
            .await;
        assert_eq!(r, Err(ActionDenied::Timeout));
        let st = state_lock().lock();
        assert!(!st.pending.iter().any(|r| r.app_id == "com.test.a5"));
        assert!(st.waiters.is_empty() || !st.waiters.keys().any(|k| st.pending.iter().all(|p| &p.request_id != k)));
    }

    #[tokio::test]
    async fn no_password_resolves_the_waiter() {
        let svc = PrivilegeService::new();
        let s2 = svc.clone();
        let h = tokio::spawn(async move { s2.authorize("com.test.a6", act("package.install")).await });
        let id = pending_id("com.test.a6").await;
        let out = svc
            .authenticate_with_detailed(&json!({"requestId": id, "password": ""}), |_| {
                Err(PasswordCheckFailure {
                    reason: "no-password",
                    message: "set a password first".into(),
                    attempts_left: None,
                    retry_at_ms: None,
                })
            })
            .unwrap();
        assert_eq!(out["reason"], "no-password");
        assert_eq!(h.await.unwrap(), Err(ActionDenied::NoPassword));
    }

    #[tokio::test]
    async fn a_wrong_password_keeps_the_call_held() {
        let svc = PrivilegeService::new();
        let s2 = svc.clone();
        let h = tokio::spawn(async move { s2.authorize("com.test.a7", act("package.install")).await });
        let id = pending_id("com.test.a7").await;
        let out = svc
            .authenticate_with(&json!({"requestId": id, "password": "x"}), |_| Err("Incorrect password".into()))
            .unwrap();
        assert_eq!(out["ok"], false);
        assert!(!h.is_finished());
        svc.call("Dismiss", &json!({"requestId": id})).unwrap();
        assert_eq!(h.await.unwrap(), Err(ActionDenied::Denied));
    }

    #[test]
    fn error_json_shape() {
        for (d, r) in [
            (ActionDenied::Denied, "denied"),
            (ActionDenied::Timeout, "timeout"),
            (ActionDenied::NoPassword, "no-password"),
        ] {
            let v: Value = serde_json::from_str(&d.to_error_json()).unwrap();
            assert_eq!(v["reason"], r);
            assert!(v["error"].as_str().unwrap().len() > 3);
        }
    }

    #[tokio::test]
    async fn the_signal_carries_action_and_details() {
        use std::sync::Mutex as StdMutex;
        static SEEN: StdMutex<Vec<(String, Value)>> = StdMutex::new(Vec::new());
        *signal_slot().write() = Some(Arc::new(|name: &str, v: Value| {
            SEEN.lock().unwrap().push((name.to_string(), v));
        }));
        let svc = PrivilegeService::new();
        let s2 = svc.clone();
        let h = tokio::spawn(async move {
            s2.authorize("com.test.a8", ActionRequest { action: "network.manage", details: json!({"ssid": "Home"}) }).await
        });
        let id = pending_id("com.test.a8").await;
        let sig = SEEN
            .lock()
            .unwrap()
            .iter()
            .find(|(n, v)| n == SIGNAL_PRIVILEGE_REQUESTED && v["appId"] == "com.test.a8")
            .map(|(_, v)| v.clone())
            .expect("PrivilegeRequested");
        assert_eq!(sig["action"], "network.manage");
        assert_eq!(sig["details"], json!({"ssid": "Home"}));
        assert_eq!(sig["requestId"], id);
        assert_eq!(sig["durationSecs"], 300);
        // GetState carries it too, and so does a filesystem request.
        let st = svc.get_state(&json!({})).unwrap();
        assert!(st["pending"].as_array().unwrap().iter().any(|p| p["requestId"] == id.as_str() && p["action"] == "network.manage"));
        let fs = svc.call("request", &json!({"appId": "com.test.a9", "reason": "x"})).unwrap();
        assert_eq!(fs["request"]["action"], "filesystem.system-write");
        let fs_sig = SEEN.lock().unwrap().iter().find(|(n, v)| n == SIGNAL_PRIVILEGE_REQUESTED && v["appId"] == "com.test.a9").map(|(_, v)| v.clone()).unwrap();
        assert_eq!(fs_sig["action"], "filesystem.system-write");
        let _ = svc.call("Dismiss", &json!({"requestId": fs["requestId"]}));
        svc.call("Dismiss", &json!({"requestId": id})).unwrap();
        let _ = h.await;
        *signal_slot().write() = None;
    }

    #[tokio::test]
    async fn revoking_an_app_denies_its_held_calls_and_drops_grants() {
        let svc = PrivilegeService::new();
        put_grant("com.test.b1", "network.manage");
        let s2 = svc.clone();
        let h = tokio::spawn(async move { s2.authorize("com.test.b1", act("package.install")).await });
        pending_id("com.test.b1").await;
        assert!(revoke_for_app("com.test.b1", "revoked"));
        assert_eq!(h.await.unwrap(), Err(ActionDenied::Denied));
        assert!(!state_lock().lock().action_grants.keys().any(|(a, _)| a == "com.test.b1"));
    }

    #[test]
    fn revoke_all_clears_action_grants_and_waiters() {
        let mut st = PrivilegeState::default();
        let now = now_ms();
        let g = |app: &str| Grant {
            app_id: app.into(), app_name: app.into(), reason: String::new(),
            granted_by: "u".into(), granted_at_ms: now, expires_at_ms: now + 9_000,
            action: "network.manage".into(),
        };
        st.action_grants.insert(("x".into(), "network.manage".into()), g("x"));
        let (apps, _, _) = st.revoke_all_at(now);
        assert_eq!(apps, vec!["x".to_string()]);
        assert!(st.action_grants.is_empty());
    }
}
