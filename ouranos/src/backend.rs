use smithay::backend::drm::{DrmDevice, DrmSurface};
use smithay::backend::libinput::LibinputSessionInterface;
use smithay::backend::session::{Session, SessionNotifier};
use anyhow::Result;
use std::path::PathBuf;

pub struct BackendState {
    pub drm_device: Option<DrmDevice<'static>>,
    pub session: Session,
    pub session_notifier: SessionNotifier,
}

impl BackendState {
    /// Initialize graphics backend (DRM + GBM)
    pub fn new() -> Result<Self> {
        tracing::info!("Initializing graphics backend");

        // Enumerate available DRM devices
        let device_path = find_primary_gpu()
            .ok_or_else(|| anyhow::anyhow!("No DRM device found"))?;

        tracing::info!("Found GPU at: {}", device_path.display());

        // Initialize session (for VT switching and device access)
        let (session, notifier) = Session::new(None)?;

        tracing::debug!("Session initialized");

        Ok(BackendState {
            drm_device: None,
            session,
            session_notifier: notifier,
        })
    }

    pub fn is_session_active(&self) -> bool {
        self.session.is_active()
    }
}

/// Find primary GPU device (first available DRM device)
fn find_primary_gpu() -> Option<PathBuf> {
    // Typical locations for DRM devices
    let paths = vec![
        "/dev/dri/card0",
        "/dev/dri/card1",
        "/dev/dri/renderD128",
        "/dev/dri/renderD129",
    ];

    paths.into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}
