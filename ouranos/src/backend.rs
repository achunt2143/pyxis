use anyhow::Result;
use smithay::backend::drm::{DrmDevice, DrmNode, DrmSurface, DrmDeviceFd};
use smithay::reexports::drm;
use smithay::reexports::drm::control::{crtc, connector, Mode};
use smithay::utils::DeviceFd;
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay, ffi};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{WinitEventLoop, WinitGraphicsBackend};
use smithay::backend::allocator::gbm::GbmDevice;
use std::ffi::CStr;
use std::fs::File;
use std::os::fd::AsFd;
use std::path::PathBuf;
use tracing::{info, error};

/// Backend type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    /// DRM/KMS backend for real hardware
    Drm,
    /// Winit backend for development/nested
    Winit,
    /// Headless backend for testing
    Headless,
}

/// Backend state
pub struct BackendState {
    pub backend_type: BackendType,
    pub winit_back: Option<WinitGraphicsBackend<GlesRenderer>>,
    pub renderer: Option<GlesRenderer>,
}

impl BackendState {
    /// Initialize DRM backend
    pub fn init_drm(device_path: PathBuf) -> Result<Self> {
        info!("Initializing DRM backend: {:?}", device_path);

        // Open DRM device node
        let drm_node = DrmNode::from_path(&device_path)?;
        let (drm_device, drm_surface) = Self::open_drm_device(drm_node)?;

        // Create GBM device from DRM device
        let gbm_device = GbmDevice::new(drm_device)?;
        
        // Create EGL display from GBM device
        let egl_display = unsafe { EGLDisplay::new(gbm_device) }?;
        let egl_context = EGLContext::new(&egl_display)?;

        // Create GLES renderer
        let renderer = unsafe {
            GlesRenderer::new(egl_context)?
        };

        info!("✓ DRM backend initialized");

        Ok(BackendState {
            backend_type: BackendType::Drm,
            winit_back: None,
            renderer: Some(renderer),
        })
    }

    /// Initialize Winit backend (for development)
    pub fn init_winit() -> Result<Self> {
        info!("Initializing Winit backend");

        // Create Winit window and event loop
        let (backend, _event_loop): (WinitGraphicsBackend<GlesRenderer>, WinitEventLoop) = smithay::backend::winit::init().map_err(|e| anyhow::anyhow!("Failed to initialize winit backend: {:?}", e))?;
        // let renderer = backend.renderer();

        info!("✓ Winit backend initialized");

        Ok(BackendState {
            backend_type: BackendType::Winit,
            winit_back: Some(backend),
            renderer: None,
        })
    }

    /// Initialize headless backend (for testing)
    pub fn init_headless() -> Result<Self> {
        info!("Initializing Headless backend");

        // Create headless EGL context
        // let egl_display = EGLDisplay::new_headless()?;
        // let egl_context = EGLContext::new(&egl_display)?;

        // let renderer = GlesRenderer::new(EGLContext);

        info!("✓ Headless backend initialized");

        Ok(BackendState {
            backend_type: BackendType::Headless,
            winit_back: None,
            renderer: None,
        })
    }

    /// Auto-detect and initialize appropriate backend
    pub fn init_auto() -> Result<Self> {
        info!("Auto-detecting backend");

        // Try DRM first (for real hardware)
        if let Ok(backend) = Self::try_init_drm() {
            return Ok(backend);
        }

        // Fall back to Winit (for development)
        if let Ok(backend) = Self::init_winit() {
            return Ok(backend);
        }

        // Last resort: headless
        Self::init_headless()
    }

    fn try_init_drm() -> Result<Self> {
        // Try common DRM device paths
        let paths = vec![
            PathBuf::from("/dev/dri/card0"),
            PathBuf::from("/dev/dri/card1"),
            PathBuf::from("/dev/dri/renderD128"),
        ];

        for path in paths {
            if path.exists() {
                if let Ok(backend) = Self::init_drm(path) {
                    return Ok(backend);
                }
            }
        }

        anyhow::bail!("No DRM device found")
    }

    fn open_drm_device(node: DrmNode) -> Result<(DrmDevice, GbmDevice<DrmDeviceFd>)> {
    // Open the device node as a File
    let file = File::open(node.dev_path().as_ref().unwrap())?;
    let device_fd = DeviceFd::from(file.as_fd().try_clone_to_owned().unwrap());

    // Create DrmDeviceFd from DeviceFd
    let drm_device_fd = DrmDeviceFd::new(device_fd);

    // Create DrmDevice (drm is the actual device, notifier handles events)
    let (drm, _notifier) = DrmDevice::new(drm_device_fd.clone(), true)?;

    // Create GBM device for rendering
    let gbm = GbmDevice::new(drm_device_fd)?;

    Ok((drm, gbm))
}

/// Get renderer reference (now requires mutable self)
pub fn renderer_mut(&mut self) -> &mut GlesRenderer {
    match self.backend_type {
        BackendType::Winit => {
            self.winit_back
                .as_mut()
                .expect("Winit backend not initialized")
                .renderer()  // This returns &mut GlesRenderer in smithay 0.7
        }
        BackendType::Drm | BackendType::Headless => {
            self.renderer
                .as_mut()
                .expect("Renderer not initialized")
        }
    }
}

// Remove the immutable renderer() method, or only support it for non-Winit backends
pub fn renderer(&self) -> &GlesRenderer {
    match self.backend_type {
        BackendType::Winit => {
            panic!("Use renderer_mut() for Winit backend - immutable access not supported")
        }
        BackendType::Drm | BackendType::Headless => {
            self.renderer
                .as_ref()
                .expect("Renderer not initialized")
        }
    }
}



}

/// GPU information
#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub vendor: String,
    pub renderer: String,
    pub version: String,
    pub extensions: Vec<String>,
}

impl GpuInfo {
    /// Query GPU information from renderer
    pub fn query(_renderer: &mut GlesRenderer) -> Self {
        unsafe {
            let vendor = Self::get_gl_string(gl::VENDOR);
            let renderer_name = Self::get_gl_string(gl::RENDERER);
            let version = Self::get_gl_string(gl::VERSION);

            GpuInfo {
                vendor,
                renderer: renderer_name,
                version,
                extensions: Vec::new(),
            }
        }
    }

    unsafe fn get_gl_string(name: gl::types::GLenum) -> String {
        let ptr = gl::GetString(name);
        if ptr.is_null() {
            "Unknown".to_string()
        } else {
            CStr::from_ptr(ptr as *const _)
                .to_string_lossy()
                .into_owned()
        }
    }

    /// Print GPU information
    pub fn print(&self) {
        info!("GPU Information:");
        info!("  Vendor: {}", self.vendor);
        info!("  Renderer: {}", self.renderer);
        info!("  Version: {}", self.version);
    }
}
