//! Echo IPC Client for GeckoOS - Evo Integration
//! 
//! This module provides a synchronous API for Evo (calloop-based) to communicate with the Echo daemon,
//! while maintaining an async broker in a background tokio runtime for efficient Unix socket I/O.
//!
//! Architecture:
//! - `GeckoEchoService`: Main sync API (used from Evo's calloop)
//! - Background broker loop: Runs in dedicated tokio runtime, handles socket I/O
//! - Command channel: Bridge between sync (calloop) and async (broker) code
//! - Window registry: Tracks all windows created by the compositor

use anyhow::{anyhow, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::unix::OwnedWriteHalf;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use geckoOS_echo::{ArgumentDefinition, MethodDefinition, ServiceInterface, SignalDefinition};

// ============================================================================
// TYPE DEFINITIONS
// ============================================================================

/// Callback for window creation requests from Echo IPC
/// Returns (window_id_to_track, success) if handled
pub type CreateWindowHandler =
    dyn Fn(&CreateWindowRequest) -> Result<u32> + Send + Sync + 'static;

/// Window creation request from Echo IPC
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateWindowRequest {
    #[serde(alias = "appid", alias = "app_id")]
    pub app_id: String,
    
    #[serde(alias = "apptype", alias = "app_type")]
    pub app_type: String,
    
    pub width: u32,
    pub height: u32,
    
    #[serde(default)]
    pub url: Option<String>,
}


impl CreateWindowRequest {
    pub fn new(app_id: &str, app_type: &str, width: u32, height: u32) -> Self {
        CreateWindowRequest {
            app_id: app_id.to_string(),
            app_type: app_type.to_string(),
            width,
            height,
            url: None,
        }
    }

    pub fn with_url(mut self, url: &str) -> Self {
        self.url = Some(url.to_string());
        self
    }
}

/// Window information exposed via IPC
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub window_id: u32,
    pub app_id: String,
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub focused: bool,
    pub visible: bool,
}

/// Rendering status exposed via IPC
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderingStatus {
    pub is_ready: bool,
    pub active_windows: usize,
    pub fps: u32,
    pub memory_mb: u32,
}

/// A window-stacking/focus action requested by an incoming Echo IPC method
/// (`FocusWindow`/`MinimizeWindow`/`RestoreWindow`). Applying these touches
/// `Space`/`Seat`, which live on Evo's calloop thread, not the broker's
/// tokio thread -- so `handle_incoming_message` only enqueues the request
/// here and answers the IPC call immediately; `EvoState::
/// process_pending_window_actions` (driven once per event-loop iteration by
/// each backend) drains the queue and does the actual raise/minimize/
/// restore via the same machinery the internal minimize keybinding uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    Focus,
    Minimize,
    Restore,
}

#[derive(Debug, Clone, Copy)]
pub struct PendingWindowAction {
    pub window_id: u32,
    pub action: WindowAction,
}

// ============================================================================
// ECHO PROTOCOL INTERNALS
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EchoMessage {
    pub header: EchoMessageHeader,
    pub body: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EchoMessageHeader {
    pub message_id: String,
    #[serde(rename = "message_type")]
    pub message_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    pub destination: String,
    pub interface: String,
    pub method: String,
    #[serde(default)]
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    pub timestamp: u64,
}

impl EchoMessage {
    fn method_call(destination: String, interface: String, method: String, body: Value) -> Self {
        EchoMessage {
            header: EchoMessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: "METHOD_CALL".to_string(),
                sender: None,
                destination,
                interface,
                method,
                signature: String::new(),
                reply_to: None,
                timestamp: now_ms(),
            },
            body,
        }
    }

    fn method_return(reply_to: String, body: Value) -> Self {
        EchoMessage {
            header: EchoMessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: "METHOD_RETURN".to_string(),
                sender: Some("org.geckoOS.evo".to_string()),
                destination: String::new(),
                interface: "org.geckoOS.evo".to_string(),
                method: String::new(),
                signature: String::new(),
                reply_to: Some(reply_to),
                timestamp: now_ms(),
            },
            body,
        }
    }

    fn method_error(reply_to: String, error_message: &str) -> Self {
        EchoMessage {
            header: EchoMessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: "ERROR".to_string(),
                sender: Some("org.geckoOS.evo".to_string()),
                destination: String::new(),
                interface: "org.geckoOS.evo".to_string(),
                method: String::new(),
                signature: String::new(),
                reply_to: Some(reply_to),
                timestamp: now_ms(),
            },
            body: json!({
                "error": error_message
            }),
        }
    }

    fn handshake() -> Self {
        EchoMessage {
            header: EchoMessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: "HANDSHAKE".to_string(),
                sender: None,
                destination: "org.geckoOS.Echo".to_string(),
                interface: "org.geckoOS.Echo".to_string(),
                method: "HANDSHAKE".to_string(),
                signature: String::new(),
                reply_to: None,
                timestamp: now_ms(),
            },
            body: json!({
                "version": "1.0.0",
                "client": "geckoOS-evo-compositor"
            }),
        }
    }
}

