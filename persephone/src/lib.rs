pub mod native;
pub mod wayland;
pub mod x11;
pub mod dbus_session;
pub mod elf_loader;
pub mod runtime;
pub mod compatibility;
pub mod echo_ipc;

pub use native::NativeExecutor;
pub use wayland::WaylandClientAdapter;
pub use x11::X11CompatibilityLayer;
pub use dbus_session::DBusSessionManager;
pub use elf_loader::ELFLoader;
pub use runtime::LinuxRuntime;
pub use compatibility::CompatibilityLayer;
pub use echo_ipc::PersephoneEchoService;

use anyhow::Result;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing;
use std::path::{Path, PathBuf};

/// Persephone: Linux Application Support Layer for GeckoOS
/// Provides native binary execution, Wayland/X11 compatibility, and D-Bus access
pub struct Persephone {
    native_executor: Arc<NativeExecutor>,
    wayland_adapter: Arc<WaylandClientAdapter>,
    x11_compat: Arc<X11CompatibilityLayer>,
    dbus_manager: Arc<DBusSessionManager>,
    elf_loader: Arc<ELFLoader>,
    runtime: Arc<LinuxRuntime>,
    compat_layer: Arc<CompatibilityLayer>,
    state: Arc<RwLock<PersephoneState>>,
}

#[derive(Debug, Clone)]
pub struct PersephoneState {
    pub initialized: bool,
    pub running_binaries: usize,
    pub active_displays: usize,
    pub dbus_connected: bool,
}

/// Configuration for Persephone
#[derive(Debug, Clone)]
pub struct PersephoneConfig {
    pub enable_wayland: bool,
    pub enable_x11: bool,
    pub enable_dbus: bool,
    pub app_search_paths: Vec<PathBuf>,
    pub library_paths: Vec<PathBuf>,
    pub x11_display: Option<String>,
    pub wayland_socket: Option<String>,
}

impl Default for PersephoneConfig {
    fn default() -> Self {
        PersephoneConfig {
            enable_wayland: true,
            enable_x11: false,
            enable_dbus: true,
            app_search_paths: vec![
                PathBuf::from("/usr/bin"),
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/opt/bin"),
            ],
            library_paths: vec![
                PathBuf::from("/usr/lib"),
                PathBuf::from("/usr/lib/x86_64-linux-gnu"),
                PathBuf::from("/usr/local/lib"),
            ],
            x11_display: None,
            wayland_socket: None,
        }
    }
}

impl Persephone {
    /// Initialize Persephone with configuration
    pub async fn new(config: PersephoneConfig) -> Result<Self> {
        tracing::info!("Initializing Persephone Linux Compatibility Layer");

        let native_executor = Arc::new(NativeExecutor::new().await?);
        let elf_loader = Arc::new(ELFLoader::new(&config.library_paths).await?);
        let runtime = Arc::new(LinuxRuntime::new().await?);

        let wayland_adapter = if config.enable_wayland {
            Arc::new(WaylandClientAdapter::new(&config.wayland_socket).await?)
        } else {
            Arc::new(WaylandClientAdapter::disabled().await?)
        };

        let x11_compat = if config.enable_x11 {
            Arc::new(X11CompatibilityLayer::new(&config.x11_display).await?)
        } else {
            Arc::new(X11CompatibilityLayer::disabled().await?)
        };

        let dbus_manager = if config.enable_dbus {
            Arc::new(DBusSessionManager::new().await?)
        } else {
            Arc::new(DBusSessionManager::disabled().await?)
        };

        let compat_layer = Arc::new(CompatibilityLayer::new(&config.app_search_paths).await?);

        let state = Arc::new(RwLock::new(PersephoneState {
            initialized: true,
            running_binaries: 0,
            active_displays: 0,
            dbus_connected: config.enable_dbus,
        }));

        let persephone = Persephone {
            native_executor,
            wayland_adapter,
            x11_compat,
            dbus_manager,
            elf_loader,
            runtime,
            compat_layer,
            state,
        };

        tracing::info!("Persephone initialized successfully");
        Ok(persephone)
    }

