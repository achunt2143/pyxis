//! GeckoOS Service Modules
//! 
//! Organized by service type, each module handles a specific domain of system functionality.
//! All handlers follow the same pattern: accept Message, execute operation, return Message response.

pub mod filesystem;
pub mod appmanager;
pub mod apphost;
pub mod nova;
pub mod installer;
pub mod package_sig;
pub mod power;
pub mod settings;
pub mod network;
pub mod deviceinfo;
pub mod notification;
pub mod sound;
pub mod audio;
pub mod process;
pub mod system;
pub mod terminal;
pub mod time;
pub mod session;
pub mod accounts;
pub mod privilege;

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
pub use notification::NotificationStore;
pub use process::ProcessService;
pub use system::SystemService;
pub use session::{SessionService, SESSION_IFACE};
pub use accounts::{AccountsService, ACCOUNTS_IFACE};
pub use privilege::{PrivilegeService, PRIVILEGE_IFACE, PRIVILEGE_SERVICE};
pub use terminal::{TerminalService, TERMINAL_IFACE};