// ============================================================================
// COMMAND CHANNEL FOR SYNC/ASYNC BRIDGE
// ============================================================================

/// Commands sent from sync Evo code to async broker
#[derive(Debug)]
enum EchoCommand {
    /// Send a method call and get response
    Call {
        message: EchoMessage,
        reply: tokio::sync::oneshot::Sender<Result<Value>>,
    },
    /// Register the service
    Register {
        interface: ServiceInterface,
        reply: tokio::sync::oneshot::Sender<Result<()>>,
    },
    /// Emit a signal
    EmitSignal {
        signal_name: String,
        args: Value,
        reply: tokio::sync::oneshot::Sender<Result<()>>,
    },
}

// ============================================================================
// GECKO ECHO SERVICE (SYNC API FOR EVO)
// ============================================================================

/// Gecko compositor service on Echo IPC bus
///
/// This provides a synchronous API for use in Evo's calloop-based event loop,
/// while maintaining an async broker in the background for efficient Unix socket I/O.
pub struct GeckoEchoService {
    pub service_name: String,
    pub ready: Arc<RwLock<bool>>,
    pub windows: Arc<RwLock<HashMap<u32, WindowInfo>>>,
    pub next_window_id: Arc<RwLock<u32>>,
    pub command_tx: mpsc::Sender<EchoCommand>,
    pub runtime_handle: tokio::runtime::Handle,
    // ✅ Change Box to Arc
    pub create_window_cb: Arc<RwLock<Option<Arc<dyn Fn(&CreateWindowRequest) -> Result<u32> + Send + Sync>>>>,

    /// Queue of FocusWindow/MinimizeWindow/RestoreWindow requests received
    /// over Echo IPC, awaiting pickup by `EvoState::process_pending_window_actions`.
    pending_window_actions: Arc<RwLock<Vec<PendingWindowAction>>>,

    // ---- Real FPS tracking (see `record_frame`/`get_rendering_status`) ----
    // Frames rendered since `fps_window_start` was last reset. Plain atomic,
    // not RwLock, since this increments once per rendered frame — a hot
    // path where lock overhead would actually matter, unlike the rest of
    // this struct's fields.
    frame_count: Arc<AtomicU64>,
    // Instant (as nanos since an arbitrary epoch via `Instant::elapsed`) the
    // current 1-second measurement window started. Stored as the actual
    // `Instant` behind a lock rather than an atomic since `Instant` isn't
    // atomically representable; contention is negligible (read/reset once
    // per second, not once per frame).
    fps_window_start: Arc<RwLock<Instant>>,
    // Last computed FPS, cached so `get_rendering_status` never blocks on
    // the render thread — it just reads whatever was last computed.
    last_fps: Arc<AtomicU32>,
}


impl GeckoEchoService {
    /// Create a new Gecko Echo service
    ///
    /// This spawns a background tokio runtime for the Echo broker.
    /// The service is designed to be called from sync code (Evo's calloop).
    pub fn new() -> Result<Self> {
    info!("Creating GeckoEchoService with background broker");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("echo-broker")
        .enable_all()
        .build()?;

    let runtime_handle = runtime.handle().clone();
    let ready = Arc::new(RwLock::new(false));
    let create_window_cb = Arc::new(RwLock::new(None));
    let windows = Arc::new(RwLock::new(HashMap::new()));
    let next_window_id = Arc::new(RwLock::new(1));
    let pending_window_actions = Arc::new(RwLock::new(Vec::new()));

    let (command_tx, command_rx) = mpsc::channel::<EchoCommand>(32);

    // Clone for broker
    let windows_clone = Arc::clone(&windows);
    let next_window_id_clone = Arc::clone(&next_window_id);
    let create_window_cb_clone = Arc::clone(&create_window_cb);
    let pending_window_actions_clone = Arc::clone(&pending_window_actions);

    let socket_path = Self::echo_socket_path();

    // Spawn broker with only the parts it needs
    runtime.spawn(async move {
        Self::echo_broker_loop(
            command_rx,
            socket_path,
            windows_clone,
            next_window_id_clone,
            create_window_cb_clone,
            pending_window_actions_clone,
        )
        .await;
    });

    std::mem::forget(runtime);

    info!("✓ Echo broker spawned in background runtime");

    Ok(GeckoEchoService {
        service_name: "org.geckoOS.evo".to_string(),
        ready,
        windows,
        next_window_id,
        command_tx,
        runtime_handle,
        create_window_cb,
        pending_window_actions,
        frame_count: Arc::new(AtomicU64::new(0)),
        fps_window_start: Arc::new(RwLock::new(Instant::now())),
        last_fps: Arc::new(AtomicU32::new(0)),
    })
}


    // ========================================================================
    // SYNC PUBLIC API (for use in Evo)
    // ========================================================================

