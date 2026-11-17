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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
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
            accent: c.accent.as_ref().map(|a| a.to_ascii_lowercase()),
            category: c.category.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PackageConfig, Project, ProjectConfig, SdkConfig, WebBuilder};
    use std::path::PathBuf;

    fn sample_project(category: Option<&str>) -> Project {
        Project {
            root: PathBuf::from("/tmp/sample-app"),
            config: ProjectConfig {
                id: "com.example.clock".into(),
                title: "Clock".into(),
                vendor: "Example".into(),
                version: "1.0.0".into(),
                runtime: Runtime::Web,
                source_dir: None,
                public_dir: None,
                build_dir: None,
                entry: Some("index.html".into()),
                icon: "assets/icon.png".into(),
                accent: Some("#0D9488".into()),
                category: category.map(str::to_string),
                sdk: Some(SdkConfig {
                    builder: WebBuilder::Static,
                    out_dir: None,
                }),
                pdk: None,
                permissions: None,
                services: None,
                signing: None,
                package: PackageConfig {
                    section: "applications".into(),
                    priority: "optional".into(),
                    architecture: "all".into(),
                    depends: None,
                    maintainer_email: None,
                    description: None,
                },
            },
        }
    }

    #[test]
    fn category_round_trips_and_omits_none() {
        let with_cat = AppInfo::from_project(&sample_project(Some("utilities"))).unwrap();
        assert_eq!(with_cat.category.as_deref(), Some("utilities"));
        let serialized = with_cat.to_json();
        assert!(serialized.contains("\"category\": \"utilities\""));
        let parsed: AppInfo = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed.category.as_deref(), Some("utilities"));

        let without_cat = AppInfo::from_project(&sample_project(None)).unwrap();
        assert_eq!(without_cat.category, None);
        let serialized_none = without_cat.to_json();
        assert!(!serialized_none.contains("\"category\""));
        let parsed_legacy: AppInfo = serde_json::from_str(&serialized_none).unwrap();
        assert_eq!(parsed_legacy.category, None);
    }
}
