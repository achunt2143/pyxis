//! Settings Service
//! 
//! Provides persistent key-value configuration storage.

use serde_json::{json, Value, Map};
use anyhow::{Result, anyhow};
use std::fs;
use std::path::PathBuf;

pub struct SettingsService;

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

fn load_settings() -> Result<Map<String, Value>> {
    let settings_file = get_settings_file();
    
    if !settings_file.exists() {
        return Ok(Map::new());
    }

    let content = fs::read_to_string(&settings_file)?;
    let json: Value = serde_json::from_str(&content)?;
    
    if let Value::Object(map) = json {
        Ok(map)
    } else {
        Ok(Map::new())
    }
}

fn save_settings(settings: &Map<String, Value>) -> Result<()> {
    let settings_file = get_settings_file();
    
    // Ensure directory exists
    if let Some(parent) = settings_file.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = Value::Object(settings.clone());
    let content = serde_json::to_string_pretty(&json)?;
    fs::write(&settings_file, content)?;
    
    Ok(())
}

impl SettingsService {
    pub fn get(key: &str) -> Result<Value> {
        let settings = load_settings()?;
        
        Ok(json!({
            "value": settings.get(key),
            "key": key,
            "exists": settings.contains_key(key)
        }))
    }

    pub fn set(key: &str, value: Value) -> Result<Value> {
        let mut settings = load_settings()?;
        settings.insert(key.to_string(), value.clone());
        save_settings(&settings)?;

        Ok(json!({
            "success": true,
            "key": key,
            "value": value
        }))
    }

    pub fn get_all() -> Result<Value> {
        let settings = load_settings()?;

        Ok(json!({
            "settings": Value::Object(settings.clone()),
            "count": settings.len()
        }))
    }

    pub fn delete(key: &str) -> Result<Value> {
        let mut settings = load_settings()?;
        let existed = settings.remove(key).is_some();
        
        if existed {
            save_settings(&settings)?;
        }

        Ok(json!({
            "success": existed,
            "key": key
        }))
    }

    pub fn clear() -> Result<Value> {
        let settings_file = get_settings_file();
        
        if settings_file.exists() {
            fs::remove_file(&settings_file)?;
        }

        Ok(json!({
            "success": true
        }))
    }

    pub fn reset() -> Result<Value> {
        // Reset to defaults - for now just clear
        Self::clear()
    }
}