    /// Register service with Echo bus (SYNC)
    pub fn register(&self) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let interface = self.create_service_interface();
        let cmd = EchoCommand::Register { interface, reply: tx };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Registration channel closed"))?
        })
    }

    /// Signal that Gecko is ready (SYNC)
    pub fn signal_ready(&self) -> Result<()> {
        *self.ready.write() = true;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::EmitSignal {
            signal_name: "GeckoReady".to_string(),
            args: json!({}),
            reply: tx,
        };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Signal channel closed"))?
        })
    }

    /// Allocate a new window ID (SYNC)
    pub fn allocate_window_id(&self) -> u32 {
        let mut id_gen = self.next_window_id.write();
        let id = *id_gen;
        *id_gen = id.wrapping_add(1);
        if id == 0 {
            *id_gen = 1; // Never use 0
        }
        id
    }

    /// Track a window created by Evo (SYNC)
    pub fn track_window(&self, window: WindowInfo) {
        let mut windows = self.windows.write();
        windows.insert(window.window_id, window.clone());
        info!(
            "✓ Window tracked: id={}, app_id={}",
            window.window_id, window.app_id
        );
    }

    /// Update window state (SYNC)
    pub fn update_window(&self, window_id: u32, updates: WindowInfo) -> bool {
        let mut windows = self.windows.write();
        if windows.contains_key(&window_id) {
            windows.insert(window_id, updates);
            true
        } else {
            false
        }
    }

    /// Remove a tracked window (SYNC)
    pub fn untrack_window(&self, window_id: u32) -> bool {
        let mut windows = self.windows.write();
        let removed = windows.remove(&window_id).is_some();
        if removed {
            info!("✓ Window untracked: id={}", window_id);
        }
        removed
    }

    /// Get list of tracked windows (SYNC)
    pub fn get_windows(&self) -> Vec<WindowInfo> {
        self.windows.read().values().cloned().collect()
    }

    /// Get window count (SYNC)
    pub fn window_count(&self) -> usize {
        self.windows.read().len()
    }

    /// Check if ready (SYNC)
    pub fn is_ready(&self) -> bool {
        *self.ready.read()
    }

    /// Get service name (SYNC)
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// Call once per frame actually rendered/presented (from
    /// `udev.rs`'s render loop). Updates the cached FPS once per rolling
    /// 1-second window rather than on every call, so this stays cheap on
    /// the render hot path.
    pub fn record_frame(&self) {
        let count = self.frame_count.fetch_add(1, Ordering::Relaxed) + 1;
        let elapsed = self.fps_window_start.read().elapsed();
        if elapsed.as_secs_f64() >= 1.0 {
            let fps = (count as f64 / elapsed.as_secs_f64()).round() as u32;
            self.last_fps.store(fps, Ordering::Relaxed);
            self.frame_count.store(0, Ordering::Relaxed);
            *self.fps_window_start.write() = Instant::now();
        }
    }

    /// Get rendering status (SYNC)
    pub fn get_rendering_status(&self) -> RenderingStatus {
        let ready = *self.ready.read();
        let windows = self.windows.read();
        RenderingStatus {
            is_ready: ready,
            active_windows: windows.len(),
            fps: self.last_fps.load(Ordering::Relaxed),
            memory_mb: current_process_rss_mb(),
        }
    }

    /// Emit window created signal (SYNC)
    pub fn emit_window_created(
        &self,
        window_id: u32,
        app_id: &str,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::EmitSignal {
            signal_name: "WindowCreated".to_string(),
            args: json!({
                "window_id": window_id,
                "app_id": app_id,
                "width": width,
                "height": height
            }),
            reply: tx,
        };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Signal channel closed"))?
        })
    }

    /// Emit window closed signal (SYNC)
    pub fn emit_window_closed(&self, window_id: u32) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::EmitSignal {
            signal_name: "WindowClosed".to_string(),
            args: json!({ "window_id": window_id }),
            reply: tx,
        };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Signal channel closed"))?
        })
    }

    /// Emit window moved signal (SYNC)
    pub fn emit_window_moved(
        &self,
        window_id: u32,
        x: i32,
        y: i32,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::EmitSignal {
            signal_name: "WindowMoved".to_string(),
            args: json!({
                "window_id": window_id,
                "x": x,
                "y": y,
                "timestamp": now_ms()
            }),
            reply: tx,
        };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Signal channel closed"))?
        })
    }

    /// Emit workspace changed signal (SYNC)
    pub fn emit_workspace_changed(&self, workspace_id: u32, workspace_name: &str) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::EmitSignal {
            signal_name: "WorkspaceChanged".to_string(),
            args: json!({
                "workspace_id": workspace_id,
                "workspace_name": workspace_name,
                "timestamp": now_ms()
            }),
            reply: tx,
        };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Signal channel closed"))?
        })
    }

    /// Emit window moved to workspace signal (SYNC)
    pub fn emit_window_moved_to_workspace(
        &self,
        window_id: u32,
        from_workspace: u32,
        to_workspace: u32,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::EmitSignal {
            signal_name: "WindowMovedToWorkspace".to_string(),
            args: json!({
                "window_id": window_id,
                "from_workspace": from_workspace,
                "to_workspace": to_workspace,
                "timestamp": now_ms()
            }),
            reply: tx,
        };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Signal channel closed"))?
        })
    }

    /// Emit window focus changed signal (SYNC)
    pub fn emit_window_focus_changed(
        &self,
        window_id: Option<u32>,
        workspace_id: u32,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::EmitSignal {
            signal_name: "WindowFocusChanged".to_string(),
            args: json!({
                "window_id": window_id,
                "workspace_id": workspace_id,
                "timestamp": now_ms()
            }),
            reply: tx,
        };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Signal channel closed"))?
        })
    }

    /// Emit window minimized signal (SYNC)
    pub fn emit_window_minimized(&self, window_id: u32, minimized: bool) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::EmitSignal {
            signal_name: "WindowMinimized".to_string(),
            args: json!({
                "window_id": window_id,
                "minimized": minimized,
                "timestamp": now_ms()
            }),
            reply: tx,
        };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Signal channel closed"))?
        })
    }

    /// Emit window maximized signal (SYNC)
    pub fn emit_window_maximized(&self, window_id: u32, maximized: bool) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::EmitSignal {
            signal_name: "WindowMaximized".to_string(),
            args: json!({
                "window_id": window_id,
                "maximized": maximized,
                "timestamp": now_ms()
            }),
            reply: tx,
        };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Signal channel closed"))?
        })
    }

    /// Emit window fullscreen signal (SYNC)
    pub fn emit_window_fullscreen(&self, window_id: u32, fullscreen: bool) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::EmitSignal {
            signal_name: "WindowFullscreen".to_string(),
            args: json!({
                "window_id": window_id,
                "fullscreen": fullscreen,
                "timestamp": now_ms()
            }),
            reply: tx,
        };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Signal channel closed"))?
        })
    }

    /// Emit workspace created signal (SYNC)
    pub fn emit_workspace_created(&self, workspace_id: u32, workspace_name: &str) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::EmitSignal {
            signal_name: "WorkspaceCreated".to_string(),
            args: json!({
                "workspace_id": workspace_id,
                "workspace_name": workspace_name,
                "timestamp": now_ms()
            }),
            reply: tx,
        };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Signal channel closed"))?
        })
    }

    /// Emit workspace deleted signal (SYNC)
    pub fn emit_workspace_deleted(&self, workspace_id: u32) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::EmitSignal {
            signal_name: "WorkspaceDeleted".to_string(),
            args: json!({
                "workspace_id": workspace_id,
                "timestamp": now_ms()
            }),
            reply: tx,
        };

        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            rx.await.map_err(|_| anyhow!("Signal channel closed"))?
        })
    }

    /// Drain and return all pending FocusWindow/MinimizeWindow/RestoreWindow
    /// requests queued since the last call (SYNC). Meant to be polled once
    /// per event-loop iteration from `EvoState::process_pending_window_actions`.
    pub fn take_pending_window_actions(&self) -> Vec<PendingWindowAction> {
        std::mem::take(&mut *self.pending_window_actions.write())
    }

    /// Register a handler for window creation requests from Echo
    /// 
    /// Handler should:
    /// - Parse the CreateWindowRequest
    /// - Spawn the appropriate application
    /// - Return the assigned window_id that the application will eventually claim via new_toplevel
    pub fn set_create_window_handler<F>(&self, handler: F)
    where
        F: Fn(&CreateWindowRequest) -> Result<u32> + Send + Sync + 'static,
    {
        *self.create_window_cb.write() = Some(Arc::new(handler));
    }

    // ========================================================================
    // ASYNC BROKER LOOP (runs in background runtime)
    // ========================================================================

    async fn echo_broker_loop(
    mut command_rx: mpsc::Receiver<EchoCommand>,
    socket_path: PathBuf,
    windows: Arc<RwLock<HashMap<u32, WindowInfo>>>,
    next_window_id: Arc<RwLock<u32>>,
    // ✅ Arc instead of Box
    create_window_cb: Arc<RwLock<Option<Arc<dyn Fn(&CreateWindowRequest) -> Result<u32> + Send + Sync>>>>,
    pending_window_actions: Arc<RwLock<Vec<PendingWindowAction>>>,
) {
    loop {
        match UnixStream::connect(&socket_path).await {
            Ok(stream) => {
                info!("Connected to Echo daemon at {:?}", socket_path);

                let (mut read_half, mut write_half) = stream.into_split();

                if let Err(e) = Self::send_echo_message(&mut write_half, &EchoMessage::handshake()).await {
                    error!("Failed to send handshake: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }

                match Self::recv_echo_message(&mut read_half).await {
                    Ok(resp) => {
                        if resp.header.message_type == "HANDSHAKE_ACK"
                            || resp.header.message_type == "METHOD_RETURN"
                        {
                            info!("✓ Handshake complete with Echo daemon");
                        } else {
                            warn!("Unexpected handshake response: {}", resp.header.message_type);
                        }
                    }
                    Err(e) => {
                        error!("Handshake failed: {}", e);
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        continue;
                    }
                }

                loop {
                    tokio::select! {
                        msg_result = Self::recv_echo_message(&mut read_half) => {
                            match msg_result {
                                Ok(msg) => {
                                    // ✅ Pass the individual components instead of full service
                                    if let Err(e) = Self::handle_incoming_message(
                                        &mut write_half,
                                        msg,
                                        &windows,
                                        &next_window_id,
                                        &create_window_cb,
                                        &pending_window_actions,
                                    )
                                    .await
                                    {
                                        error!("Error handling incoming message: {}", e);
                                    }
                                }
                                Err(e) => {
                                    error!("Error receiving message: {}", e);
                                    break;
                                }
                            }
                        }

                        cmd = command_rx.recv() => {
                            match cmd {
                                Some(cmd) => {
                                    if let Err(e) = Self::process_command(&mut write_half, &mut read_half, cmd).await {
                                        error!("Command processing error: {}", e);
                                        break;
                                    }
                                }
                                None => {
                                    warn!("Command channel closed, shutting down broker");
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("Failed to connect to Echo daemon: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}


    /// Handle incoming METHOD_CALL messages from Echo daemon
    async fn handle_incoming_message(
    write_half: &mut OwnedWriteHalf,
    msg: EchoMessage,
    windows: &Arc<RwLock<HashMap<u32, WindowInfo>>>,
    next_window_id: &Arc<RwLock<u32>>,
    create_window_cb: &Arc<RwLock<Option<Arc<dyn Fn(&CreateWindowRequest) -> Result<u32> + Send + Sync>>>>,
    pending_window_actions: &Arc<RwLock<Vec<PendingWindowAction>>>,
) -> Result<()> {
        // Only handle METHOD_CALL messages
        if msg.header.message_type != "METHOD_CALL" {
            debug!(
                "Ignoring non-METHOD_CALL message: {}",
                msg.header.message_type
            );
            return Ok(());
        }

        let method = msg.header.method.as_str();
        let message_id = msg.header.message_id.clone();
        debug!("Handling incoming method call: {}", method);

        let response = match method {
            "ListWindows" => {
                let windows_list = windows.read();
                let window_list: Vec<Value> = windows_list
                    .values()
                    .map(|w| {
                        json!({
                            "window_id": w.window_id,
                            "app_id": w.app_id,
                            "width": w.width,
                            "height": w.height,
                            "x": w.x,
                            "y": w.y,
                            "focused": w.focused,
                            "visible": w.visible,
                        })
                    })
                    .collect();
                EchoMessage::method_return(message_id, json!({ "windows": window_list }))
            }

            "GetRenderingStatus" => {
                let windows_list = windows.read();
                let status = json!({
                    "is_ready": true,
                    "active_windows": windows_list.len(),
                    "fps": 60,
                    "memory_mb": 256,
                });
                EchoMessage::method_return(message_id, json!({ "status": status }))
            }

            "CreateWindow" => {
                let app_id = msg
                    .body
                    .get("app_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let app_type = msg
                    .body
                    .get("app_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("native")
                    .to_string();
                let width = msg.body.get("width").and_then(|v| v.as_u64()).unwrap_or(800) as u32;
                let height = msg.body.get("height").and_then(|v| v.as_u64()).unwrap_or(600) as u32;
                let url = msg.body.get("url").and_then(|v| v.as_str()).map(|s| s.to_string());

                let request = CreateWindowRequest {
                    app_id: app_id.clone(),
                    app_type,
                    width,
                    height,
                    url,
                };

                // Call the Evo callback that was registered
                let response_msg = {
                    // let service_locked = service.read();
                    let x = if let Some(ref callback) = *create_window_cb.read() {
                        match callback(&request) {
                            Ok(assigned_window_id) => {
                                info!(
                                    "✓ Window creation request handled for {} with ID {}",
                                    app_id, assigned_window_id
                                );
                                EchoMessage::method_return(
                                    message_id,
                                    json!({
                                        "success": true,
                                        "window_id": assigned_window_id,
                                        "app_id": app_id
                                    }),
                                )
                            }
                            Err(e) => {
                                warn!("✗ Window creation failed for {}: {}", app_id, e);
                                EchoMessage::method_error(message_id, &format!("Creation failed: {}", e))
                            }
                        }
                    } else {
                        EchoMessage::method_error(message_id, "No window creation handler registered")
                    }; x
                };
                response_msg
            }

            "CloseWindow" => {
                let window_id = msg
                    .body
                    .get("window_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;

                let was_tracked = {
                    let mut windows_map = windows.write();
                    windows_map.remove(&window_id).is_some()
                };

                if was_tracked {
                    info!("Closed window {}", window_id);
                }

                EchoMessage::method_return(message_id, json!({ "success": was_tracked }))
            }

            "SwitchWorkspace" => {
                let workspace_id = msg
                    .body
                    .get("workspace_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;

                info!("Switched to workspace {}", workspace_id);
                EchoMessage::method_return(
                    message_id,
                    json!({ "success": true, "workspace_id": workspace_id }),
                )
            }

            "FocusWindow" => {
                let window_id = msg
                    .body
                    .get("window_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;

                pending_window_actions.write().push(PendingWindowAction {
                    window_id,
                    action: WindowAction::Focus,
                });
                info!("Queued FocusWindow request for window {}", window_id);
                EchoMessage::method_return(
                    message_id,
                    json!({ "success": true, "window_id": window_id }),
                )
            }

            "MinimizeWindow" => {
                let window_id = msg
                    .body
                    .get("window_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;

                pending_window_actions.write().push(PendingWindowAction {
                    window_id,
                    action: WindowAction::Minimize,
                });
                info!("Queued MinimizeWindow request for window {}", window_id);
                EchoMessage::method_return(
                    message_id,
                    json!({ "success": true, "window_id": window_id }),
                )
            }

            "RestoreWindow" => {
                let window_id = msg
                    .body
                    .get("window_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;

                pending_window_actions.write().push(PendingWindowAction {
                    window_id,
                    action: WindowAction::Restore,
                });
                info!("Queued RestoreWindow request for window {}", window_id);
                EchoMessage::method_return(
                    message_id,
                    json!({ "success": true, "window_id": window_id }),
                )
            }

            "CreateWorkspace" => {
                let workspace_name = msg
                    .body
                    .get("workspace_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("New Workspace")
                    .to_string();

                let workspace_id = 1 + (rand::random::<u32>() % 10);

                info!("Created workspace {}: {}", workspace_id, workspace_name);
                EchoMessage::method_return(
                    message_id,
                    json!({ "workspace_id": workspace_id, "success": true }),
                )
            }

            _ => {
                warn!("Unhandled method call: {}", method);
                EchoMessage::method_error(
                    message_id,
                    &format!("Method not implemented: {}", method),
                )
            }
        };

        Self::send_echo_message(write_half, &response).await
    }

    async fn process_command(
        write_half: &mut tokio::net::unix::OwnedWriteHalf,
        read_half: &mut tokio::net::unix::OwnedReadHalf,
        cmd: EchoCommand,
    ) -> Result<()> {
        match cmd {
            EchoCommand::Call { message, reply } => {
                Self::send_echo_message(write_half, &message).await?;
                let response = Self::recv_echo_message(read_half).await?;
                let result = if response.header.message_type == "METHOD_RETURN" {
                    Ok(response.body)
                } else {
                    Err(anyhow!("Echo error: {:?}", response.body))
                };
                let _ = reply.send(result);
            }

            EchoCommand::Register { interface, reply } => {
                let msg = EchoMessage::method_call(
                    "org.geckoOS.Echo".to_string(),
                    "org.geckoOS.Echo".to_string(),
                    "RegisterService".to_string(),
                    json!({
                        "name": "org.geckoOS.evo",
                        "interface": serde_json::to_value(&interface)?
                    }),
                );

                Self::send_echo_message(write_half, &msg).await?;
                let response = Self::recv_echo_message(read_half).await?;
                let result = if response.header.message_type == "METHOD_RETURN" {
                    info!("✓ Service registered with Echo daemon");
                    Ok(())
                } else {
                    Err(anyhow!("Registration failed: {:?}", response.body))
                };
                let _ = reply.send(result);
            }

            EchoCommand::EmitSignal {
                signal_name,
                args,
                reply,
            } => {
                let msg = EchoMessage::method_call(
                    "org.geckoOS.Echo".to_string(),
                    "org.geckoOS.evo".to_string(),
                    "EmitSignal".to_string(),
                    json!({
                        "signal_name": signal_name,
                        "args": args
                    }),
                );

                Self::send_echo_message(write_half, &msg).await?;
                let response = Self::recv_echo_message(read_half).await?;
                let result = if response.header.message_type == "METHOD_RETURN" {
                    debug!("✓ Signal emitted: {}", signal_name);
                    Ok(())
                } else {
                    Err(anyhow!("Signal emission failed: {:?}", response.body))
                };
                let _ = reply.send(result);
            }
        }

        Ok(())
    }

    // ========================================================================
    // ECHO MESSAGE I/O
    // ========================================================================

    async fn send_echo_message(
        write_half: &mut tokio::net::unix::OwnedWriteHalf,
        msg: &EchoMessage,
    ) -> Result<()> {
        let data = serde_json::to_vec(msg)?;
        let len = data.len() as u32;
        write_half.write_u32_le(len).await?;
        write_half.write_all(&data).await?;
        write_half.flush().await?;
        debug!("Sent Echo message: {} bytes", data.len());
        Ok(())
    }

    async fn recv_echo_message(
        read_half: &mut tokio::net::unix::OwnedReadHalf,
    ) -> Result<EchoMessage> {
        let len = read_half.read_u32_le().await? as usize;
        if len > 1024 * 1024 {
            return Err(anyhow!("Message too large: {} bytes", len));
        }

        let mut buffer = vec![0u8; len];
        read_half.read_exact(&mut buffer).await?;
        debug!("Received Echo message: {} bytes", len);
        let msg: EchoMessage = serde_json::from_slice(&buffer)?;
        Ok(msg)
    }

    // ========================================================================
    // HELPERS
    // ========================================================================

    fn echo_socket_path() -> PathBuf {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("geckoOS/echo.sock")
        } else {
            PathBuf::from("/tmp/geckoOS/echo.sock")
        }
    }

    fn create_service_interface(&self) -> ServiceInterface {
        ServiceInterface {
            name: self.service_name.clone(),
            methods: vec![
                MethodDefinition {
                    name: "CreateWindow".to_string(),
                    input_args: vec![
                        ArgumentDefinition {
                            name: "app_id".to_string(),
                            arg_type: "s".to_string(),
                        },
                        ArgumentDefinition {
                            name: "app_type".to_string(),
                            arg_type: "s".to_string(),
                        },
                        ArgumentDefinition {
                            name: "width".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "height".to_string(),
                            arg_type: "u".to_string(),
                        },
                    ],
                    output_args: vec![ArgumentDefinition {
                        name: "window_id".to_string(),
                        arg_type: "u".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "CloseWindow".to_string(),
                    input_args: vec![ArgumentDefinition {
                        name: "window_id".to_string(),
                        arg_type: "u".to_string(),
                    }],
                    output_args: vec![],
                },
                MethodDefinition {
                    name: "ListWindows".to_string(),
                    input_args: vec![],
                    output_args: vec![ArgumentDefinition {
                        name: "windows".to_string(),
                        arg_type: "a(usuu)".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "GetRenderingStatus".to_string(),
                    input_args: vec![],
                    output_args: vec![ArgumentDefinition {
                        name: "status".to_string(),
                        arg_type: "v".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "SwitchWorkspace".to_string(),
                    input_args: vec![ArgumentDefinition {
                        name: "workspace_id".to_string(),
                        arg_type: "u".to_string(),
                    }],
                    output_args: vec![ArgumentDefinition {
                        name: "success".to_string(),
                        arg_type: "b".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "CreateWorkspace".to_string(),
                    input_args: vec![ArgumentDefinition {
                        name: "workspace_name".to_string(),
                        arg_type: "s".to_string(),
                    }],
                    output_args: vec![ArgumentDefinition {
                        name: "workspace_id".to_string(),
                        arg_type: "u".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "MoveWindowToWorkspace".to_string(),
                    input_args: vec![
                        ArgumentDefinition {
                            name: "window_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "workspace_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                    ],
                    output_args: vec![ArgumentDefinition {
                        name: "success".to_string(),
                        arg_type: "b".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "ListWorkspaces".to_string(),
                    input_args: vec![],
                    output_args: vec![ArgumentDefinition {
                        name: "workspaces".to_string(),
                        arg_type: "a(us)".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "GetActiveWorkspace".to_string(),
                    input_args: vec![],
                    output_args: vec![ArgumentDefinition {
                        name: "workspace_id".to_string(),
                        arg_type: "u".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "FocusWindow".to_string(),
                    input_args: vec![ArgumentDefinition {
                        name: "window_id".to_string(),
                        arg_type: "u".to_string(),
                    }],
                    output_args: vec![ArgumentDefinition {
                        name: "success".to_string(),
                        arg_type: "b".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "MinimizeWindow".to_string(),
                    input_args: vec![ArgumentDefinition {
                        name: "window_id".to_string(),
                        arg_type: "u".to_string(),
                    }],
                    output_args: vec![ArgumentDefinition {
                        name: "success".to_string(),
                        arg_type: "b".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "RestoreWindow".to_string(),
                    input_args: vec![ArgumentDefinition {
                        name: "window_id".to_string(),
                        arg_type: "u".to_string(),
                    }],
                    output_args: vec![ArgumentDefinition {
                        name: "success".to_string(),
                        arg_type: "b".to_string(),
                    }],
                },
            ],
            signals: vec![
                SignalDefinition {
                    name: "GeckoReady".to_string(),
                    arguments: vec![],
                },
                SignalDefinition {
                    name: "WindowCreated".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "window_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "app_id".to_string(),
                            arg_type: "s".to_string(),
                        },
                        ArgumentDefinition {
                            name: "width".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "height".to_string(),
                            arg_type: "u".to_string(),
                        },
                    ],
                },
                SignalDefinition {
                    name: "WindowClosed".to_string(),
                    arguments: vec![ArgumentDefinition {
                        name: "window_id".to_string(),
                        arg_type: "u".to_string(),
                    }],
                },
                SignalDefinition {
                    name: "WindowMoved".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "window_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "x".to_string(),
                            arg_type: "i".to_string(),
                        },
                        ArgumentDefinition {
                            name: "y".to_string(),
                            arg_type: "i".to_string(),
                        },
                        ArgumentDefinition {
                            name: "timestamp".to_string(),
                            arg_type: "t".to_string(),
                        },
                    ],
                },
                SignalDefinition {
                    name: "RenderingStarted".to_string(),
                    arguments: vec![],
                },
                SignalDefinition {
                    name: "RenderingError".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "error_code".to_string(),
                            arg_type: "i".to_string(),
                        },
                        ArgumentDefinition {
                            name: "message".to_string(),
                            arg_type: "s".to_string(),
                        },
                    ],
                },
                SignalDefinition {
                    name: "WorkspaceChanged".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "workspace_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "workspace_name".to_string(),
                            arg_type: "s".to_string(),
                        },
                        ArgumentDefinition {
                            name: "timestamp".to_string(),
                            arg_type: "t".to_string(),
                        },
                    ],
                },
                SignalDefinition {
                    name: "WindowMovedToWorkspace".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "window_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "from_workspace".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "to_workspace".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "timestamp".to_string(),
                            arg_type: "t".to_string(),
                        },
                    ],
                },
                SignalDefinition {
                    name: "WindowFocusChanged".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "window_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "workspace_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "timestamp".to_string(),
                            arg_type: "t".to_string(),
                        },
                    ],
                },
                SignalDefinition {
                    name: "WindowMinimized".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "window_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "minimized".to_string(),
                            arg_type: "b".to_string(),
                        },
                        ArgumentDefinition {
                            name: "timestamp".to_string(),
                            arg_type: "t".to_string(),
                        },
                    ],
                },
                SignalDefinition {
                    name: "WindowMaximized".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "window_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "maximized".to_string(),
                            arg_type: "b".to_string(),
                        },
                        ArgumentDefinition {
                            name: "timestamp".to_string(),
                            arg_type: "t".to_string(),
                        },
                    ],
                },
                SignalDefinition {
                    name: "WindowFullscreen".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "window_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "fullscreen".to_string(),
                            arg_type: "b".to_string(),
                        },
                        ArgumentDefinition {
                            name: "timestamp".to_string(),
                            arg_type: "t".to_string(),
                        },
                    ],
                },
                SignalDefinition {
                    name: "WorkspaceCreated".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "workspace_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "workspace_name".to_string(),
                            arg_type: "s".to_string(),
                        },
                        ArgumentDefinition {
                            name: "timestamp".to_string(),
                            arg_type: "t".to_string(),
                        },
                    ],
                },
                SignalDefinition {
                    name: "WorkspaceDeleted".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "workspace_id".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "timestamp".to_string(),
                            arg_type: "t".to_string(),
                        },
                    ],
                },
            ],
            properties: vec![],
        }
    }
}

impl Clone for GeckoEchoService {
    fn clone(&self) -> Self {
        GeckoEchoService {
            service_name: self.service_name.clone(),
            ready: Arc::clone(&self.ready),
            windows: Arc::clone(&self.windows),
            next_window_id: Arc::clone(&self.next_window_id),
            command_tx: self.command_tx.clone(),
            runtime_handle: self.runtime_handle.clone(),
            create_window_cb: Arc::clone(&self.create_window_cb),
            pending_window_actions: Arc::clone(&self.pending_window_actions),
            frame_count: Arc::clone(&self.frame_count),
            fps_window_start: Arc::clone(&self.fps_window_start),
            last_fps: Arc::clone(&self.last_fps),
        }
    }
}

impl Default for GeckoEchoService {
    fn default() -> Self {
        Self::new().expect("Failed to create default GeckoEchoService")
    }
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Real resident-set size of this process in MB, read from
/// `/proc/self/status`'s `VmRSS` line — the same value `ps`/`top` show.
/// Replaces a previously hardcoded `256` in `get_rendering_status`. Falls
/// back to 0 (rather than a fake constant) if `/proc` isn't readable, so a
/// genuine failure is visibly "0", not silently indistinguishable from a
/// real small-but-nonzero reading.
fn current_process_rss_mb() -> u32 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            if let Some(kb_str) = rest.trim().split_whitespace().next() {
                if let Ok(kb) = kb_str.parse::<u64>() {
                    return (kb / 1024) as u32;
                }
            }
        }
    }
    0
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_window_request() {
        let req = CreateWindowRequest::new("test.app", "native", 1280, 720);
        assert_eq!(req.app_id, "test.app");
        assert_eq!(req.app_type, "native");
        assert_eq!(req.width, 1280);
        assert_eq!(req.height, 720);
        assert!(req.url.is_none());
    }

    #[test]
    fn test_create_window_request_with_url() {
        let req = CreateWindowRequest::new("gaia.app", "gaia", 800, 600)
            .with_url("https://example.com");
        assert_eq!(req.url, Some("https://example.com".to_string()));
    }

    #[test]
    fn test_window_info_serialization() {
        let window = WindowInfo {
            window_id: 42,
            app_id: "test.app".to_string(),
            width: 1280,
            height: 720,
            x: 100,
            y: 50,
            focused: true,
            visible: true,
        };

        let json = serde_json::to_string(&window).unwrap();
        let deserialized: WindowInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.window_id, window.window_id);
        assert_eq!(deserialized.app_id, window.app_id);
    }
}