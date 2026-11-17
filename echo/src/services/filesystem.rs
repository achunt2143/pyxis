//! Filesystem Service
//!
//! Provides file and directory operations, in two halves.
//!
//! A call that carries a `scope` is confined to one directory tree and runs
//! as geckouser: see `scope.rs`. Every call from an app carries one, because
//! the bridge stamps it.
//!
//! A call without one is the unconfined service below, run as root, with
//! paths absolute or relative to the daemon's `$HOME` (which is `/` on the
//! device). Only a client holding the Echo socket can make such a call.

pub mod scope;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use serde_json::{json, Value};
use anyhow::{Result, anyhow};

/// Every method of `org.geckoOS.filesystem`, for daemon.rs to register.
pub const METHODS: &[&str] = &[
    "readDirectory",
    "readFile",
    "writeFile",
    "appendFile",
    "deleteFile",
    "deleteDirectory",
    "createDirectory",
    "exists",
    "stat",
    "copy",
    "move",
    "chmod",
];

pub struct FilesystemService;

impl FilesystemService {
    /// Answers one call. With a `scope`, the call is confined to that scope
    /// and runs as geckouser (scope.rs); without one, it is the unconfined
    /// service below. A `scope` that is present but not understood is
    /// refused, never taken for no scope at all.
    pub async fn call(method: &str, body: &Value) -> Result<Value> {
        if body.get("scope").is_some() {
            scope::call(scope::Scope::from_body(body)?, method, body).await
        } else {
            Self::call_unscoped(method, body)
        }
    }

    /// The unconfined service, with the defaults its handlers in daemon.rs
    /// have always had.
    fn call_unscoped(method: &str, body: &Value) -> Result<Value> {
        let encoding = field(body, "encoding", "utf-8");
        let mode = body.get("mode").and_then(Value::as_u64);
        match method {
            "readDirectory" => Self::read_directory(field(body, "path", "/")),
            "readFile" => Self::read_file(
                field(body, "path", "/"),
                encoding,
                body.get("maxBytes").and_then(Value::as_u64),
            ),
            "writeFile" => {
                Self::write_file(field(body, "path", ""), field(body, "content", ""), encoding)
            }
            "appendFile" => {
                Self::append_file(field(body, "path", ""), field(body, "content", ""), encoding)
            }
            "deleteFile" => Self::delete_file(field(body, "path", "")),
            "deleteDirectory" => Self::delete_directory(
                field(body, "path", ""),
                body.get("recursive").and_then(Value::as_bool).unwrap_or(false),
            ),
            "createDirectory" => {
                Self::create_directory(field(body, "path", ""), mode.map(|m| m as u32))
            }
            "exists" => Self::exists(field(body, "path", "")),
            "stat" => Self::stat(field(body, "path", "")),
            "copy" => Self::copy(field(body, "source", ""), field(body, "destination", "")),
            "move" => Self::move_file(field(body, "source", ""), field(body, "destination", "")),
            "chmod" => Self::chmod(field(body, "path", ""), mode.unwrap_or(0o755) as u32),
            other => Err(anyhow!("org.geckoOS.filesystem has no method {other:?}")),
        }
    }

