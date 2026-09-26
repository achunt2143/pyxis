//! System sounds: `play` a sound by name.
//!
//! Every component that makes a system sound goes through here: Bloom (via
//! the SDK), the power service (the shutdown sound), the notification store
//! (the notification sound), and geckoOS-boot (the boot sound). So there is
//! one place that knows where the files live and how they get played.
//!
//! A sound is a file `<name>.<ext>` in /usr/share/sounds/pyxis, staged from
//! `system-sounds/` at the repo root (create-rootfs.sh,
//! stage_system_sounds). Swapping a sound needs no code change. Playback is
//! `pw-play`, PipeWire's own player, which reads anything libsndfile does.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Result};

pub const SOUND_IFACE: &str = "org.geckoOS.sound";
pub const SOUNDS_DIR: &str = "/usr/share/sounds/pyxis";

/// Tried in this order when a name has files in several formats.
const EXTENSIONS: &[&str] = &["wav", "ogg", "oga", "flac", "mp3"];

/// The session user's runtime dir, where PipeWire's socket is.
const RUNTIME_DIR: &str = "/run/user/1000";

pub struct SoundService {
    dir: PathBuf,
}

impl SoundService {
    /// The sounds the image ships.
    pub fn system() -> Self {
        Self::with_dir(SOUNDS_DIR)
    }

    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The file for `name`. A name is a plain word (`notification`,
    /// `low-battery`). No separators and no dots, because it becomes a path
    /// under the sounds directory and must not be able to leave it.
    pub fn resolve(&self, name: &str) -> Result<PathBuf> {
        let plain = !name.is_empty()
            && name.len() <= 64
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !plain {
            return Err(anyhow!("not a sound name: {name:?}"));
        }
        EXTENSIONS
            .iter()
            .map(|ext| self.dir.join(format!("{name}.{ext}")))
            .find(|path| path.is_file())
            .ok_or_else(|| anyhow!("no sound named {name:?} in {}", self.dir.display()))
    }

    /// Every sound available, by name.
    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let ext = path.extension()?.to_str()?;
                EXTENSIONS.contains(&ext).then(|| path.file_stem()?.to_str().map(str::to_owned))?
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Start playing `name`, and return as soon as the player is running.
    /// The player is reaped in the background.
    pub async fn play(&self, name: &str) -> Result<u32> {
        let path = self.resolve(name)?;
        let mut child = player(&path).spawn().map_err(|e| anyhow!("pw-play: {e}"))?;
        let pid = child.id().unwrap_or(0);
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(pid)
    }

    /// Play `name` and wait for it to end, for at most `limit`. This is for
    /// the shutdown sound, which has to finish before the power goes. A sound
    /// that overruns is cut off, not waited for.
    pub async fn play_and_wait(&self, name: &str, limit: Duration) -> Result<()> {
        let path = self.resolve(name)?;
        let mut child = player(&path).spawn().map_err(|e| anyhow!("pw-play: {e}"))?;
        match tokio::time::timeout(limit, child.wait()).await {
            Ok(status) => {
                status?;
                Ok(())
            }
            Err(_) => {
                let _ = child.kill().await;
                Ok(())
            }
        }
    }
}

/// `program`, run as the session user when we are root (echo-daemon is),
/// with the session's runtime dir, where PipeWire's socket lives. PipeWire
/// runs as the session user, and nothing that talks to it (a decoder, a
/// volume query) has any business running as root.
pub(crate) fn as_session_user(program: &str) -> tokio::process::Command {
    // SAFETY: geteuid(2) takes no arguments and cannot fail.
    let root = unsafe { libc::geteuid() } == 0;
    let mut cmd = if root {
        let mut cmd = tokio::process::Command::new("setpriv");
        cmd.args(["--reuid=1000", "--regid=1000", "--clear-groups", program]);
        cmd
    } else {
        tokio::process::Command::new(program)
    };
    cmd.env("XDG_RUNTIME_DIR", RUNTIME_DIR).stdin(Stdio::null());
    cmd
}

/// `pw-play <path>`, as the session user.
fn player(path: &Path) -> tokio::process::Command {
    let mut cmd = as_session_user("pw-play");
    cmd.arg(path).stdout(Stdio::null()).stderr(Stdio::null());
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sounds(files: &[&str]) -> (tempfile::TempDir, SoundService) {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            std::fs::write(dir.path().join(file), b"x").unwrap();
        }
        let service = SoundService::with_dir(dir.path());
        (dir, service)
    }

    #[test]
    fn resolves_a_name_to_its_file_in_format_order() {
        let (_dir, service) = sounds(&["boot.mp3", "boot.wav", "error.mp3"]);
        assert!(service.resolve("boot").unwrap().ends_with("boot.wav"));
        assert!(service.resolve("error").unwrap().ends_with("error.mp3"));
    }

    #[test]
    fn a_name_cannot_leave_the_sounds_directory() {
        let (_dir, service) = sounds(&["boot.wav"]);
        for bad in ["../boot", "/etc/passwd", "boot.wav", "", "a/b", ".."] {
            assert!(service.resolve(bad).is_err(), "{bad:?} resolved");
        }
    }

    #[test]
    fn missing_sounds_say_so() {
        let (_dir, service) = sounds(&[]);
        let err = service.resolve("boot").unwrap_err().to_string();
        assert!(err.contains("no sound named"), "{err}");
    }

    #[test]
    fn lists_each_name_once_and_ignores_other_files() {
        let (_dir, service) = sounds(&["boot.mp3", "boot.wav", "notification.wav", "README.txt"]);
        assert_eq!(service.list(), vec!["boot", "notification"]);
    }
}
