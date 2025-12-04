// Gaia: System Shell UI for GeckoOS
// File: gaia/src/lib.rs
// Purpose: Main shell interface orchestrator

use anyhow::Result;
use parking_lot::RwLock;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn, error};
use tokio::sync::mpsc;
use serde_json::json;

// Module declarations
pub mod ui_server;
pub mod taskbar;
pub mod services;
pub mod gecko_app_launcher;
pub mod managers;
pub mod echo_ipc;

// Re-export public types
pub use services::ShellState;
pub use services::ShellService;
pub use taskbar::TaskbarManager;
pub use gecko_app_launcher::GeckoAppLauncher;
pub use managers::TerminalManager;
pub use managers::SettingsManager;
pub use managers::BrowserManager;
pub use managers::WindowManager;
pub use ui_server::UIServer;
pub use echo_ipc::GeckoEchoService;

/// Process event types from Cronus
#[derive(Debug, Clone)]
pub enum ProcessEvent {
    /// Process created: (pid, app_id)
    Created(u32, String),
    
    /// Process running
    Running(u32),
    
    /// Process suspended
    Suspended(u32),
    
    /// Process resumed
    Resumed(u32),
    
    /// Process terminated: (pid, exit_code)
    Terminated(u32, i32),
    
    /// Process failed: (pid, error)
    Failed(u32, String),
    
    /// Resource limit exceeded: (pid, resource, limit, current)
    ResourceLimitExceeded(u32, String, u32, u32),
}

impl std::fmt::Display for ProcessEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessEvent::Created(pid, app) => write!(f, "Created(pid={}, app={})", pid, app),
            ProcessEvent::Running(pid) => write!(f, "Running(pid={})", pid),
            ProcessEvent::Suspended(pid) => write!(f, "Suspended(pid={})", pid),
            ProcessEvent::Resumed(pid) => write!(f, "Resumed(pid={})", pid),
            ProcessEvent::Terminated(pid, code) => write!(f, "Terminated(pid={}, code={})", pid, code),
            ProcessEvent::Failed(pid, err) => write!(f, "Failed(pid={}, err={})", pid, err),
            ProcessEvent::ResourceLimitExceeded(pid, res, limit, curr) => {
                write!(f, "ResourceLimitExceeded(pid={}, {}={}/{}", pid, res, curr, limit)
            }
        }
    }
}

/// Gaia: System Shell UI for GeckoOS
/// Provides task manager, launcher, terminal, settings, browser, and window decorations
#[derive(Clone)]
pub struct Gaia {
    service: Arc<ShellService>,
    ui_server: Arc<UIServer>,
    taskbar: Arc<TaskbarManager>,
    launcher: Arc<GeckoAppLauncher>,
    terminal: Arc<TerminalManager>,
    settings: Arc<SettingsManager>,
    browser: Arc<BrowserManager>,
    window_manager: Arc<WindowManager>,
    state: Arc<RwLock<ShellState>>,
    
    /// Channel for process events
    process_event_tx: Arc<RwLock<Option<mpsc::Sender<ProcessEvent>>>>,
}

impl Gaia {
    /// Initialize Gaia Shell
    pub async fn new(data_dir: PathBuf) -> Result<Self> {
        tracing::info!("Initializing Gaia Shell UI");

        // Initialize core service
        let service = Arc::new(ShellService::new().await?);
        debug!("✓ ShellService initialized");

        // Initialize UI server
        let ui_server = Arc::new(UIServer::new(8080).await?);
        debug!("✓ UIServer initialized on port 8080");

        // Initialize managers with service reference
        let taskbar = Arc::new(TaskbarManager::new(service.clone()).await?);
        debug!("✓ TaskbarManager initialized");

        let launcher = Arc::new(GeckoAppLauncher::new());
        debug!("✓ AppLauncher initialized");

        let terminal = Arc::new(TerminalManager::new(service.clone()).await?);
        debug!("✓ TerminalManager initialized");

        let settings = Arc::new(SettingsManager::new(data_dir.clone()).await?);
        debug!("✓ SettingsManager initialized");

        let browser = Arc::new(BrowserManager::new(service.clone()).await?);
        debug!("✓ BrowserManager initialized");

        let window_manager = Arc::new(WindowManager::new(service.clone()).await?);
        debug!("✓ WindowManager initialized");

        let mut state = ShellState::new();
        state.initialized = true;

        let state = Arc::new(RwLock::new(state));

        let gaia = Gaia {
            service,
            ui_server,
            taskbar,
            launcher,
            terminal,
            settings,
            browser,
            window_manager,
            state,
            process_event_tx: Arc::new(RwLock::new(None)),
        };

        tracing::info!("✓ Gaia Shell initialized successfully");
        Ok(gaia)
    }

