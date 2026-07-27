//! `appinfo.json` generation and validation (spec §7, schemas/appinfo.schema.json).

use serde::{Deserialize, Serialize};

use crate::config::{Project, Runtime};
use crate::error::{PyxisError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub id: String,
    pub version: String,
    pub vendor: String,
    pub title: String,
    #[serde(rename = "type")]
    pub app_type: Runtime,
    pub main: String,
    pub icon: String,
    #[serde(rename = "uiRevision")]
    pub ui_revision: u32,
    #[serde(rename = "requiredPermissions", skip_serializing_if = "Option::is_none")]
    pub required_permissions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<String>>,
}

impl AppInfo {
    /// Compose the installed app manifest from the project config.
    pub fn from_project(project: &Project) -> Result<Self> {
        let c = &project.config;
        let main = match c.runtime {
            Runtime::Web => c
                .entry
                .clone()
                .ok_or_else(|| PyxisError::Manifest("web apps require `entry`".into()))?,
            Runtime::Native => project.native_main().map_err(|e| PyxisError::Manifest(e.to_string()))?,
        };
        let icon = std::path::Path::new(&c.icon)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| PyxisError::Manifest(format!("cannot derive icon filename from `{}`", c.icon)))?
            .to_string();

        let info = AppInfo {
            id: c.id.clone(),
            version: c.version.clone(),
            vendor: c.vendor.clone(),
            title: c.title.clone(),
            app_type: c.runtime,
            main,
            icon,
            ui_revision: 1,
            required_permissions: c.permissions.clone(),
            services: c.services.as_ref().map(|svcs| {
                svcs.iter().map(|s| s.name.clone()).collect()
            }),
        };
        info.validate()?;
        Ok(info)
    }

    /// Runtime-specific `main` rules (spec §7.3 / §20).
    pub fn validate(&self) -> Result<()> {
        match self.app_type {
            Runtime::Web => {
                if self.main.starts_with('/') {
                    return Err(PyxisError::Manifest(format!(
                        "web `main` must be relative, got `{}`",
                        self.main
                    )));
                }
            }
            Runtime::Native => {
                if !self.main.starts_with('/') {
                    return Err(PyxisError::Manifest(format!(
                        "native `main` must be an absolute path, got `{}`",
                        self.main
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn to_json(&self) -> String {
        // serde_json cannot fail on this plain struct
        serde_json::to_string_pretty(self).expect("appinfo serialization")
    }
}
