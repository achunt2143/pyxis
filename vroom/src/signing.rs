//! Detached `.ipk` signatures (the scheme `echo/src/services/package_sig.rs`
//! verifies on the device).
//!
//! - `<pkg>.ipk.sig` is the raw output of
//!   `openssl dgst -sha256 -sign <key.pem> -out <pkg>.ipk.sig <pkg>.ipk`.
//! - `<pkg>.ipk.sig.keyid`, optional, holds the key id on one line. The device
//!   verifies with `/etc/pyxis/keys/<keyid>.pub`, or tries every key there
//!   when the file is absent.
//! - `pyxis keygen <id>` makes `<id>.pem` (private, 0600) and `<id>.pub`
//!   (public, to be copied to `/etc/pyxis/keys/<id>.pub` on each device).
//!
//! The package command signs the `.ipk` it just emitted when `pyxis.json`
//! has a `signing` object or `PYXIS_SIGNING_KEY` is set.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Project;
use crate::error::{PyxisError, Result};

pub const ENV_KEY: &str = "PYXIS_SIGNING_KEY";
pub const ENV_KEY_ID: &str = "PYXIS_SIGNING_KEY_ID";

/// Resolved signing settings for one package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningKey {
    pub key: PathBuf,
    pub key_id: Option<String>,
}

/// `<pkg>.sig` beside the package.
pub fn sig_path(pkg: &Path) -> PathBuf {
    append_suffix(pkg, ".sig")
}

/// `<pkg>.sig.keyid` beside the package.
pub fn keyid_path(pkg: &Path) -> PathBuf {
    append_suffix(pkg, ".sig.keyid")
}

fn append_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Same rules the device applies to a key id (package_sig.rs `valid_key_id`).
pub fn valid_key_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && !id.contains("..")
        && !id.contains('/')
        && !id.contains('\0')
        && !id.contains('\n')
        && !id.trim().is_empty()
}

fn require_openssl() -> Result<()> {
    which::which("openssl")
        .map(|_| ())
        .map_err(|_| PyxisError::Package("package signing needs `openssl` on PATH".into()))
}

