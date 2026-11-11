//! Settings Service
//!
//! Provides persistent key-value configuration storage.
//!
//! `pyxis.*` keys are the system's own and are checked against `schema`; any
//! other key is an app's and is stored as given. Every change is signalled
//! (`SettingChanged` on `org.geckoOS.settings`, one per changed key), because
//! the Settings app and the shell are different origins: the signal is the
//! only way a change made in one reaches the other while both are open.

pub mod schema;

use anyhow::Result;
use serde_json::{json, Map, Value};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const SETTINGS_IFACE: &str = "org.geckoOS.settings";
pub const SIGNAL_SETTING_CHANGED: &str = "SettingChanged";

pub struct SettingsService;

/// One key whose stored entry a call changed. `value` and `previous` are what
/// `get` would answer after and before: the stored value, or the key's
/// default when nothing is stored (null for a key the schema doesn't list).
/// `exists` is whether a value is stored now, so a delete reads
/// `exists: false` with `value` the default.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingChange {
    pub key: String,
    pub value: Value,
    pub previous: Value,
    pub exists: bool,
}

impl SettingChange {
    pub fn signal_body(&self) -> Value {
        json!({
            "key": self.key,
            "value": self.value,
            "previous": self.previous,
            "exists": self.exists,
        })
    }
}

/// A write's reply, plus what it changed, for the caller to signal.
pub struct Outcome {
    pub reply: Value,
    pub changes: Vec<SettingChange>,
}

// Settings file location.
//
// Persistent, HOME-independent state under the writable data root Nova already
// uses (/var/lib/pyxis). The echo-daemon runs as root, so keying settings off
// $HOME was fragile: whatever HOME the daemon inherited decided where they
// landed, and an unset HOME sent them to /tmp — a tmpfs wiped on every reboot,
// which is exactly the "settings don't persist" symptom. An absolute state
// path removes that coupling. `PYXIS_SETTINGS_DIR` overrides it (dev/tests on
// a normal box where /var/lib/pyxis isn't writable).
//
// NOTE: this only survives reboots of the *same* image — if the rootfs is
// rebuilt between runs, nothing under it persists without a separate data
// partition/overlay (tracked separately).
fn get_settings_file() -> PathBuf {
    if let Ok(dir) = std::env::var("PYXIS_SETTINGS_DIR") {
        return PathBuf::from(dir).join("settings.json");
    }
    PathBuf::from("/var/lib/pyxis/settings.json")
}

// One lock around every read-modify-write in the process. Calls arrive on
// separate tasks, and each loads the whole file, changes it and writes it
// back: without the lock two `set`s at once both load the old file and the
// second write drops the first's key.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

fn write_lock() -> std::sync::MutexGuard<'static, ()> {
    // A panic while holding it left no half-written file (writes are
    // atomic), so a poisoned lock is still a good lock.
    WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The settings file at one path. The service uses the system's
