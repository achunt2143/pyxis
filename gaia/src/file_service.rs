// ============================================================================
// FILE SERVICE MODULE
// File: gaia/src/file_service.rs
// Purpose: File system operations
// ============================================================================

use std::fs;
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub size: u64,
    pub modified: String,
}

pub struct FileService {
    base_path: String,
}

impl FileService {
    pub fn new() -> Self {
        info!("📂 Initializing File Service");
        
        FileService {
            base_path: "/home/user".to_string(),
        }
    }

    pub fn list_directory(&self, path: &str) -> Result<Vec<FileInfo>, String> {
        debug!("📂 Listing directory: {}", path);
        
        let full_path = format!("{}/{}", self.base_path, path);
        
        match fs::read_dir(&full_path) {
            Ok(entries) => {
                let files: Vec<FileInfo> = entries
                    .filter_map(|entry| {
                        entry.ok().and_then(|e| {
                            e.metadata().ok().map(|meta| {
                                FileInfo {
                                    name: e.file_name().to_string_lossy().to_string(),
                                    path: e.path().to_string_lossy().to_string(),
                                    is_directory: meta.is_dir(),
                                    size: meta.len(),
                                    modified: format!("{:?}", meta.modified()),
                                }
                            })
                        })
                    })
                    .collect();
                Ok(files)
            }
            Err(e) => Err(format!("Failed to read directory: {}", e)),
        }
    }

    pub fn read_file(&self, path: &str) -> Result<String, String> {
        debug!("📖 Reading file: {}", path);
        
        fs::read_to_string(path)
            .map_err(|e| format!("Failed to read file: {}", e))
    }

    pub fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        debug!("✍️ Writing file: {}", path);
        
        fs::write(path, content)
            .map_err(|e| format!("Failed to write file: {}", e))
    }

    pub fn delete_file(&self, path: &str) -> Result<(), String> {
        debug!("🗑️ Deleting file: {}", path);
        
        if Path::new(path).is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
        .map_err(|e| format!("Failed to delete file: {}", e))
    }
}