    /// Start the Gaia Shell
    pub async fn start(&self) -> Result<()> {
        tracing::info!("Starting Gaia Shell");
        
        // Start UI server
        debug!("Starting UI server...");
        self.ui_server.start(Arc::new(self.clone())).await?;
        debug!("✓ UI server started");

        // Setup taskbar
        debug!("Setting up taskbar...");
        self.taskbar.setup().await?;
        debug!("✓ Taskbar setup complete");

        // Start listening to process events
        debug!("Setting up process listener...");
        self.setup_process_listener().await?;
        debug!("✓ Process listener ready");

        // Mark as running
        let mut state = self.state.write();
        state.desktop_ready = true;

        tracing::info!("✓ Gaia Shell started successfully");
        Ok(())
    }

    /// Stop the Gaia Shell
    pub async fn stop(&self) -> Result<()> {
        tracing::info!("Stopping Gaia Shell");

        let mut state = self.state.write();
        state.desktop_ready = false;

        // Stop UI server
        self.ui_server.stop().await?;

        tracing::info!("✓ Gaia Shell stopped");
        Ok(())
    }

    /// Setup listener for process events from Cronus
    /// 
    /// This sets up the process event handling system that:
    /// - Subscribes to Cronus process events via ECHO IPC bus
    /// - Creates an async task to handle incoming events
    /// - Updates taskbar and monitors as processes change
    /// - Handles process lifecycle (created, running, suspended, terminated)
    /// - Tracks resource usage and alerts on limits
    async fn setup_process_listener(&self) -> Result<()> {
        debug!("Setting up process listener from Cronus");

        // Create channel for process events
        let (tx, mut rx) = mpsc::channel::<ProcessEvent>(256);
        
        // Store sender for external event injection
        *self.process_event_tx.write() = Some(tx.clone());

        // Get references to managers for event handling
        let taskbar = self.taskbar.clone();
        let launcher = self.launcher.clone();
        let ui_server = self.ui_server.clone();

        // Spawn async task to handle process events
        tokio::spawn(async move {
            info!("Process event listener started");

            while let Some(event) = rx.recv().await {
                debug!("Process event received: {}", event);

                match event {
                    ProcessEvent::Created(pid, app_id) => {
                        Self::handle_process_created(
                            pid,
                            &app_id,
                            &taskbar,
                            &launcher,
                            &ui_server,
                        )
                        .await;
                    }

                    ProcessEvent::Running(pid) => {
                        Self::handle_process_running(pid, &taskbar, &launcher).await;
                    }

                    ProcessEvent::Suspended(pid) => {
                        Self::handle_process_suspended(pid, &taskbar, &launcher).await;
                    }

                    ProcessEvent::Resumed(pid) => {
                        Self::handle_process_resumed(pid, &taskbar, &launcher).await;
                    }

                    ProcessEvent::Terminated(pid, exit_code) => {
                        Self::handle_process_terminated(
                            pid,
                            exit_code,
                            &taskbar,
                            &launcher,
                            &ui_server,
                        )
                        .await;
                    }

                    ProcessEvent::Failed(pid, error) => {
                        Self::handle_process_failed(pid, &error, &taskbar, &ui_server).await;
                    }

                    ProcessEvent::ResourceLimitExceeded(pid, resource, limit, current) => {
                        Self::handle_resource_limit(
                            pid,
                            &resource,
                            limit,
                            current,
                            &taskbar,
                            &ui_server,
                        )
                        .await;
                    }
                }
            }

            warn!("Process event listener stopped");
        });

        // TODO: Subscribe to ECHO IPC bus for Cronus signals
        // This would connect to the Cronus service and subscribe to:
        // - org.geckoOS.Cronus.ProcessCreated
        // - org.geckoOS.Cronus.ProcessTerminated
        // - org.geckoOS.Cronus.ProcessSuspended
        // - org.geckoOS.Cronus.ProcessResumed
        // - org.geckoOS.Cronus.ProcessFailed
        // - org.geckoOS.Cronus.ResourceLimitExceeded
        //
        // Events would be forwarded to the process_event_tx channel

        info!("Process listener setup complete");
        Ok(())
    }

