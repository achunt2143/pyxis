//! Filesystem Service
//! 
//! Provides file and directory operations.
//! All paths should be absolute or relative to the user's home directory.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use serde_json::{json, Value};
use anyhow::{Result, anyhow};

pub struct FilesystemService;

impl FilesystemService {
    /// Read directory contents and return file list
    pub fn read_directory(path: &str) -> Result<Value> {
        let path = PathBuf::from(expand_path(path)?);

        if !path.exists() {
            return Err(anyhow!("Path does not exist: {}", path.display()));
        }

        if !path.is_dir() {
            return Err(anyhow!("Path is not a directory: {}", path.display()));
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            let file_name = entry.file_name();

            entries.push(json!({
                "name": file_name.to_string_lossy(),
                "path": entry.path().display().to_string(),
                "isFile": metadata.is_file(),
                "isDirectory": metadata.is_dir(),
                "isSymlink": metadata.is_symlink(),
                "size": metadata.len(),
                "modified": metadata.modified()?
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs(),
                "created": metadata.created()?
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs(),
                "permissions": format!("{:o}", metadata.permissions().mode())
            }));
        }

        Ok(json!({
            "entries": entries,
            "path": path.display().to_string(),
            "count": entries.len()
        }))
    }

    /// Read file content
    pub fn read_file(path: &str, encoding: &str) -> Result<Value> {
        let path = PathBuf::from(expand_path(path)?);

        if !path.exists() {
            return Err(anyhow!("File does not exist: {}", path.display()));
        }

        if !path.is_file() {
            return Err(anyhow!("Path is not a file: {}", path.display()));
        }

        let content = fs::read_to_string(&path)?;
        let size = content.len();

        Ok(json!({
            "content": content,
            "size": size,
            "encoding": encoding,
            "path": path.display().to_string()
        }))
    }

    /// Write content to file (creates or truncates)
    pub fn write_file(path: &str, content: &str, encoding: &str) -> Result<Value> {
        let path = PathBuf::from(expand_path(path)?);

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }

        fs::write(&path, content)?;
        let bytes_written = content.len();

        Ok(json!({
            "bytesWritten": bytes_written,
            "path": path.display().to_string(),
            "encoding": encoding,
            "success": true
        }))
    }

    /// Append content to file
    pub fn append_file(path: &str, content: &str, encoding: &str) -> Result<Value> {
        let path = PathBuf::from(expand_path(path)?);

        if !path.exists() {
            return Err(anyhow!("File does not exist: {}", path.display()));
        }

        let current = fs::read_to_string(&path)?;
        let new_content = format!("{}{}", current, content);
        fs::write(&path, &new_content)?;

        Ok(json!({
            "bytesWritten": content.len(),
            "path": path.display().to_string(),
            "encoding": encoding,
            "success": true
        }))
    }

    /// Delete a file
    pub fn delete_file(path: &str) -> Result<Value> {
        let path = PathBuf::from(expand_path(path)?);

        if !path.exists() {
            return Err(anyhow!("File does not exist: {}", path.display()));
        }

        fs::remove_file(&path)?;

        Ok(json!({
            "success": true,
            "path": path.display().to_string()
        }))
    }

    /// Delete a directory
    pub fn delete_directory(path: &str, recursive: bool) -> Result<Value> {
        let path = PathBuf::from(expand_path(path)?);

        if !path.exists() {
            return Err(anyhow!("Directory does not exist: {}", path.display()));
        }

        if !path.is_dir() {
            return Err(anyhow!("Path is not a directory: {}", path.display()));
        }

        if recursive {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_dir(&path)?;
        }

        Ok(json!({
            "success": true,
            "path": path.display().to_string()
        }))
    }

    /// Create a directory
    pub fn create_directory(path: &str, mode: Option<u32>) -> Result<Value> {
        let path = PathBuf::from(expand_path(path)?);

        if path.exists() {
            return Err(anyhow!("Path already exists: {}", path.display()));
        }

        fs::create_dir_all(&path)?;

        // Set permissions if specified
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(mode);
            fs::set_permissions(&path, perms)?;
        }

        Ok(json!({
            "success": true,
            "path": path.display().to_string()
        }))
    }

    /// Check if path exists
    pub fn exists(path: &str) -> Result<Value> {
        let path = PathBuf::from(expand_path(path)?);

        Ok(json!({
            "exists": path.exists(),
            "path": path.display().to_string()
        }))
    }

    /// Get file/directory statistics
    pub fn stat(path: &str) -> Result<Value> {
        let path = PathBuf::from(expand_path(path)?);

        if !path.exists() {
            return Err(anyhow!("Path does not exist: {}", path.display()));
        }

        let metadata = fs::metadata(&path)?;
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;

        Ok(json!({
            "path": path.display().to_string(),
            "size": metadata.len(),
            "isFile": metadata.is_file(),
            "isDirectory": metadata.is_dir(),
            "isSymlink": metadata.is_symlink(),
            "isBlockDevice": metadata.dev(),
            "isCharDevice": metadata.is_symlink(),
            "isFifo": metadata.is_symlink(),
            "isSocket": metadata.is_symlink(),
            "permissions": format!("{:o}", metadata.permissions().mode()),
            "owner": metadata.uid(),
            "group": metadata.gid(),
            "accessed": metadata.atime(),
            "modified": metadata.mtime(),
            "changed": metadata.ctime()
        }))
    }

    /// Copy file or directory
    pub fn copy(source: &str, destination: &str) -> Result<Value> {
        let source = PathBuf::from(expand_path(source)?);
        let dest = PathBuf::from(expand_path(destination)?);

        if !source.exists() {
            return Err(anyhow!("Source does not exist: {}", source.display()));
        }

        if source.is_file() {
            fs::copy(&source, &dest)?;
        } else {
            copy_dir_all(&source, &dest)?;
        }

        Ok(json!({
            "success": true,
            "source": source.display().to_string(),
            "destination": dest.display().to_string()
        }))
    }

    /// Move/rename file or directory
    pub fn move_file(source: &str, destination: &str) -> Result<Value> {
        let source = PathBuf::from(expand_path(source)?);
        let dest = PathBuf::from(expand_path(destination)?);

        if !source.exists() {
            return Err(anyhow!("Source does not exist: {}", source.display()));
        }

        fs::rename(&source, &dest)?;

        Ok(json!({
            "success": true,
            "source": source.display().to_string(),
            "destination": dest.display().to_string()
        }))
    }

    /// Change file permissions
    pub fn chmod(path: &str, mode: u32) -> Result<Value> {
        let path = PathBuf::from(expand_path(path)?);

        if !path.exists() {
            return Err(anyhow!("Path does not exist: {}", path.display()));
        }

        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(mode);
        fs::set_permissions(&path, perms)?;

        Ok(json!({
            "success": true,
            "path": path.display().to_string(),
            "mode": format!("{:o}", mode)
        }))
    }
}

/// Expand ~ to home directory
fn expand_path(path: &str) -> Result<String> {
    if path.starts_with('~') {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_string());
        Ok(path.replacen('~', &home, 1))
    } else {
        Ok(path.to_string())
    }
}

/// Recursively copy directory
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(&dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let file_name = entry.file_name();
        let src_path = src.join(&file_name);
        let dst_path = dst.join(&file_name);

        if ty.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "unix")]
trait MetadataExt {
    fn mode(&self) -> u32;
}

#[cfg(target_os = "unix")]
impl MetadataExt for std::fs::Metadata {
    fn mode(&self) -> u32 {
        use std::os::unix::fs::MetadataExt as UnixMetadataExt;
        UnixMetadataExt::mode(self)
    }
}