/// (`get_settings_file`); tests use their own.
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Store { path: path.into() }
    }

    fn load(&self) -> Result<Map<String, Value>> {
        if !self.path.exists() {
            return Ok(Map::new());
        }

        let content = fs::read_to_string(&self.path)?;
        let json: Value = serde_json::from_str(&content)?;

        if let Value::Object(map) = json {
            Ok(map)
        } else {
            Ok(Map::new())
        }
    }

    /// Write the whole map atomically: a temp file in the same directory,
    /// fsynced, renamed over the old one. A crash mid-write leaves the old
    /// file or the new one, never an empty or partial one.
    fn save(&self, settings: &Map<String, Value>) -> Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir)?;

        let content = serde_json::to_string_pretty(&Value::Object(settings.clone()))?;
        let name = self.path.file_name().and_then(|n| n.to_str()).unwrap_or("settings.json");
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let written = (|| -> Result<()> {
            let mut file = File::create(&tmp)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            fs::rename(&tmp, &self.path)?;
            Ok(())
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written?;
        sync_dir(dir);
        Ok(())
    }

    fn effective(settings: &Map<String, Value>, key: &str) -> Value {
        settings.get(key).cloned().unwrap_or_else(|| schema::default_for(key))
    }

    pub fn get(&self, key: &str) -> Result<Value> {
        let settings = self.load()?;

        Ok(json!({
            "value": Self::effective(&settings, key),
            "key": key,
            "exists": settings.contains_key(key)
        }))
    }

    pub fn set(&self, key: &str, value: Value) -> Result<Outcome> {
        schema::validate(key, &value)?;

        let _guard = write_lock();
        let mut settings = self.load()?;
        let previous = Self::effective(&settings, key);
        let mut changes = Vec::new();
        if settings.get(key) != Some(&value) {
            settings.insert(key.to_string(), value.clone());
            self.save(&settings)?;
            changes.push(SettingChange { key: key.to_string(), value: value.clone(), previous, exists: true });
        }

        Ok(Outcome {
            reply: json!({
                "success": true,
                "key": key,
                "value": value
            }),
            changes,
        })
    }

    pub fn get_all(&self) -> Result<Value> {
        let settings = self.load()?;

        Ok(json!({
            "settings": Value::Object(settings.clone()),
            "count": settings.len(),
            "defaults": Value::Object(schema::defaults()),
        }))
    }

    pub fn delete(&self, key: &str) -> Result<Outcome> {
        let _guard = write_lock();
        let mut settings = self.load()?;
        let mut changes = Vec::new();
        if let Some(previous) = settings.remove(key) {
            self.save(&settings)?;
            changes.push(SettingChange {
                key: key.to_string(),
                value: schema::default_for(key),
                previous,
                exists: false,
            });
        }

        Ok(Outcome {
            reply: json!({
                "success": !changes.is_empty(),
                "key": key
            }),
            changes,
        })
    }

    pub fn clear(&self) -> Result<Outcome> {
        let _guard = write_lock();
        let settings = self.load()?;
        if self.path.exists() {
            fs::remove_file(&self.path)?;
            if let Some(dir) = self.path.parent() {
                sync_dir(dir);
            }
        }
        let changes = settings
            .into_iter()
            .map(|(key, previous)| SettingChange {
                value: schema::default_for(&key),
                key,
                previous,
                exists: false,
            })
            .collect();

        Ok(Outcome {
            reply: json!({
                "success": true
            }),
            changes,
        })
    }
}

