pub mod services;
pub mod ui;
pub mod taskbar;
pub mod launcher;
pub mod terminal;
pub mod settings;
pub mod browser;
pub mod window_manager;

pub use services::{ShellService, ShellState};
pub use ui::UIServer;
pub use taskbar::TaskbarManager;
pub use launcher::AppLauncher;
pub use terminal::TerminalManager;
pub use settings::SettingsManager;
pub use browser::BrowserManager;
pub use window_manager::WindowManager;

use anyhow::Result;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing;
use std::path::PathBuf;

/// Gaia: System Shell UI for GeckoOS
/// Provides task manager, launcher, terminal, settings, browser, and window decorations
pub struct GaiaShell {
    service: Arc<ShellService>,
    ui_server: Arc<UIServer>,
    taskbar: Arc<TaskbarManager>,
    launcher: Arc<AppLauncher>,
    terminal: Arc<TerminalManager>,
    settings: Arc<SettingsManager>,
    browser: Arc<BrowserManager>,
    window_manager: Arc<WindowManager>,
    state: Arc<RwLock<ShellState>>,
}

impl GaiaShell {
    /// Initialize Gaia Shell
    pub async fn new(data_dir: PathBuf) -> Result<Self> {
        tracing::info!("Initializing Gaia Shell UI");

        let service = Arc::new(ShellService::new().await?);
        let ui_server = Arc::new(UIServer::new(8080).await?);
        let taskbar = Arc::new(TaskbarManager::new(service.clone()).await?);
        let launcher = Arc::new(AppLauncher::new(service.clone()).await?);
        let terminal = Arc::new(TerminalManager::new(service.clone()).await?);
        let settings = Arc::new(SettingsManager::new(data_dir.clone()).await?);
        let browser = Arc::new(BrowserManager::new(service.clone()).await?);
        let window_manager = Arc::new(WindowManager::new(service.clone()).await?);

        let state = Arc::new(RwLock::new(ShellState::new()));

        let gaia = GaiaShell {
            service,
            ui_server,
            taskbar,
            launcher,
            terminal,
            settings,
            browser,
            window_manager,
            state,
        };

        tracing::info!("Gaia Shell initialized successfully");
        Ok(gaia)
    }

    /// Start the Gaia Shell
    pub async fn start(&self) -> Result<()> {
        tracing::info!("Starting Gaia Shell");

        // Start UI server
        self.ui_server.start().await?;

        // Setup taskbar
        self.taskbar.setup().await?;

        // Start listening to process events
        self.setup_process_listener().await?;

        tracing::info!("Gaia Shell started successfully");
        Ok(())
    }

    /// Setup listener for process events from Cronus
    async fn setup_process_listener(&self) -> Result<()> {
        // Subscribe to process events from IPC Bus
        // Will be connected via WebSocket

        Ok(())
    }

    /// Get service reference
    pub fn service(&self) -> Arc<ShellService> {
        self.service.clone()
    }

    /// Get taskbar manager
    pub fn taskbar(&self) -> Arc<TaskbarManager> {
        self.taskbar.clone()
    }

    /// Get launcher
    pub fn launcher(&self) -> Arc<AppLauncher> {
        self.launcher.clone()
    }

    /// Get terminal manager
    pub fn terminal(&self) -> Arc<TerminalManager> {
        self.terminal.clone()
    }

    /// Get settings manager
    pub fn settings(&self) -> Arc<SettingsManager> {
        self.settings.clone()
    }

    /// Get browser manager
    pub fn browser(&self) -> Arc<BrowserManager> {
        self.browser.clone()
    }

    /// Get window manager
    pub fn window_manager(&self) -> Arc<WindowManager> {
        self.window_manager.clone()
    }

    /// Get shell state
    pub fn state(&self) -> Arc<RwLock<ShellState>> {
        self.state.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_gaia_initialization() {
        let shell = GaiaShell::new("/tmp/geckoOS".into()).await.unwrap();
        assert!(!shell.state.read().initialized);
    }
}
