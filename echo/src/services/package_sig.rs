//! Detached signatures for `.ipk` packages, checked by the installer before
//! opkg runs.
//!
//! opkg is built WITH_GPGME=OFF and its own `check_signature` covers only
//! feed indexes, never a local `.ipk`, while a package's maintainer scripts
//! run as root. So the installer verifies here.
//!
//! Format (the same keys Nova's optional manifest signatures use):
//! - Public keys are PEM files, `/etc/pyxis/keys/<keyId>.pub`.
//! - The signature is `<pkg>.sig`, the raw output of
//!   `openssl dgst -sha256 -sign <key>.pem -out <pkg>.sig <pkg>`.
//! - The key id is read from the optional sibling text file
//!   `<pkg>.sig.keyid` (first line). Without it every key in the directory
//!   is tried and the first that verifies wins. Either way the id of the key
//!   that verified is returned.
//!
//! Callers must verify a root-owned private copy, never the user-writable
//! original (see [`PrivateCopy`]), or the package could change between the
//! check and opkg reading it.
//!
//! Environment overrides (tests): `PYXIS_ALLOW_UNSIGNED_MARKER` (marker path),
//! `PYXIS_PACKAGE_KEYS_DIR` (keys directory).

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const DEFAULT_MARKER: &str = "/etc/pyxis/allow-unsigned-packages";
const DEFAULT_KEYS_DIR: &str = "/etc/pyxis/keys";

/// True unless the dev-image marker file exists.
pub fn signatures_required() -> bool {
    let marker = std::env::var("PYXIS_ALLOW_UNSIGNED_MARKER")
        .unwrap_or_else(|_| DEFAULT_MARKER.to_string());
    !Path::new(&marker).exists()
}

pub fn keys_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("PYXIS_PACKAGE_KEYS_DIR").unwrap_or_else(|_| DEFAULT_KEYS_DIR.to_string()),
    )
}

/// `<pkg>.sig` beside the package.
pub fn sig_path_for(pkg: &Path) -> PathBuf {
    let mut s = pkg.as_os_str().to_owned();
    s.push(".sig");
    PathBuf::from(s)
}

fn keyid_path_for(sig: &Path) -> PathBuf {
    let mut s = sig.as_os_str().to_owned();
    s.push(".keyid");
    PathBuf::from(s)
}

fn valid_key_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && !id.contains("..")
        && !id.contains('/')
        && !id.contains('\0')
}

fn openssl_verify(pkg: &Path, sig: &Path, key: &Path) -> bool {
    std::process::Command::new("openssl")
        .args(["dgst", "-sha256", "-verify"])
        .arg(key)
        .arg("-signature")
        .arg(sig)
        .arg(pkg)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Verify `pkg` against detached signature `sig`; returns the verifying key id.
pub fn verify_detached(pkg: &Path, sig: &Path, keys_dir: &Path) -> Result<String> {
    let keyid_file = keyid_path_for(sig);
    let candidates: Vec<String> = if keyid_file.is_file() {
        let raw = fs::read_to_string(&keyid_file).context("read key id file")?;
        let id = raw.lines().next().unwrap_or("").trim().to_string();
        if !valid_key_id(&id) {
            return Err(anyhow!("invalid key id '{}' in {}", id, keyid_file.display()));
        }
        vec![id]
    } else {
        let mut ids: Vec<String> = fs::read_dir(keys_dir)
            .with_context(|| format!("read keys dir {}", keys_dir.display()))?
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) != Some("pub") {
                    return None;
                }
                p.file_stem().and_then(|s| s.to_str()).map(str::to_string)
            })
            .collect();
        ids.sort();
        ids
    };
    if candidates.is_empty() {
        return Err(anyhow!("no trusted keys in {}", keys_dir.display()));
    }
    for id in candidates {
        let key = keys_dir.join(format!("{id}.pub"));
        if key.is_file() && openssl_verify(pkg, sig, &key) {
            return Ok(id);
        }
    }
    Err(anyhow!("signature does not verify against any trusted key"))
}

/// Advisory status of a package in place: `true`, `false` (no signature) or
/// `"bad"`. Install never relies on this; it verifies its private copy.
pub fn status(pkg: &Path) -> Value {
    let sig = sig_path_for(pkg);
    if !sig.is_file() {
        return json!(false);
    }
    match verify_detached(pkg, &sig, &keys_dir()) {
        Ok(_) => json!(true),
        Err(_) => json!("bad"),
    }
}

/// A root-owned 0700 directory holding private copies of a package and its
/// signature. Removed on drop.
pub struct PrivateCopy {
    dir: PathBuf,
    pub pkg: PathBuf,
    pub sig: Option<PathBuf>,
}

impl Drop for PrivateCopy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn copy_regular(src: &Path, dst: &Path) -> Result<()> {
    let mut input = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(src)
        .with_context(|| format!("open {}", src.display()))?;
    let meta = input.metadata()?;
    if !meta.is_file() {
        return Err(anyhow!("{} is not a regular file", src.display()));
    }
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dst)
        .with_context(|| format!("create {}", dst.display()))?;
    std::io::copy(&mut input, &mut out)?;
    out.flush()?;
    out.sync_all()?;
    Ok(())
}

