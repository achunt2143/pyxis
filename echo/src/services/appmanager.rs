//! Application Manager Service
//! 
//! Manages application lifecycle, installation, and execution.
//! Stub implementation - awaiting windowManager integration.

use serde_json::{json, Value};
use anyhow::{Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};

pub struct AppManagerService;

// Simple app registry stored in JSON
fn get_app_registry_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".config/geckoos")
        .join("apps.json")
}

impl AppManagerService {
    pub fn launch(_app_id: &str, _params: Option<Value>) -> Result<Value> {
        // Stub: Full implementation awaits windowManager service
        Ok(json!({
            "success": false,
            "message": "windowManager service not yet implemented",
            "appId": _app_id
        }))
    }

    pub fn close(_app_id: &str) -> Result<Value> {
        Ok(json!({
            "success": false,
            "message": "windowManager service not yet implemented",
            "appId": _app_id
        }))
    }

    pub fn list() -> Result<Value> {
        let registry_path = get_app_registry_path();
        
        if !registry_path.exists() {
            return Ok(json!({
                "apps": [],
                "count": 0
            }));
        }

        let content = fs::read_to_string(&registry_path)?;
        let apps: Value = serde_json::from_str(&content)?;

        Ok(json!({
            "apps": apps,
            "count": apps.as_array().map(|a| a.len()).unwrap_or(0)
        }))
    }

    pub fn get_info(app_id: &str) -> Result<Value> {
        let content = fs::read_to_string(get_app_registry_path())
            .unwrap_or_else(|_| "[]".to_string());
        
        let apps: Vec<Value> = serde_json::from_str(&content)?;
        
        for app in apps {
            if let Some(id) = app.get("id").and_then(|v| v.as_str()) {
                if id == app_id {
                    return Ok(app);
                }
            }
        }

        Err(anyhow!("App not found: {}", app_id))
    }

    pub fn install(_app_path: &str) -> Result<Value> {
        Ok(json!({
            "success": false,
            "message": "Installation not yet implemented"
        }))
    }

    pub fn uninstall(_app_id: &str) -> Result<Value> {
        Ok(json!({
            "success": false,
            "message": "Uninstallation not yet implemented"
        }))
    }

    pub fn kill(_app_id: &str) -> Result<Value> {
        Ok(json!({
            "success": false,
            "message": "windowManager service not yet implemented"
        }))
    }

    pub fn restart(app_id: &str) -> Result<Value> {
        Self::close(app_id)?;
        Self::launch(app_id, None)
    }

    pub fn set_active(_app_id: &str) -> Result<Value> {
        Ok(json!({
            "success": false,
            "message": "windowManager service not yet implemented"
        }))
    }

    pub fn get_active() -> Result<Value> {
        Ok(json!({
            "appId": null,
            "message": "windowManager service not yet implemented"
        }))
    }
}