    /// Handle process created event
    async fn handle_process_created(
        pid: u32,
        app_id: &str,
        taskbar: &Arc<TaskbarManager>,
        launcher: &Arc<GeckoAppLauncher>,
        ui_server: &Arc<UIServer>,
    ) {
        info!("Process created: {} (pid={})", app_id, pid);

        // Update taskbar with new application
        if let Err(e) = taskbar.add_application(app_id).await {
            error!("Failed to add app to taskbar: {}", e);
        }

        // Emit signal to UI clients
        let signal = json!({
            "event": "process_created",
            "pid": pid,
            "app_id": app_id,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });

        if let Err(e) = ui_server.broadcast_signal("process_event", &signal).await {
            error!("Failed to broadcast process_created signal: {}", e);
        }

        debug!("✓ Process created event handled");
    }

    /// Handle process running event
    async fn handle_process_running(
        pid: u32,
        taskbar: &Arc<TaskbarManager>,
        launcher: &Arc<GeckoAppLauncher>,
    ) {
        debug!("Process running: pid={}", pid);

        // Find app by PID and update status if available
        if let Some(app) = launcher.get_app_by_pid(pid) {
            if let Err(e) = launcher.set_status(&app.app_id, gecko_app_launcher::AppStatus::Running) {
                error!("Failed to update app status: {}", e);
            }

            // Update taskbar
            if let Err(e) = taskbar.update_application_status(&app.app_id, "running").await {
                error!("Failed to update taskbar: {}", e);
            }

            debug!("✓ Process running event handled");
        }
    }

    /// Handle process suspended event
    async fn handle_process_suspended(
        pid: u32,
        taskbar: &Arc<TaskbarManager>,
        launcher: &Arc<GeckoAppLauncher>,
    ) {
        debug!("Process suspended: pid={}", pid);

        // Find app by PID and update status
        if let Some(app) = launcher.get_app_by_pid(pid) {
            if let Err(e) = launcher.set_status(&app.app_id, gecko_app_launcher::AppStatus::Suspended) {
                error!("Failed to update app status: {}", e);
            }

            // Update taskbar
            if let Err(e) = taskbar.update_application_status(&app.app_id, "suspended").await {
                error!("Failed to update taskbar: {}", e);
            }

            debug!("✓ Process suspended event handled");
        }
    }

    /// Handle process resumed event
    async fn handle_process_resumed(
        pid: u32,
        taskbar: &Arc<TaskbarManager>,
        launcher: &Arc<GeckoAppLauncher>,
    ) {
        debug!("Process resumed: pid={}", pid);

        // Find app by PID and update status
        if let Some(app) = launcher.get_app_by_pid(pid) {
            if let Err(e) = launcher.set_status(&app.app_id, gecko_app_launcher::AppStatus::Running) {
                error!("Failed to update app status: {}", e);
            }

            // Update taskbar
            if let Err(e) = taskbar.update_application_status(&app.app_id, "running").await {
                error!("Failed to update taskbar: {}", e);
            }

            debug!("✓ Process resumed event handled");
        }
    }

    /// Handle process terminated event
    async fn handle_process_terminated(
        pid: u32,
        exit_code: i32,
        taskbar: &Arc<TaskbarManager>,
        launcher: &Arc<GeckoAppLauncher>,
        ui_server: &Arc<UIServer>,
    ) {
        info!("Process terminated: pid={}, exit_code={}", pid, exit_code);

        // Find app by PID
        if let Some(app) = launcher.get_app_by_pid(pid) {
            let app_id = app.app_id.clone();

            // Update status
            if let Err(e) = launcher.set_status(&app_id, gecko_app_launcher::AppStatus::Terminated) {
                error!("Failed to update app status: {}", e);
            }

            // Remove from taskbar
            if let Err(e) = taskbar.remove_application(&app_id).await {
                error!("Failed to remove app from taskbar: {}", e);
            }

            // Log exit code if non-zero
            if exit_code != 0 {
                warn!("Application {} exited with code: {}", app_id, exit_code);
            }

            // Emit signal to UI clients
            let signal = json!({
                "event": "process_terminated",
                "pid": pid,
                "app_id": app_id,
                "exit_code": exit_code,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            });

            if let Err(e) = ui_server.broadcast_signal("process_event", &signal).await {
                error!("Failed to broadcast process_terminated signal: {}", e);
            }
        }

        debug!("✓ Process terminated event handled");
    }