impl PrivateCopy {
    /// Copy `pkg` (and `<pkg>.sig` and `.sig.keyid` when present) into a fresh
    /// 0700 directory under `base`. The directory is made with a random name
    /// and mkdir (fails if it exists), so nothing can be pre-planted in it.
    pub fn make(pkg: &Path, base: &Path) -> Result<PrivateCopy> {
        let dir = base.join(format!("pyxis-pkg-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&dir).context("create private dir")?;
        // From here the guard cleans up on any error.
        let mut copy = PrivateCopy { dir: dir.clone(), pkg: dir.join("package.ipk"), sig: None };
        let me = unsafe { libc::geteuid() };
        let dm = fs::metadata(&dir)?;
        if dm.uid() != me || dm.mode() & 0o077 != 0 {
            return Err(anyhow!("private dir {} is not private", dir.display()));
        }
        copy_regular(pkg, &copy.pkg)?;
        let sig = sig_path_for(pkg);
        if fs::symlink_metadata(&sig).is_ok() {
            let dst = dir.join("package.ipk.sig");
            copy_regular(&sig, &dst)?;
            let kid = keyid_path_for(&sig);
            if fs::symlink_metadata(&kid).is_ok() {
                copy_regular(&kid, &keyid_path_for(&dst))?;
            }
            copy.sig = Some(dst);
        }
        Ok(copy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::Mutex;

    static ENV: Mutex<()> = Mutex::new(());

    fn have_openssl() -> bool {
        Command::new("openssl").arg("version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    struct Fixture {
        dir: tempfile::TempDir,
        keys: PathBuf,
        pkg: PathBuf,
    }

    fn fixture() -> Option<Fixture> {
        if !have_openssl() {
            eprintln!("skipping: openssl not installed");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("keys");
        fs::create_dir(&keys).unwrap();
        let pem = dir.path().join("k.pem");
        assert!(Command::new("openssl").args(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out"])
            .arg(&pem).status().unwrap().success());
        assert!(Command::new("openssl").args(["pkey", "-pubout", "-in"]).arg(&pem)
            .arg("-out").arg(keys.join("pub1.pub")).status().unwrap().success());
        let pkg = dir.path().join("a.ipk");
        fs::write(&pkg, b"package bytes").unwrap();
        assert!(Command::new("openssl").args(["dgst", "-sha256", "-sign"]).arg(&pem)
            .arg("-out").arg(sig_path_for(&pkg)).arg(&pkg).status().unwrap().success());
        Some(Fixture { dir, keys, pkg })
    }

    #[test]
    fn marker_controls_requirement() {
        let _g = ENV.lock().unwrap();
        let d = tempfile::tempdir().unwrap();
        let marker = d.path().join("m");
        std::env::set_var("PYXIS_ALLOW_UNSIGNED_MARKER", &marker);
        assert!(signatures_required());
        fs::write(&marker, "").unwrap();
        assert!(!signatures_required());
        std::env::remove_var("PYXIS_ALLOW_UNSIGNED_MARKER");
    }

    #[test]
    fn good_signature_verifies_with_and_without_keyid() {
        let Some(f) = fixture() else { return };
        let sig = sig_path_for(&f.pkg);
        assert_eq!(verify_detached(&f.pkg, &sig, &f.keys).unwrap(), "pub1");
        fs::write(keyid_path_for(&sig), "pub1\n").unwrap();
        assert_eq!(verify_detached(&f.pkg, &sig, &f.keys).unwrap(), "pub1");
        fs::write(keyid_path_for(&sig), "../x\n").unwrap();
        assert!(verify_detached(&f.pkg, &sig, &f.keys).is_err());
    }

    #[test]
    fn tampered_package_is_refused() {
        let Some(f) = fixture() else { return };
        fs::write(&f.pkg, b"package bytes, changed").unwrap();
        assert!(verify_detached(&f.pkg, &sig_path_for(&f.pkg), &f.keys).is_err());
    }

    #[test]
    fn missing_signature_gives_no_copy_sig() {
        let Some(f) = fixture() else { return };
        fs::remove_file(sig_path_for(&f.pkg)).unwrap();
        let c = PrivateCopy::make(&f.pkg, f.dir.path()).unwrap();
        assert!(c.sig.is_none());
    }

    #[test]
    fn the_copy_is_what_gets_verified() {
        let Some(f) = fixture() else { return };
        let c = PrivateCopy::make(&f.pkg, f.dir.path()).unwrap();
        // The attacker rewrites the original after the copy was made.
        fs::write(&f.pkg, b"evil").unwrap();
        let sig = c.sig.clone().unwrap();
        assert_eq!(verify_detached(&c.pkg, &sig, &f.keys).unwrap(), "pub1");
        assert!(verify_detached(&f.pkg, &sig, &f.keys).is_err());
        let dir = c.pkg.parent().unwrap().to_path_buf();
        drop(c);
        assert!(!dir.exists());
    }

    #[test]
    fn symlink_source_is_refused() {
        let Some(f) = fixture() else { return };
        let link = f.dir.path().join("l.ipk");
        std::os::unix::fs::symlink(&f.pkg, &link).unwrap();
        assert!(PrivateCopy::make(&link, f.dir.path()).is_err());
    }
}