fn sync_dir(dir: &Path) {
    // Makes the rename itself durable. Best effort: the data is already safe
    // in the renamed file, and not every filesystem lets a directory be synced.
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

fn store() -> Store {
    Store::at(get_settings_file())
}

impl SettingsService {
    pub fn get(key: &str) -> Result<Value> {
        store().get(key)
    }

    pub fn set(key: &str, value: Value) -> Result<Outcome> {
        store().set(key, value)
    }

    pub fn get_all() -> Result<Value> {
        store().get_all()
    }

    pub fn delete(key: &str) -> Result<Outcome> {
        store().delete(key)
    }

    pub fn clear() -> Result<Outcome> {
        store().clear()
    }

    pub fn reset() -> Result<Outcome> {
        // Every key back to its default, which is what an absent key reads as.
        Self::clear()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("settings.json"));
        (dir, store)
    }

    #[test]
    fn an_absent_key_reads_as_its_default() {
        let (_dir, store) = temp_store();
        let got = store.get("pyxis.wallpaper").unwrap();
        assert_eq!(got["value"], json!("daylight"));
        assert_eq!(got["exists"], json!(false));

        let got = store.get("com.example.thing").unwrap();
        assert_eq!(got["value"], Value::Null);
        assert_eq!(got["exists"], json!(false));

        store.set("pyxis.wallpaper", json!("forest")).unwrap();
        let got = store.get("pyxis.wallpaper").unwrap();
        assert_eq!(got["value"], json!("forest"));
        assert_eq!(got["exists"], json!(true));
    }

    #[test]
    fn a_bad_value_is_refused_and_nothing_is_written() {
        let (dir, store) = temp_store();
        let err = store.set("pyxis.theme", json!(42)).err().expect("42 accepted as a theme");
        assert!(err.to_string().contains("pyxis.theme"), "{err}");
        assert!(!dir.path().join("settings.json").exists());
        // An app's own key is stored as given.
        store.set("com.example.notes.font", json!({"size": 14})).unwrap();
        assert_eq!(store.get("com.example.notes.font").unwrap()["value"], json!({"size": 14}));
    }

    #[test]
    fn each_write_reports_one_change_per_changed_key() {
        let (_dir, store) = temp_store();

        let out = store.set("pyxis.theme", json!("dark")).unwrap();
        assert_eq!(
            out.changes,
            vec![SettingChange { key: "pyxis.theme".into(), value: json!("dark"), previous: json!("light"), exists: true }]
        );
        // The same value again changes nothing, so it signals nothing.
        assert!(store.set("pyxis.theme", json!("dark")).unwrap().changes.is_empty());

        let out = store.set("pyxis.theme", json!("auto")).unwrap();
        assert_eq!(out.changes.len(), 1);
        assert_eq!(out.changes[0].previous, json!("dark"));

        let out = store.delete("pyxis.theme").unwrap();
        assert_eq!(
            out.changes,
            vec![SettingChange { key: "pyxis.theme".into(), value: json!("light"), previous: json!("auto"), exists: false }]
        );
        assert_eq!(out.reply["success"], json!(true));
        let out = store.delete("pyxis.theme").unwrap();
        assert!(out.changes.is_empty());
        assert_eq!(out.reply["success"], json!(false));

        store.set("pyxis.dnd", json!(true)).unwrap();
        store.set("pyxis.wallpaper.pinnedTime", json!(1080)).unwrap();
        store.set("com.example.x", json!(1)).unwrap();
        let mut changes = store.clear().unwrap().changes;
        changes.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(
            changes,
            vec![
                SettingChange { key: "com.example.x".into(), value: Value::Null, previous: json!(1), exists: false },
                SettingChange { key: "pyxis.dnd".into(), value: json!(false), previous: json!(true), exists: false },
                SettingChange {
                    key: "pyxis.wallpaper.pinnedTime".into(),
                    value: Value::Null,
                    previous: json!(1080),
                    exists: false
                },
            ]
        );
        assert!(store.clear().unwrap().changes.is_empty());
    }

    #[test]
    fn the_signal_carries_key_value_previous_and_exists() {
        let change = SettingChange { key: "pyxis.clock24h".into(), value: json!(true), previous: json!(false), exists: true };
        assert_eq!(
            change.signal_body(),
            json!({"key": "pyxis.clock24h", "value": true, "previous": false, "exists": true})
        );
    }

    #[test]
    fn concurrent_sets_all_land() {
        let (dir, store) = temp_store();
        let store = Arc::new(store);
        let keys: Vec<String> = (0..16).map(|i| format!("com.example.k{i}")).collect();
        let threads: Vec<_> = keys
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, key)| {
                let store = store.clone();
                std::thread::spawn(move || store.set(&key, json!(i)).unwrap())
            })
            .collect();
        // Two system keys at once, too.
        let a = { let s = store.clone(); std::thread::spawn(move || s.set("pyxis.theme", json!("dark")).unwrap()) };
        let b = { let s = store.clone(); std::thread::spawn(move || s.set("pyxis.wallpaper", json!("mochi")).unwrap()) };
        for t in threads {
            t.join().unwrap();
        }
        a.join().unwrap();
        b.join().unwrap();

        let all = store.get_all().unwrap();
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(all["settings"][key], json!(i), "{key} was lost");
        }
        assert_eq!(all["settings"]["pyxis.theme"], json!("dark"));
        assert_eq!(all["settings"]["pyxis.wallpaper"], json!("mochi"));
        // No temp file left behind.
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "settings.json")
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    #[test]
    fn get_all_lists_the_defaults() {
        let (_dir, store) = temp_store();
        let all = store.get_all().unwrap();
        assert_eq!(all["count"], json!(0));
        assert_eq!(all["defaults"]["pyxis.wallpaper"], json!("daylight"));
        assert_eq!(all["defaults"]["pyxis.location"], Value::Null);
    }
}
