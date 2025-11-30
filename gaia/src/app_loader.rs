// ============================================================================
// APP LOADER MODULE
// File: gaia/src/app_loader.rs
// Purpose: Dynamic application loading
// ============================================================================

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AppLoader {
    app_path: PathBuf,
    loaded_apps: Vec<LoadedApp>,
}

#[derive(Debug, Clone)]
pub struct LoadedApp {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub icon: String,
}

impl AppLoader {
    pub fn new(app_path: &str) -> Self {
        info!("📱 Initializing App Loader from: {}", app_path);
        
        AppLoader {
            app_path: PathBuf::from(app_path),
            loaded_apps: vec![
                LoadedApp {
                    id: "file-manager".to_string(),
                    name: "File Manager".to_string(),
                    path: PathBuf::from("./apps/file-manager"),
                    icon: "📁".to_string(),
                },
                LoadedApp {
                    id: "terminal".to_string(),
                    name: "Terminal".to_string(),
                    path: PathBuf::from("./apps/terminal"),
                    icon: "⌨️".to_string(),
                },
                LoadedApp {
                    id: "editor".to_string(),
                    name: "Text Editor".to_string(),
                    path: PathBuf::from("./apps/editor"),
                    icon: "📝".to_string(),
                },
                LoadedApp {
                    id: "media-player".to_string(),
                    name: "Media Player".to_string(),
                    path: PathBuf::from("./apps/media-player"),
                    icon: "🎵".to_string(),
                },
            ],
        }
    }

    pub fn load_app(&self, app_id: &str) -> Option<LoadedApp> {
        self.loaded_apps
            .iter()
            .find(|app| app.id == app_id)
            .cloned()
    }

    pub fn list_apps(&self) -> Vec<LoadedApp> {
        self.loaded_apps.clone()
    }

    pub fn reload(&mut self) {
        debug!("🔄 Reloading applications");
    }
}