    /// Execute a native Linux binary
    pub async fn execute_binary(
        &self,
        binary: &str,
        args: Vec<String>,
        env: Option<std::collections::HashMap<String, String>>,
    ) -> Result<u32> {
        tracing::info!("Executing binary: {} with {} args", binary, args.len());

        // Resolve binary path
        let binary_path = self.compat_layer.resolve_binary(binary).await?;

        // Load and validate ELF binary
        let elf_info = self.elf_loader.load(&binary_path).await?;

        // Execute via native executor
        let pid = self.native_executor.exec(&binary_path, args, env).await?;

        // Track execution
        let mut state = self.state.write();
        state.running_binaries += 1;

        tracing::debug!("Binary execution started: PID {}", pid);
        Ok(pid)
    }

    /// Get native executor
    pub fn native_executor(&self) -> Arc<NativeExecutor> {
        self.native_executor.clone()
    }

    /// Get Wayland adapter
    pub fn wayland_adapter(&self) -> Arc<WaylandClientAdapter> {
        self.wayland_adapter.clone()
    }

    /// Get X11 compatibility layer
    pub fn x11_compat(&self) -> Arc<X11CompatibilityLayer> {
        self.x11_compat.clone()
    }

    /// Get D-Bus session manager
    pub fn dbus_manager(&self) -> Arc<DBusSessionManager> {
        self.dbus_manager.clone()
    }

    /// Get ELF loader
    pub fn elf_loader(&self) -> Arc<ELFLoader> {
        self.elf_loader.clone()
    }

    /// Get runtime
    pub fn runtime(&self) -> Arc<LinuxRuntime> {
        self.runtime.clone()
    }

    /// Check compatibility of an application
    pub async fn check_compatibility(&self, app_path: &Path) -> Result<CompatibilityReport> {
        tracing::debug!("Checking compatibility: {:?}", app_path);

        let mut report = CompatibilityReport::new(app_path.to_string_lossy().to_string());

        // Check ELF format
        if let Ok(elf_info) = self.elf_loader.load(app_path).await {
            report.is_elf = true;
            report.arch = Some(elf_info.arch);
            report.bits = Some(elf_info.bits);
        }

        // Check dependencies
        report.dependencies = self.elf_loader.get_dependencies(app_path).await.unwrap_or_default();

        // Check D-Bus requirements
        if self.dbus_manager.is_enabled().await {
            report.dbus_ready = true;
        }

        // Check display support
        report.wayland_ready = self.wayland_adapter.is_connected().await;
        report.x11_ready = self.x11_compat.is_connected().await;

        Ok(report)
    }

    /// Get state
    pub fn state(&self) -> Arc<RwLock<PersephoneState>> {
        self.state.clone()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CompatibilityReport {
    pub app_path: String,
    pub is_elf: bool,
    pub arch: Option<String>,
    pub bits: Option<u32>,
    pub dependencies: Vec<String>,
    pub dbus_ready: bool,
    pub wayland_ready: bool,
    pub x11_ready: bool,
}

impl CompatibilityReport {
    pub fn new(app_path: String) -> Self {
        CompatibilityReport {
            app_path,
            is_elf: false,
            arch: None,
            bits: None,
            dependencies: Vec::new(),
            dbus_ready: false,
            wayland_ready: false,
            x11_ready: false,
        }
    }

    pub fn is_compatible(&self) -> bool {
        self.is_elf && (self.wayland_ready || self.x11_ready)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_persephone_init() {
        let config = PersephoneConfig::default();
        let persephone = Persephone::new(config).await.unwrap();
        let state = persephone.state.read();
        assert!(state.initialized);
    }

    #[tokio::test]
    async fn test_compatibility_report() {
        let report = CompatibilityReport::new("/usr/bin/ls".to_string());
        assert!(!report.is_compatible());
    }
}