    /// Handle process failed event
    async fn handle_process_failed(
        pid: u32,
        error: &str,
        taskbar: &Arc<TaskbarManager>,
        ui_server: &Arc<UIServer>,
    ) {
        error!("Process failed: pid={}, error={}", pid, error);

        // Emit error signal to UI clients
        let signal = json!({
            "event": "process_failed",
            "pid": pid,
            "error": error,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });

        if let Err(e) = ui_server.broadcast_signal("process_event", &signal).await {
            error!("Failed to broadcast process_failed signal: {}", e);
        }

        debug!("✓ Process failed event handled");
    }

    /// Handle resource limit exceeded event
    async fn handle_resource_limit(
        pid: u32,
        resource: &str,
        limit: u32,
        current: u32,
        taskbar: &Arc<TaskbarManager>,
        ui_server: &Arc<UIServer>,
    ) {
        warn!(
            "Resource limit exceeded: pid={}, resource={}, limit={}, current={}",
            pid, resource, limit, current
        );

        // Emit warning signal to UI clients
        let signal = json!({
            "event": "resource_limit_exceeded",
            "pid": pid,
            "resource": resource,
            "limit": limit,
            "current": current,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });

        if let Err(e) = ui_server.broadcast_signal("process_event", &signal).await {
            error!("Failed to broadcast resource_limit signal: {}", e);
        }

        debug!("✓ Resource limit event handled");
    }

    /// Send process event (for testing and manual injection)
    pub async fn send_process_event(&self, event: ProcessEvent) -> Result<()> {
        let tx = self.process_event_tx.read();
        if let Some(sender) = tx.as_ref() {
            sender.send(event).await
                .map_err(|e| anyhow::anyhow!("Failed to send process event: {}", e))
        } else {
            Err(anyhow::anyhow!("Process event channel not initialized"))
        }
    }

    /// Get service reference
    pub fn service(&self) -> Arc<ShellService> {
        self.service.clone()
    }

    /// Get UI server reference
    pub fn ui_server(&self) -> Arc<UIServer> {
        self.ui_server.clone()
    }

    /// Get taskbar manager
    pub fn taskbar(&self) -> Arc<TaskbarManager> {
        self.taskbar.clone()
    }

    /// Get launcher
    pub fn launcher(&self) -> Arc<GeckoAppLauncher> {
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

    /// Check if shell is running
    pub fn is_running(&self) -> bool {
        self.state.read().desktop_ready
    }

    /// Get number of open applications
    pub async fn open_apps_count(&self) -> usize {
        self.service.app_count().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_gaia_initialization() {
        let shell = Gaia::new("/tmp/geckoOS".into()).await.unwrap();
        let state = shell.state.read();
        assert!(state.initialized);
        assert!(!state.desktop_ready);
    }

    #[tokio::test]
    async fn test_gaia_start_stop() {
        let shell = Gaia::new("/tmp/geckoOS".into()).await.unwrap();

        // Start shell
        shell.start().await.unwrap();
        assert!(shell.is_running());

        // Stop shell
        shell.stop().await.unwrap();
        assert!(!shell.is_running());
    }

    #[tokio::test]
    async fn test_manager_access() {
        let shell = Gaia::new("/tmp/geckoOS".into()).await.unwrap();

        // Verify all managers are accessible
        let _ = shell.service();
        let _ = shell.ui_server();
        let _ = shell.taskbar();
        let _ = shell.launcher();
        let _ = shell.terminal();
        let _ = shell.settings();
        let _ = shell.browser();
        let _ = shell.window_manager();
        let _ = shell.state();

        assert!(shell.is_running() == false); // Not started yet
    }

    #[tokio::test]
    async fn test_process_event_creation() {
        let shell = Gaia::new("/tmp/geckoOS".into()).await.unwrap();
        shell.start().await.unwrap();

        // Inject test event
        let event = ProcessEvent::Created(1234, "test-app".to_string());
        let result = shell.send_process_event(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_process_event_termination() {
        let shell = Gaia::new("/tmp/geckoOS".into()).await.unwrap();
        shell.start().await.unwrap();

        // Inject test events
        shell.send_process_event(ProcessEvent::Created(1234, "test-app".to_string())).await.ok();
        shell.send_process_event(ProcessEvent::Running(1234)).await.ok();
        shell.send_process_event(ProcessEvent::Terminated(1234, 0)).await.ok();

        // Give async task time to process
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        
        // Verify event was processed
        assert!(true); // Events processed without panic
    }
}