    /// Read directory contents and return file list
    pub fn read_directory(path: &str) -> Result<Value> {
        let path = PathBuf::from(expand_path(path)?);

        if !path.exists() {
            return Err(anyhow!("Path does not exist: {}", path.display()));
        }

        if !path.is_dir() {
            return Err(anyhow!("Path is not a directory: {}", path.display()));
        }

        // Every per-entry failure below is non-fatal *by design*. This loop
        // used to propagate with `?` on `entry.metadata()`, `modified()` and
        // `created()`, which meant a single unreadable or timestamp-less entry
        // aborted the whole listing and the Files app rendered an empty
        // directory. That is not a hypothetical: `Metadata::created()` returns
        // `ErrorKind::Unsupported` on Linux whenever the filesystem doesn't
        // report `STATX_BTIME` -- true of tmpfs, overlayfs, procfs, sysfs, and
        // ext4 inodes created with a 256-byte inode size. Listing /tmp or
        // /proc was therefore a guaranteed blank screen.
        //
        // A directory listing should degrade per-entry, never wholesale: a
        // broken symlink or a file we can't stat is worth one missing row, not
        // an empty window.
        let mut entries = Vec::new();
        for entry in fs::read_dir(&path)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    tracing::debug!("readDirectory: skipping unreadable entry in {}: {}", path.display(), e);
                    continue;
                }
            };

            // `metadata()` follows symlinks, so a dangling link fails here.
            // Fall back to `symlink_metadata()` so the entry still lists (as a
            // symlink) instead of disappearing.
            let metadata = match entry.metadata().or_else(|_| entry.path().symlink_metadata()) {
                Ok(metadata) => metadata,
                Err(e) => {
                    tracing::debug!("readDirectory: skipping {:?} (no metadata): {}", entry.path(), e);
                    continue;
                }
            };
            let file_name = entry.file_name();

            entries.push(json!({
                "name": file_name.to_string_lossy(),
                "path": entry.path().display().to_string(),
                "isFile": metadata.is_file(),
                "isDirectory": metadata.is_dir(),
                "isSymlink": metadata.is_symlink(),
                "size": metadata.len(),
                "modified": unix_secs(metadata.modified()),
                "created": unix_secs(metadata.created()),
                "permissions": format!("{:o}", metadata.permissions().mode())
            }));
        }

        Ok(json!({
            "entries": entries,
            "path": path.display().to_string(),
            "count": entries.len()
        }))
    }

    /// Read file content.
    ///
    /// `encoding` is honoured rather than merely echoed back:
    /// - `utf-8` (default) returns the text as-is, and errors with an
    ///   actionable message if the bytes aren't valid UTF-8.
    /// - `base64` returns the bytes base64-encoded, which is what makes
    ///   binary reads (image preview in Files, for one) possible at all.
    ///
    /// `max_bytes` caps the read. Everything here crosses the Echo bus as a
    /// single JSON message, so an unbounded read is a way to wedge the daemon
    /// with one `readFile` on a large file; the cap is checked against the
    /// file's real length *before* any of it is read into memory.
    pub fn read_file(path: &str, encoding: &str, max_bytes: Option<u64>) -> Result<Value> {
        let path = PathBuf::from(expand_path(path)?);

        if !path.exists() {
            return Err(anyhow!("File does not exist: {}", path.display()));
        }

        if !path.is_file() {
            return Err(anyhow!("Path is not a file: {}", path.display()));
        }

        let limit = max_bytes.unwrap_or(DEFAULT_MAX_READ_BYTES);
        let len = fs::metadata(&path)?.len();
        if len > limit {
            return Err(over_read_limit(len, limit, &path.display().to_string()));
        }

        let bytes = fs::read(&path)?;
        let size = bytes.len();
        let content = encode_content(bytes, encoding, &path.display().to_string())?;

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
        use std::os::unix::fs::FileTypeExt;
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;

        Ok(json!({
            "path": path.display().to_string(),
            "size": metadata.len(),
            "isFile": metadata.is_file(),
            "isDirectory": metadata.is_dir(),
            "isSymlink": metadata.is_symlink(),
            "isBlockDevice": metadata.file_type().is_block_device(),
            "isCharDevice": metadata.file_type().is_char_device(),
            "isFifo": metadata.file_type().is_fifo(),
            "isSocket": metadata.file_type().is_socket(),
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

/// Default cap for a single `readFile`, in bytes (8 MiB). Callers that
/// genuinely want more can pass `maxBytes`.
const DEFAULT_MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// A string field of a call's body, or `default` when it is missing.
fn field<'a>(body: &'a Value, key: &str, default: &'a str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or(default)
}

/// A read refused for its size, in the words both halves of the service use.
fn over_read_limit(len: u64, limit: u64, path: &str) -> anyhow::Error {
    anyhow!(
        "File is {} bytes, which exceeds the {} byte read limit for {}; \
         pass a larger maxBytes to override",
        len,
        limit,
        path
    )
}

/// A file's bytes as `encoding` asks for them: the text itself for utf-8,
/// which the bytes must then be, or base64.
fn encode_content(bytes: Vec<u8>, encoding: &str, path: &str) -> Result<String> {
    match encoding.to_ascii_lowercase().as_str() {
        "base64" => Ok(base64_encode(&bytes)),
        "utf-8" | "utf8" | "" => String::from_utf8(bytes).map_err(|_| {
            anyhow!(
                "{} is not valid UTF-8; request encoding \"base64\" to read it as binary",
                path
            )
        }),
        other => Err(anyhow!(
            "Unsupported encoding \"{}\"; expected \"utf-8\" or \"base64\"",
            other
        )),
    }
}

/// Seconds since the Unix epoch, or `None` if the platform didn't supply the
/// timestamp. Serializes to JSON `null`.
///
/// Kept infallible on purpose: callers use this to build directory listings,
/// where a missing birth time is an ordinary fact about the filesystem (see
/// `read_directory`) and not an error worth failing a whole listing over.
fn unix_secs(time: std::io::Result<std::time::SystemTime>) -> Option<u64> {
    time.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// Standard base64 (RFC 4648, with padding).
///
/// Hand-rolled rather than pulling in the `base64` crate for this one call
/// site, matching the same call made for sha256 in `nova.rs`. Unlike that one
/// this can't shell out -- it runs per `readFile`, on the bus thread.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18 & 0x3F) as usize] as char);
        out.push(ALPHABET[(triple >> 12 & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6 & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 §10 test vectors, which cover all three padding cases.
    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(input.as_bytes()), expected, "input {input:?}");
        }
    }

    #[test]
    fn base64_encodes_non_utf8_bytes() {
        // 0xFF is never valid UTF-8 -- the case that made image preview
        // impossible while readFile was read_to_string-only.
        assert_eq!(base64_encode(&[0xFF, 0xFE, 0x00]), "//4A");
    }

    /// `/proc` is the case that used to blank the Files app: procfs reports no
    /// `STATX_BTIME`, so `created()` fails for every entry in it.
    #[test]
    fn read_directory_lists_procfs_despite_missing_birth_times() {
        let result = FilesystemService::read_directory("/proc")
            .expect("listing /proc must not fail just because created() is unsupported");

        let entries = result["entries"].as_array().expect("entries array");
        assert!(!entries.is_empty(), "/proc should list entries, got none");

        // The whole point: the field is present as null rather than aborting.
        let missing_birth_time = entries.iter().any(|e| e["created"].is_null());
        assert!(
            missing_birth_time,
            "expected at least one /proc entry with a null `created`"
        );
    }

    #[test]
    fn read_directory_reports_a_count_matching_its_entries() {
        let result = FilesystemService::read_directory("/proc").expect("listing /proc");
        let entries = result["entries"].as_array().expect("entries array");

        assert_eq!(result["count"].as_u64(), Some(entries.len() as u64));
    }

    #[test]
    fn read_file_honours_the_requested_encoding() {
        let path = std::env::temp_dir().join("pyxis-fs-encoding-test.bin");
        std::fs::write(&path, [0xFF, 0xFE, 0x00]).expect("write fixture");
        let path_str = path.to_str().expect("utf-8 path");

        let encoded = FilesystemService::read_file(path_str, "base64", None).expect("base64 read");
        assert_eq!(encoded["content"].as_str(), Some("//4A"));
        assert_eq!(encoded["size"].as_u64(), Some(3));

        // The same bytes as utf-8 must fail loudly, and say what to do instead.
        let err = FilesystemService::read_file(path_str, "utf-8", None)
            .expect_err("invalid utf-8 must not read as text")
            .to_string();
        assert!(err.contains("base64"), "unhelpful error: {err}");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_file_rejects_a_file_over_the_byte_limit() {
        let path = std::env::temp_dir().join("pyxis-fs-maxbytes-test.txt");
        std::fs::write(&path, "0123456789").expect("write fixture");
        let path_str = path.to_str().expect("utf-8 path");

        let err = FilesystemService::read_file(path_str, "utf-8", Some(4))
            .expect_err("a file over maxBytes must be refused")
            .to_string();
        assert!(err.contains("exceeds"), "unexpected error: {err}");

        // ...and is read normally when the limit allows it.
        let ok = FilesystemService::read_file(path_str, "utf-8", Some(64)).expect("read under limit");
        assert_eq!(ok["content"].as_str(), Some("0123456789"));

        std::fs::remove_file(&path).ok();
    }

    /// The exact path the Files app's image preview depends on: a real binary
    /// file read as base64 and handed to an `<img src="data:...">`. The
    /// expected string is a 1x1 PNG encoded by an independent implementation
    /// (Python's `base64.b64encode`), so this pins the hand-rolled encoder to
    /// the standard rather than to itself.
    #[test]
    fn read_file_base64_round_trips_a_real_png() {
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xDA, 0x63, 0xFC, 0xCF, 0xC0, 0x50, 0x0F, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xA9,
            0x8C, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        const EXPECTED: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

        let path = std::env::temp_dir().join("pyxis-fs-png-test.png");
        std::fs::write(&path, PNG).expect("write fixture");

        let res = FilesystemService::read_file(path.to_str().unwrap(), "base64", None)
            .expect("base64 read of a PNG");
        assert_eq!(res["content"].as_str(), Some(EXPECTED));
        assert_eq!(res["size"].as_u64(), Some(PNG.len() as u64));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_file_rejects_an_unknown_encoding() {
        let path = std::env::temp_dir().join("pyxis-fs-unknown-encoding-test.txt");
        std::fs::write(&path, "hello").expect("write fixture");

        let err = FilesystemService::read_file(path.to_str().unwrap(), "latin-1", None)
            .expect_err("unknown encodings must be refused, not silently treated as utf-8")
            .to_string();
        assert!(err.contains("Unsupported encoding"), "unexpected error: {err}");

        std::fs::remove_file(&path).ok();
    }

    /// A call naming a scope the service doesn't understand is refused.
    /// Treating it as unscoped would hand a mangled app call root's view of
    /// the whole filesystem.
    #[tokio::test]
    async fn a_scope_that_is_not_understood_is_refused_not_ignored() {
        for scope in [json!(5), json!(null), json!("root"), json!("app:../etc")] {
            let err = FilesystemService::call("exists", &json!({ "path": "/", "scope": scope }))
                .await
                .expect_err("an unknown scope must be refused")
                .to_string();
            assert!(err.contains("scope") || err.contains("app id"), "{scope}: {err}");
        }
    }

    #[tokio::test]
    async fn a_call_without_a_scope_is_the_service_it_always_was() {
        let res = FilesystemService::call("exists", &json!({ "path": "/proc" }))
            .await
            .expect("an unscoped exists");
        assert_eq!(res["exists"], true);
    }
}
