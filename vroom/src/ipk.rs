//! `.ipk` emission (spec §16, §23).
//!
//! Prefers the system `opkg-build` when available (the spec's recommended
//! packager). Otherwise falls back to a built-in writer producing the same
//! layout: an outer gzipped tar holding `debian-binary`, `control.tar.gz`,
//! and `data.tar.gz`, with all entries owned root:root.

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::write::GzEncoder;
use flate2::Compression;

use crate::config::Project;
use crate::error::{PyxisError, Result};

pub fn build_ipk(project: &Project) -> Result<PathBuf> {
    let dist = project.dist_dir();
    fs::create_dir_all(&dist)
        .map_err(|e| PyxisError::Package(format!("creating dist dir: {e}")))?;
    let artifact = dist.join(project.artifact_name());

    if which::which("opkg-build").is_ok() {
        build_with_opkg_build(project, &dist, &artifact)?;
    } else {
        eprintln!("[pyxis] opkg-build not found, using built-in IPK writer");
        build_builtin(project, &artifact)?;
    }

    if !artifact.is_file() {
        return Err(PyxisError::Package(format!(
            "expected artifact {} was not produced",
            artifact.display()
        )));
    }
    Ok(artifact)
}

fn build_with_opkg_build(project: &Project, dist: &Path, artifact: &Path) -> Result<()> {
    let status = Command::new("opkg-build")
        .args(["-o", "root", "-g", "root"])
        .arg(project.stage_pkg_dir())
        .arg(dist)
        .status()
        .map_err(|e| PyxisError::Package(format!("cannot run opkg-build: {e}")))?;
    if !status.success() {
        return Err(PyxisError::Package(format!("opkg-build exited with {status}")));
    }
    // opkg-build names the file from the control fields, which we generate
    // from the same values as artifact_name(); double-check anyway.
    if !artifact.is_file() {
        return Err(PyxisError::Package(format!(
            "opkg-build did not produce {}",
            artifact.display()
        )));
    }
    Ok(())
}

fn build_builtin(project: &Project, artifact: &Path) -> Result<()> {
    let pkg_dir = project.stage_pkg_dir();
    let work = project.work_dir();

    let control_tar = work.join("control.tar.gz");
    write_tar_gz(&control_tar, &pkg_dir.join("CONTROL"))?;

    let data_tar = work.join("data.tar.gz");
    write_data_tar(&data_tar, &pkg_dir)?;

    let debian_binary = work.join("debian-binary");
    fs::write(&debian_binary, b"2.0\n")
        .map_err(|e| PyxisError::Package(format!("writing debian-binary: {e}")))?;

    let out = File::create(artifact)
        .map_err(|e| PyxisError::Package(format!("creating {}: {e}", artifact.display())))?;
    let enc = GzEncoder::new(out, Compression::default());
    let mut tar = tar::Builder::new(enc);
    for (path, name) in [
        (&debian_binary, "debian-binary"),
        (&control_tar, "control.tar.gz"),
        (&data_tar, "data.tar.gz"),
    ] {
        append_file_as_root(&mut tar, path, name, 0o644)?;
    }
    tar.into_inner()
        .and_then(|enc| enc.finish())
        .map_err(|e| PyxisError::Package(format!("finalizing ipk: {e}")))?
        .flush()
        .map_err(|e| PyxisError::Package(format!("flushing ipk: {e}")))?;
    Ok(())
}

/// Tar up `dir`'s contents (as `./...`) into a gzipped archive, root-owned.
fn write_tar_gz(out_path: &Path, dir: &Path) -> Result<()> {
    let out = File::create(out_path)
        .map_err(|e| PyxisError::Package(format!("creating {}: {e}", out_path.display())))?;
    let enc = GzEncoder::new(out, Compression::default());
    let mut tar = tar::Builder::new(enc);
    append_tree_as_root(&mut tar, dir, dir, &|_| true)?;
    tar.into_inner()
        .and_then(|enc| enc.finish())
        .map_err(|e| PyxisError::Package(format!("finalizing {}: {e}", out_path.display())))?;
    Ok(())
}

/// data.tar.gz: everything under the stage dir except CONTROL/.
fn write_data_tar(out_path: &Path, pkg_dir: &Path) -> Result<()> {
    let out = File::create(out_path)
        .map_err(|e| PyxisError::Package(format!("creating {}: {e}", out_path.display())))?;
    let enc = GzEncoder::new(out, Compression::default());
    let mut tar = tar::Builder::new(enc);
    let pkg_dir_owned = pkg_dir.to_path_buf();
    append_tree_as_root(&mut tar, pkg_dir, pkg_dir, &move |p: &Path| {
        p.strip_prefix(&pkg_dir_owned)
            .map(|rel| !rel.starts_with("CONTROL"))
            .unwrap_or(true)
    })?;
    tar.into_inner()
        .and_then(|enc| enc.finish())
        .map_err(|e| PyxisError::Package(format!("finalizing {}: {e}", out_path.display())))?;
    Ok(())
}

fn append_tree_as_root<W: Write>(
    tar: &mut tar::Builder<W>,
    dir: &Path,
    base: &Path,
    include: &dyn Fn(&Path) -> bool,
) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| PyxisError::Package(format!("reading {}: {e}", dir.display())))?
        .collect::<std::io::Result<_>>()
        .map_err(|e| PyxisError::Package(format!("reading {}: {e}", dir.display())))?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        if !include(&path) {
            continue;
        }
        let rel = path.strip_prefix(base).unwrap();
        let arc = format!("./{}", rel.display());
        let meta = entry
            .metadata()
            .map_err(|e| PyxisError::Package(format!("stat {}: {e}", path.display())))?;
        if meta.is_dir() {
            let mut header = root_header(&meta);
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            tar.append_data(&mut header, format!("{arc}/"), std::io::empty())
                .map_err(|e| PyxisError::Package(format!("adding dir {arc}: {e}")))?;
            append_tree_as_root(tar, &path, base, include)?;
        } else if meta.is_file() {
            append_file_as_root(tar, &path, &arc, meta.permissions().mode())?;
        }
        // Symlinks are not expected in staged trees; skip anything else.
    }
    Ok(())
}

fn append_file_as_root<W: Write>(
    tar: &mut tar::Builder<W>,
    path: &Path,
    arc_name: &str,
    mode: u32,
) -> Result<()> {
    let meta = fs::metadata(path)
        .map_err(|e| PyxisError::Package(format!("stat {}: {e}", path.display())))?;
    let mut header = root_header(&meta);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(meta.len());
    header.set_mode(mode & 0o7777);
    let file = File::open(path)
        .map_err(|e| PyxisError::Package(format!("opening {}: {e}", path.display())))?;
    tar.append_data(&mut header, arc_name, file)
        .map_err(|e| PyxisError::Package(format!("adding {arc_name}: {e}")))?;
    Ok(())
}

fn root_header(meta: &fs::Metadata) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_mode(meta.permissions().mode() & 0o7777);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
    );
    let _ = header.set_username("root");
    let _ = header.set_groupname("root");
    header
}
