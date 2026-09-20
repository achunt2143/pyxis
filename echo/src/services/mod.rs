//! GeckoOS Service Modules
//! 
//! Organized by service type, each module handles a specific domain of system functionality.
//! All handlers follow the same pattern: accept Message, execute operation, return Message response.

pub mod filesystem;
pub mod appmanager;
pub mod apphost;
pub mod nova;
pub mod installer;
pub mod power;
pub mod settings;
pub mod network;
pub mod deviceinfo;
pub mod notification;
pub mod process;
pub mod system;
pub mod terminal;

// Re-exports for convenience
pub use filesystem::FilesystemService;
pub use appmanager::AppManagerService;
pub use apphost::{AppHost, AppManifest, AppSession, AppState, APPHOST_IFACE};
pub use nova::{Nova, WebAppManifest, NOVA_IFACE};
pub use installer::{InstallerService, PackageInfo, INSTALLER_IFACE};
pub use power::PowerService;
pub use settings::SettingsService;
pub use network::NetworkService;
pub use deviceinfo::DeviceInfoService;
pub use notification::NotificationService;
pub use process::ProcessService;
pub use system::SystemService;
pub use terminal::{TerminalService, TERMINAL_IFACE};