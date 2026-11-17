// Rockhopper: Linux Application Support Layer for GeckoOS
// File: rockhopper/src/lib.rs
// Purpose: Main orchestrator for Linux compatibility

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
pub use elf_loader::ElfLoader;
pub use runtime::Runtime;
pub use compatibility::CompatibilityLayer;
pub use echo_ipc::RockhopperEchoService;

use anyhow::Result;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing;
use std::path::{Path, PathBuf};
use std::collections::HashMap;

/// Rockhopper: Linux Application Support Layer for GeckoOS
/// Provides native binary execution, Wayland/X11 compatibility, and D-Bus access
pub struct Rockhopper {
    native_executor: Arc<NativeExecutor>,
    wayland_adapter: Arc<WaylandClientAdapter>,
    x11_compat: Arc<X11CompatibilityLayer>,
    dbus_manager: Arc<DBusSessionManager>,
    #[allow(dead_code)]
    elf_loader: Arc<ElfLoader>,
    runtime: Arc<Runtime>,
    compat_layer: Arc<CompatibilityLayer>,
    state: Arc<RwLock<RockhopperState>>,
}

#[derive(Debug, Clone)]
pub struct RockhopperState {
    pub initialized: bool,
    pub running_binaries: usize,
    pub active_displays: usize,
    pub dbus_connected: bool,
}

/// Configuration for Rockhopper
#[derive(Debug, Clone)]
pub struct RockhopperConfig {
    pub enable_wayland: bool,
    pub enable_x11: bool,
    pub enable_dbus: bool,
    pub app_search_paths: Vec<PathBuf>,
    pub library_paths: Vec<PathBuf>,
    pub x11_display: Option<String>,
    pub wayland_socket: Option<String>,
}

impl Default for RockhopperConfig {
    fn default() -> Self {
        RockhopperConfig {
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

impl Rockhopper {
    /// Initialize Rockhopper with configuration
    pub async fn new(config: RockhopperConfig) -> Result<Self> {
        tracing::info!("Initializing Rockhopper Linux Compatibility Layer");

        let native_executor = Arc::new(NativeExecutor::new().await?);
        let elf_loader = Arc::new(ElfLoader);
        let runtime = Arc::new(Runtime::new(std::process::id()));

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

        let compat_layer = Arc::new(CompatibilityLayer::new());

        let state = Arc::new(RwLock::new(RockhopperState {
            initialized: true,
            running_binaries: 0,
            active_displays: 0,
            dbus_connected: config.enable_dbus,
        }));

        let rockhopper = Rockhopper {
            native_executor,
            wayland_adapter,
            x11_compat,
            dbus_manager,
            elf_loader,
            runtime,
            compat_layer,
            state,
        };

        tracing::info!("Rockhopper initialized successfully");
        Ok(rockhopper)
    }

    /// Execute a native Linux binary
    pub async fn execute_binary(
        &self,
        binary: &str,
        args: Vec<String>,
        env: Option<HashMap<String, String>>,
    ) -> Result<u32> {
        tracing::info!("Executing binary: {} with {} args", binary, args.len());

        // Resolve binary path
        let binary_path = self.compat_layer.resolve_binary(binary).await?;

        // Load and validate ELF binary
        let elf_info = ElfLoader::load(&binary_path)?;
        
        tracing::debug!("ELF info - Entry: 0x{:x}, PIE: {}", elf_info.entry_point, elf_info.is_pie);

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
    pub fn elf_loader(&self) -> Arc<ElfLoader> {
        Arc::new(ElfLoader)
    }

    /// Get runtime
    pub fn runtime(&self) -> Arc<Runtime> {
        self.runtime.clone()
    }

    /// Check compatibility of an application
    pub async fn check_compatibility(&self, app_path: &Path) -> Result<CompatibilityReport> {
        tracing::debug!("Checking compatibility: {:?}", app_path);

        let mut report = CompatibilityReport::new(app_path.to_string_lossy().to_string());

        // Check ELF format
        if let Ok(elf_info) = ElfLoader::load(app_path) {
            report.is_elf = true;
            report.arch = Some(if elf_info.is_pie { "PIE".to_string() } else { "Executable".to_string() });
            report.bits = Some(64); // TODO: Detect actual bits from ELF header
        }

        // Check dependencies
        if let Ok(elf_info) = ElfLoader::load(app_path) {
            if let Some(dyn_info) = &elf_info.dynamic_info {
                report.dependencies = dyn_info.needed_libraries.clone();
            }
        }

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
    pub fn state(&self) -> Arc<RwLock<RockhopperState>> {
        self.state.clone()
    }

    /// Shutdown Rockhopper
    pub async fn shutdown(&self) -> Result<()> {
        tracing::info!("Shutting down Rockhopper");
        
        // Kill all running processes
        let processes = self.native_executor.list_processes().await?;
        for process in processes {
            let _ = self.native_executor.kill_process(process.pid).await;
        }
        
        tracing::info!("Rockhopper shutdown complete");
        Ok(())
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
    async fn test_rockhopper_init() {
        let config = RockhopperConfig::default();
        let rockhopper = Rockhopper::new(config).await.unwrap();
        let state = rockhopper.state.read();
        assert!(state.initialized);
    }

    #[tokio::test]
    async fn test_compatibility_report() {
        let report = CompatibilityReport::new("/usr/bin/ls".to_string());
        assert!(!report.is_compatible());
    }
}