/// Work out the signing settings for `project`. Environment values win over
/// `pyxis.json`. `env` is injected so tests need not touch the process
/// environment. Returns `Ok(None)` when signing is not configured.
pub fn resolve_with(
    project: &Project,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<SigningKey>> {
    let configured = project.config.signing.as_ref();
    let env_key = env(ENV_KEY).filter(|v| !v.is_empty());
    let env_key_id = env(ENV_KEY_ID).filter(|v| !v.is_empty());

    let key_str = match (env_key, configured) {
        (Some(k), _) => k,
        (None, Some(c)) => c.key.clone(),
        (None, None) => {
            if env_key_id.is_some() {
                return Err(PyxisError::Config(format!(
                    "{ENV_KEY_ID} is set but no signing key is configured \
                     (set {ENV_KEY} or a `signing` object in pyxis.json)"
                )));
            }
            return Ok(None);
        }
    };
    if key_str.trim().is_empty() {
        return Err(PyxisError::Config("signing key path is empty".into()));
    }
    let key_id = env_key_id.or_else(|| configured.and_then(|c| c.key_id.clone()));
    if let Some(id) = &key_id {
        if !valid_key_id(id) {
            return Err(PyxisError::Config(format!(
                "signing key id `{id}` must be a plain name (no '/', '..' or newlines)"
            )));
        }
    }
    let key = Path::new(&key_str);
    let key = if key.is_absolute() {
        key.to_path_buf()
    } else {
        project.root.join(key)
    };
    Ok(Some(SigningKey { key, key_id }))
}

/// [`resolve_with`] against the real process environment.
pub fn resolve(project: &Project) -> Result<Option<SigningKey>> {
    resolve_with(project, &|name| std::env::var(name).ok())
}

/// Sign `pkg` with the PEM private key `key`, writing `<pkg>.sig` and, when
/// `key_id` is given, `<pkg>.sig.keyid`. Without a key id any stale
/// `.sig.keyid` is removed, since a leftover id would make the device check
/// only that (possibly retired) key.
pub fn sign_package(pkg: &Path, key: &Path, key_id: Option<&str>) -> Result<PathBuf> {
    if !pkg.is_file() {
        return Err(PyxisError::Package(format!("cannot sign {}: no such file", pkg.display())));
    }
    if !key.is_file() {
        return Err(PyxisError::Package(format!(
            "signing key not found: {} (create one with `pyxis keygen <id>`)",
            key.display()
        )));
    }
    if let Some(id) = key_id {
        if !valid_key_id(id) {
            return Err(PyxisError::Package(format!("invalid key id `{id}`")));
        }
    }
    require_openssl()?;

    let sig = sig_path(pkg);
    let out = Command::new("openssl")
        .args(["dgst", "-sha256", "-sign"])
        .arg(key)
        .arg("-out")
        .arg(&sig)
        .arg(pkg)
        .output()
        .map_err(|e| PyxisError::Package(format!("cannot run openssl: {e}")))?;
    if !out.status.success() {
        let _ = fs::remove_file(&sig);
        return Err(PyxisError::Package(format!(
            "openssl could not sign {}: {}",
            pkg.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    let keyid = keyid_path(pkg);
    match key_id {
        Some(id) => fs::write(&keyid, format!("{id}\n"))
            .map_err(|e| PyxisError::Package(format!("writing {}: {e}", keyid.display())))?,
        None => {
            let _ = fs::remove_file(&keyid);
        }
    }
    Ok(sig)
}

/// Remove a package's signature sidecars. A stale signature beside a rebuilt
/// package would be refused by the device, so unsigned builds clear them.
pub fn remove_sidecars(pkg: &Path) {
    let _ = fs::remove_file(sig_path(pkg));
    let _ = fs::remove_file(keyid_path(pkg));
}

/// `pyxis keygen <id>`: write `<id>.pem` (private, 0600) and `<id>.pub` into
/// `dir`. Refuses to overwrite existing files. Returns `(pem, pub)`.
pub fn keygen(id: &str, dir: &Path) -> Result<(PathBuf, PathBuf)> {
    if !valid_key_id(id) {
        return Err(PyxisError::Config(format!(
            "key id `{id}` must be a plain name (no '/', '..' or newlines)"
        )));
    }
    require_openssl()?;
    let pem = dir.join(format!("{id}.pem"));
    let pubkey = dir.join(format!("{id}.pub"));
    for p in [&pem, &pubkey] {
        if p.exists() {
            return Err(PyxisError::Config(format!(
                "{} already exists; refusing to overwrite a signing key",
                p.display()
            )));
        }
    }
    fs::create_dir_all(dir)
        .map_err(|e| PyxisError::Config(format!("creating {}: {e}", dir.display())))?;

    // Create the private file 0600 before any key material is written, so
    // openssl's output (which keeps the existing mode) is never readable by
    // others. create_new also refuses to follow a planted link.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pem)
        .map_err(|e| PyxisError::Config(format!("creating {}: {e}", pem.display())))?;

    let fail = |msg: String| {
        let _ = fs::remove_file(&pem);
        let _ = fs::remove_file(&pubkey);
        PyxisError::Config(msg)
    };

    let gen = Command::new("openssl")
        .args(["genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out"])
        .arg(&pem)
        .output()
        .map_err(|e| fail(format!("cannot run openssl: {e}")))?;
    if !gen.status.success() {
        return Err(fail(format!(
            "openssl genpkey failed: {}",
            String::from_utf8_lossy(&gen.stderr).trim()
        )));
    }

    let public = Command::new("openssl")
        .args(["pkey", "-pubout", "-in"])
        .arg(&pem)
        .arg("-out")
        .arg(&pubkey)
        .output()
        .map_err(|e| fail(format!("cannot run openssl: {e}")))?;
    if !public.status.success() {
        return Err(fail(format!(
            "openssl pkey -pubout failed: {}",
            String::from_utf8_lossy(&public.stderr).trim()
        )));
    }

    // Belt and braces: openssl truncates in place, but state the mode anyway.
    fs::set_permissions(&pem, fs::Permissions::from_mode(0o600))
        .map_err(|e| fail(format!("chmod 0600 {}: {e}", pem.display())))?;
    Ok((pem, pubkey))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PackageConfig, ProjectConfig, Runtime, SigningConfig};
    use std::process::Stdio;

    fn openssl_available() -> bool {
        which::which("openssl").is_ok()
    }

    /// A fresh directory under the system temp dir, unique per test.
    fn temp_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "vroom-sign-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn project_with(signing: Option<SigningConfig>) -> Project {
        Project {
            root: PathBuf::from("/work/app"),
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
                accent: None,
                category: None,
                sdk: None,
                pdk: None,
                permissions: None,
                services: None,
                signing,
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

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn unsigned_when_nothing_configured() {
        let p = project_with(None);
        assert_eq!(resolve_with(&p, &no_env).unwrap(), None);
    }

    #[test]
    fn config_key_resolves_against_project_root() {
        let p = project_with(Some(SigningConfig {
            key: "keys/dev.pem".into(),
            key_id: Some("dev".into()),
        }));
        let got = resolve_with(&p, &no_env).unwrap().unwrap();
        assert_eq!(got.key, PathBuf::from("/work/app/keys/dev.pem"));
        assert_eq!(got.key_id.as_deref(), Some("dev"));
    }

    #[test]
    fn env_overrides_config() {
        let p = project_with(Some(SigningConfig {
            key: "keys/dev.pem".into(),
            key_id: Some("dev".into()),
        }));
        let env = |n: &str| match n {
            ENV_KEY => Some("/secure/prod.pem".to_string()),
            ENV_KEY_ID => Some("prod".to_string()),
            _ => None,
        };
        let got = resolve_with(&p, &env).unwrap().unwrap();
        assert_eq!(got.key, PathBuf::from("/secure/prod.pem"));
        assert_eq!(got.key_id.as_deref(), Some("prod"));
    }

    #[test]
    fn env_key_alone_signs_without_key_id() {
        let p = project_with(None);
        let env = |n: &str| (n == ENV_KEY).then(|| "/secure/k.pem".to_string());
        let got = resolve_with(&p, &env).unwrap().unwrap();
        assert_eq!(got.key_id, None);
    }

    #[test]
    fn key_id_without_any_key_is_an_error() {
        let p = project_with(None);
        let env = |n: &str| (n == ENV_KEY_ID).then(|| "prod".to_string());
        assert!(matches!(resolve_with(&p, &env), Err(PyxisError::Config(_))));
    }

    #[test]
    fn bad_key_id_is_rejected() {
        let p = project_with(Some(SigningConfig {
            key: "k.pem".into(),
            key_id: Some("../evil".into()),
        }));
        assert!(matches!(resolve_with(&p, &no_env), Err(PyxisError::Config(_))));
    }

    #[test]
    fn key_id_rules_match_the_device() {
        assert!(valid_key_id("release-2026"));
        assert!(!valid_key_id(""));
        assert!(!valid_key_id("."));
        assert!(!valid_key_id("a/b"));
        assert!(!valid_key_id("a..b"));
        assert!(!valid_key_id("a\nb"));
    }

    #[test]
    fn schema_declares_signing() {
        let raw = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/pyxis.schema.json"),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let s = &v["properties"]["signing"];
        assert_eq!(s["type"], "object");
        assert_eq!(s["required"][0], "key");
        assert!(s["properties"]["keyId"].is_object());
    }

    #[test]
    fn keygen_sign_and_verify_round_trip() {
        if !openssl_available() {
            eprintln!("skipping keygen_sign_and_verify_round_trip: openssl not on PATH");
            return;
        }
        let dir = temp_dir("roundtrip");
        let (pem, pubkey) = keygen("test-key", &dir).unwrap();
        let mode = fs::metadata(&pem).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "private key must be 0600");
        assert!(pubkey.is_file());

        let pkg = dir.join("app_1.0.0_all.ipk");
        fs::write(&pkg, b"not really an ipk, but signable bytes").unwrap();
        let sig = sign_package(&pkg, &pem, Some("test-key")).unwrap();
        assert_eq!(fs::read_to_string(keyid_path(&pkg)).unwrap(), "test-key\n");

        let verify = |file: &Path| {
            Command::new("openssl")
                .args(["dgst", "-sha256", "-verify"])
                .arg(&pubkey)
                .arg("-signature")
                .arg(&sig)
                .arg(file)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        assert!(verify(&pkg), "signature must verify against the public key");

        fs::write(&pkg, b"tampered").unwrap();
        assert!(!verify(&pkg), "a changed package must not verify");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unsigned_rebuild_clears_stale_sidecars() {
        let dir = temp_dir("stale");
        let pkg = dir.join("a_1.0.0_all.ipk");
        fs::write(&pkg, b"x").unwrap();
        fs::write(sig_path(&pkg), b"old").unwrap();
        fs::write(keyid_path(&pkg), b"old\n").unwrap();
        remove_sidecars(&pkg);
        assert!(!sig_path(&pkg).exists());
        assert!(!keyid_path(&pkg).exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sign_without_key_id_removes_stale_keyid() {
        if !openssl_available() {
            eprintln!("skipping sign_without_key_id_removes_stale_keyid: openssl not on PATH");
            return;
        }
        let dir = temp_dir("nokeyid");
        let (pem, _pubkey) = keygen("k", &dir).unwrap();
        let pkg = dir.join("a_1.0.0_all.ipk");
        fs::write(&pkg, b"x").unwrap();
        fs::write(keyid_path(&pkg), b"old\n").unwrap();
        sign_package(&pkg, &pem, None).unwrap();
        assert!(sig_path(&pkg).is_file());
        assert!(!keyid_path(&pkg).exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn keygen_refuses_to_overwrite() {
        if !openssl_available() {
            eprintln!("skipping keygen_refuses_to_overwrite: openssl not on PATH");
            return;
        }
        let dir = temp_dir("overwrite");
        keygen("dup", &dir).unwrap();
        assert!(keygen("dup", &dir).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sign_reports_missing_key() {
        let dir = temp_dir("nokey");
        let pkg = dir.join("a.ipk");
        fs::write(&pkg, b"x").unwrap();
        let err = sign_package(&pkg, &dir.join("missing.pem"), None).unwrap_err();
        assert!(err.to_string().contains("signing key not found"));
        fs::remove_dir_all(&dir).ok();
    }
}
