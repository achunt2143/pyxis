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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Ask the client to close, as the titlebar X does — `xdg_toplevel.close`,
    /// not a signal. The app gets to run its own teardown; `nova.Close`
    /// (which signals the process through AppHost) is the forceful path.
    Close,
    /// What the SSD titlebar's maximize button does. There was no IPC for
    /// this at all — `MaximizeWindow` fell through to "Method not
    /// implemented" — so neither the shell nor a test could maximize a
    /// window, only a human with a pointer. That made bug #7 and the
    /// kiosk-on-relaunch report impossible to reproduce headlessly.
    Maximize,
    Unmaximize,
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
            // `message`, not `error`: that is what Echo's own
            // Message::error writes (echo/src/message.rs) and what
            // IPCBus::call_method reads back. Evo was the odd one out, so
            // EVERY error it returned reached the caller as the useless
            // "Unknown error" — including diagnostics like "no mapped
            // compositor window yet", which is exactly the sort of message
            // that would have shortened the #10 hunt.
            body: json!({
                "message": error_message
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
    /// Emit a signal. Fire-and-forget: nothing waits on the reply, so a
    /// signal can never stall the compositor thread (see `emit`).
    EmitSignal { signal_name: String, args: Value },
}

/// Largest frame the broker will read. Anything larger means the stream is
/// out of step, or someone is misbehaving.
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// How long the compositor thread will wait on an Echo reply (`request`).
/// Before this, every call waited forever, on the thread that draws the
/// desktop. So an Echo daemon that never answered would have frozen the
/// screen, not just IPC.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// A request the broker sent and has not had the reply to yet, keyed in the
/// broker by its message_id. The reply is matched by `reply_to`, which is how
/// echo-daemon correlates the calls it forwards to Evo.
enum PendingReply {
    Call(tokio::sync::oneshot::Sender<Result<Value>>),
    /// `None` when the broker re-registers on its own, after a reconnect.
    Register(Option<tokio::sync::oneshot::Sender<Result<()>>>),
    Signal(String),
}

impl PendingReply {
    fn complete(self, reply: EchoMessage) {
        let ok = reply.header.message_type == "METHOD_RETURN";
        match self {
            PendingReply::Call(tx) => {
                let _ = tx.send(if ok {
                    Ok(reply.body)
                } else {
                    Err(anyhow!("Echo error: {:?}", reply.body))
                });
            }
            PendingReply::Register(tx) => {
                let result = if ok {
                    info!("✓ Service registered with Echo daemon");
                    Ok(())
                } else {
                    Err(anyhow!("Registration failed: {:?}", reply.body))
                };
                if let Some(tx) = tx {
                    let _ = tx.send(result);
                } else if let Err(e) = result {
                    error!("Re-registration after reconnect failed: {}", e);
                }
            }
            PendingReply::Signal(name) => {
                if ok {
                    debug!("✓ Signal emitted: {}", name);
                } else {
                    warn!("Signal {} failed: {:?}", name, reply.body);
                }
            }
        }
    }

    /// The connection went away with this request unanswered. The caller
    /// hears about it now, rather than never.
    fn fail(self) {
        let lost = || anyhow!("Echo connection lost before the reply");
        match self {
            PendingReply::Call(tx) => {
                let _ = tx.send(Err(lost()));
            }
            PendingReply::Register(Some(tx)) => {
                let _ = tx.send(Err(lost()));
            }
            PendingReply::Register(None) | PendingReply::Signal(_) => {}
        }
    }
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
    /// Every live toplevel, as the compositor last saw it: what
    /// `ListWindows` and `GetRenderingStatus` report. `windows` above holds
    /// only windows made through the create/claim contract, so a
    /// Nova-launched app is never in it. Answering from it made
    /// `ListWindows` empty on a real device, and froze each entry's
    /// geometry at creation. Written by `EvoState::sync_toplevels`.
    toplevels: Arc<RwLock<Vec<WindowInfo>>>,
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
    let toplevels = Arc::new(RwLock::new(Vec::new()));
    let next_window_id = Arc::new(RwLock::new(1));
    let pending_window_actions = Arc::new(RwLock::new(Vec::new()));
    let last_fps = Arc::new(AtomicU32::new(0));

    let (command_tx, command_rx) = mpsc::channel::<EchoCommand>(32);

    // Clone for broker
    let toplevels_clone = Arc::clone(&toplevels);
    let last_fps_clone = Arc::clone(&last_fps);
    let next_window_id_clone = Arc::clone(&next_window_id);
    let create_window_cb_clone = Arc::clone(&create_window_cb);
    let pending_window_actions_clone = Arc::clone(&pending_window_actions);

    let socket_path = Self::echo_socket_path();

    // Spawn broker with only the parts it needs
    runtime.spawn(async move {
        Self::echo_broker_loop(
            command_rx,
            socket_path,
            toplevels_clone,
            last_fps_clone,
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
        toplevels,
        next_window_id,
        command_tx,
        runtime_handle,
        create_window_cb,
        pending_window_actions,
        frame_count: Arc::new(AtomicU64::new(0)),
        fps_window_start: Arc::new(RwLock::new(Instant::now())),
        last_fps,
    })
}


    // ========================================================================
    // SYNC PUBLIC API (for use in Evo)
    // ========================================================================

    /// Send a request to the broker, and wait (at most REQUEST_TIMEOUT)
    /// for its reply. This runs on the compositor thread, so the wait is
    /// bounded.
    fn request<T>(
        &self,
        cmd: EchoCommand,
        reply: tokio::sync::oneshot::Receiver<Result<T>>,
        what: &str,
    ) -> Result<T> {
        self.runtime_handle.block_on(async {
            self.command_tx
                .send(cmd)
                .await
                .map_err(|e| anyhow!("Command channel error: {}", e))?;
            match tokio::time::timeout(REQUEST_TIMEOUT, reply).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(anyhow!("{} got no reply: the broker dropped it", what)),
                Err(_) => Err(anyhow!("{} timed out after {:?}", what, REQUEST_TIMEOUT)),
            }
        })
    }

    /// Queue a signal and return at once. A signal needs no answer, and it
    /// used to block the compositor thread on the broker's reply just the
    /// same. Every focus change emits one, and so every click did.
    fn emit(&self, signal_name: &str, args: Value) -> Result<()> {
        self.command_tx
            .try_send(EchoCommand::EmitSignal {
                signal_name: signal_name.to_string(),
                args,
            })
            .map_err(|e| anyhow!("Signal {} not queued: {}", signal_name, e))
    }

    /// Register service with Echo bus (SYNC). The broker remembers the
    /// interface and registers it again after any reconnect.
    pub fn register(&self) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let interface = self.create_service_interface();
        self.request(EchoCommand::Register { interface, reply: tx }, rx, "RegisterService")
    }

    /// Signal that Gecko is ready (SYNC)
    pub fn signal_ready(&self) -> Result<()> {
        *self.ready.write() = true;
        self.emit("GeckoReady", json!({}))
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
        RenderingStatus {
            is_ready: *self.ready.read(),
            active_windows: self.toplevels.read().len(),
            fps: self.last_fps.load(Ordering::Relaxed),
            memory_mb: current_process_rss_mb(),
        }
    }

    /// Replace the live-toplevel snapshot (see `toplevels`). It takes the
    /// write lock only when something changed, so calling it on a timer
    /// from the render loop costs a read lock when the desktop is idle.
    pub fn set_toplevels(&self, toplevels: Vec<WindowInfo>) {
        if *self.toplevels.read() != toplevels {
            *self.toplevels.write() = toplevels;
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
        self.emit("WindowCreated", json!({
                "window_id": window_id,
                "app_id": app_id,
                "width": width,
                "height": height
            }))
    }

    /// Bind a mapped Wayland surface to its AppHost session, **over IPC**.
    ///
    /// This has to be a real method call to the daemon, not a call on
    /// `geckoOS_echo::AppHost::global()`. `AppHost::global()` is a
    /// `OnceLock` *process*-global (services/apphost.rs), and Evo is a
    /// separate process from `echo-daemon` — so calling it here operated on a
    /// fresh, empty AppHost living inside the compositor, which no one else
    /// can see and which was never `set_bus`'d (so it emitted no signals
    /// either). The daemon's real session therefore never learned about its
    /// window.
    ///
    /// That single mistake produced the whole of alpha2 #10: the session kept
    /// `surfaces: []`, `appHost.ListSurfaceSessions` stayed empty, the shell
    /// never received `AppSurfaceMapped`, and `nova.Focus` — which does
    /// `session.surfaces.first()` — answered "app has no mapped compositor
    /// window yet", so restore-from-dock could never work.
    pub fn bind_surface_remote(&self, app_id: &str, surface_id: u32) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::Call {
            message: EchoMessage::method_call(
                "org.geckoOS.appHost".to_string(),
                "org.geckoOS.appHost".to_string(),
                "BindSurface".to_string(),
                json!({ "appId": app_id, "surfaceId": surface_id }),
            ),
            reply: tx,
        };

        self.request(cmd, rx, "BindSurface").map(|_| ())
    }

    /// Detach a surface from its AppHost session. See `bind_surface_remote`
    /// for why this must not touch `AppHost::global()`.
    pub fn unbind_surface_remote(&self, surface_id: u32) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = EchoCommand::Call {
            message: EchoMessage::method_call(
                "org.geckoOS.appHost".to_string(),
                "org.geckoOS.appHost".to_string(),
                "UnbindSurface".to_string(),
                json!({ "surfaceId": surface_id }),
            ),
            reply: tx,
        };

        self.request(cmd, rx, "UnbindSurface").map(|_| ())
    }

    /// Emit window closed signal (SYNC)
    pub fn emit_window_closed(&self, window_id: u32) -> Result<()> {
        self.emit("WindowClosed", json!({ "window_id": window_id }))
    }

    /// Emit window moved signal (SYNC)
    pub fn emit_window_moved(
        &self,
        window_id: u32,
        x: i32,
        y: i32,
    ) -> Result<()> {
        self.emit("WindowMoved", json!({
                "window_id": window_id,
                "x": x,
                "y": y,
                "timestamp": now_ms()
            }))
    }

    /// Emit workspace changed signal (SYNC)
    pub fn emit_workspace_changed(&self, workspace_id: u32, workspace_name: &str) -> Result<()> {
        self.emit("WorkspaceChanged", json!({
                "workspace_id": workspace_id,
                "workspace_name": workspace_name,
                "timestamp": now_ms()
            }))
    }

    /// Emit window moved to workspace signal (SYNC)
    pub fn emit_window_moved_to_workspace(
        &self,
        window_id: u32,
        from_workspace: u32,
        to_workspace: u32,
    ) -> Result<()> {
        self.emit("WindowMovedToWorkspace", json!({
                "window_id": window_id,
                "from_workspace": from_workspace,
                "to_workspace": to_workspace,
                "timestamp": now_ms()
            }))
    }

    /// Emit window focus changed signal (SYNC)
    pub fn emit_window_focus_changed(
        &self,
        window_id: Option<u32>,
        workspace_id: u32,
    ) -> Result<()> {
        self.emit("WindowFocusChanged", json!({
                "window_id": window_id,
                "workspace_id": workspace_id,
                "timestamp": now_ms()
            }))
    }

    /// Emit window minimized signal (SYNC)
    pub fn emit_window_minimized(&self, window_id: u32, minimized: bool) -> Result<()> {
        self.emit("WindowMinimized", json!({
                "window_id": window_id,
                "minimized": minimized,
                "timestamp": now_ms()
            }))
    }

    /// Emit window maximized signal (SYNC)
    pub fn emit_window_maximized(&self, window_id: u32, maximized: bool) -> Result<()> {
        self.emit("WindowMaximized", json!({
                "window_id": window_id,
                "maximized": maximized,
                "timestamp": now_ms()
            }))
    }

    /// Emit window fullscreen signal (SYNC)
    pub fn emit_window_fullscreen(&self, window_id: u32, fullscreen: bool) -> Result<()> {
        self.emit("WindowFullscreen", json!({
                "window_id": window_id,
                "fullscreen": fullscreen,
                "timestamp": now_ms()
            }))
    }

    /// Emit workspace created signal (SYNC)
    pub fn emit_workspace_created(&self, workspace_id: u32, workspace_name: &str) -> Result<()> {
        self.emit("WorkspaceCreated", json!({
                "workspace_id": workspace_id,
                "workspace_name": workspace_name,
                "timestamp": now_ms()
            }))
    }

    /// Emit workspace deleted signal (SYNC)
    pub fn emit_workspace_deleted(&self, workspace_id: u32) -> Result<()> {
        self.emit("WorkspaceDeleted", json!({
                "workspace_id": workspace_id,
                "timestamp": now_ms()
            }))
    }

    /// Drain and return all pending FocusWindow/MinimizeWindow/RestoreWindow
    /// requests queued since the last call (SYNC). Meant to be polled once
    /// Validate the `window_id` argument of a window action.
    ///
    /// These three methods used to do `.unwrap_or(0)` and answer
    /// `{"success": true, "window_id": 0}` regardless, then log "Ignoring
    /// Minimize for unknown window 0" a moment later where no caller would
    /// ever see it. A caller passing `appId` instead — the natural mistake,
    /// since every *Nova* method is keyed by app id — got a success reply
    /// for a no-op. An alpha 4 test did exactly that and "passed" while
    /// exercising nothing. Reporting success for work that will not happen
    /// is the failure mode alpha 3 was about.
    ///
    /// **This validates the argument, not the window.** It deliberately does
    /// not check that the id names a live window, because the map reachable
    /// from here is the wrong one: `EchoIpcService::windows` is populated
    /// only by `track_window`, i.e. windows Evo *creates* through the
    /// two-step create/claim contract. Ordinary client toplevels — every
    /// Nova-launched app — live in `EvoState::windows_by_id` over in
    /// `state.rs`, which this async broker task cannot reach. Checking the
    /// wrong map rejected every real window and broke `nova.Focus`
    /// outright (caught on-device 2026-09-20, fixed by deleting the check).
    ///
    /// Existence is therefore still resolved where the action is applied, in
    /// `process_pending_window_actions`. Surfacing *that* outcome in the
    /// reply would mean plumbing a response channel back through the queue —
    /// worth doing, but a bigger change than this.
    fn resolve_window_arg(msg: &EchoMessage) -> std::result::Result<u32, String> {
        let raw = msg.body.get("window_id").ok_or_else(|| {
            // Name the likely mistake rather than just the missing key.
            let got: Vec<&str> = msg.body.as_object()
                .map(|o| o.keys().map(|k| k.as_str()).collect())
                .unwrap_or_default();
            format!(
                "missing required argument 'window_id' (got: [{}]). Note this \
                 method is keyed by compositor window id, not app id — use \
                 nova.Focus to act on an app.",
                got.join(", ")
            )
        })?;

        let window_id = raw
            .as_u64()
            .ok_or_else(|| format!("'window_id' must be a number, got {}", raw))?
            as u32;

        Ok(window_id)
    }

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
    toplevels: Arc<RwLock<Vec<WindowInfo>>>,
    last_fps: Arc<AtomicU32>,
    next_window_id: Arc<RwLock<u32>>,
    // ✅ Arc instead of Box
    create_window_cb: Arc<RwLock<Option<Arc<dyn Fn(&CreateWindowRequest) -> Result<u32> + Send + Sync>>>>,
    pending_window_actions: Arc<RwLock<Vec<PendingWindowAction>>>,
) {
    // The interface last registered, replayed after every reconnect.
    // echo-daemon drops a connection's service names when the connection
    // closes (IPCBus::remove_connection). A broker that reconnected without
    // re-registering therefore left `org.geckoOS.evo` gone for the rest of
    // the session, and every nova.Focus and window action failed with
    // "Service not registered".
    let mut registration: Option<ServiceInterface> = None;
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

                // One reader for everything that arrives. A reply is matched
                // to its request by `reply_to`, the way echo-daemon matches
                // the calls it forwards to us. This used to send a request and
                // then take the next frame as its answer. But the next frame
                // can be a broadcast signal, or a call the daemon is
                // forwarding to Evo (someone's `ListWindows`). That call was
                // then eaten as the "reply" and never answered, and the real
                // reply turned up later as a stray METHOD_RETURN.
                let mut pending: HashMap<String, PendingReply> = HashMap::new();

                if let Some(interface) = &registration {
                    info!("Re-registering with Echo daemon after reconnect");
                    let message = Self::register_message(interface);
                    let id = message.header.message_id.clone();
                    if Self::send_echo_message(&mut write_half, &message).await.is_ok() {
                        pending.insert(id, PendingReply::Register(None));
                    }
                }

                loop {
                    tokio::select! {
                        incoming = Self::recv_frame(&mut read_half) => {
                            let msg = match incoming {
                                Ok(Some(msg)) => msg,
                                // A frame that did not parse. It was read in
                                // full, so the stream is still in step: skip it.
                                Ok(None) => continue,
                                Err(e) => {
                                    error!("Error receiving message: {}", e);
                                    break;
                                }
                            };
                            if matches!(msg.header.message_type.as_str(), "METHOD_RETURN" | "ERROR") {
                                let reply_to = msg.header.reply_to.clone().unwrap_or_default();
                                match pending.remove(&reply_to) {
                                    Some(waiting) => waiting.complete(msg),
                                    None => debug!("Reply to nothing we sent: {}", reply_to),
                                }
                                continue;
                            }
                            if let Err(e) = Self::handle_incoming_message(
                                &mut write_half,
                                msg,
                                &toplevels,
                                &last_fps,
                                &next_window_id,
                                &create_window_cb,
                                &pending_window_actions,
                            )
                            .await
                            {
                                error!("Error handling incoming message: {}", e);
                            }
                        }

                        cmd = command_rx.recv() => {
                            match cmd {
                                Some(cmd) => {
                                    if let Err(e) = Self::send_command(&mut write_half, cmd, &mut pending, &mut registration).await {
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

                // Everyone still waiting on this connection hears so now.
                for (_, waiting) in pending.drain() {
                    waiting.fail();
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
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
    toplevels: &Arc<RwLock<Vec<WindowInfo>>>,
    last_fps: &Arc<AtomicU32>,
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
                let windows_list = toplevels.read();
                let window_list: Vec<Value> = windows_list
                    .iter()
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
                // These were literals, `"fps": 60` and `"memory_mb": 256`,
                // while the signal path (`get_rendering_status`) read the
                // real counters. Both answer from the same place now.
                let status = json!({
                    "is_ready": true,
                    "active_windows": toplevels.read().len(),
                    "fps": last_fps.load(Ordering::Relaxed),
                    "memory_mb": current_process_rss_mb(),
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

            // Was a stub: it removed the id from `EchoIpcService::windows`
            // and reported success from whether that map happened to hold
            // it. That map only ever contains windows Evo *created* via the
            // two-step create/claim contract (see resolve_window_arg), so
            // for an ordinary app window it removed nothing, answered
            // `{"success": false}`, and — the actual problem — never told
            // the client anything. There was no way to close an app window
            // from the shell at all; only nova.Close, which signals the
            // process rather than asking the window to go away.
            //
            // Now queued like every other window action, and applied in
            // EvoState::process_pending_window_actions against
            // `windows_by_id` (the map that has real toplevels), where it
            // sends xdg_toplevel.close — the same thing the titlebar X does.
            "CloseWindow" => {
                match Self::resolve_window_arg(&msg) {
                    Ok(window_id) => {
                        pending_window_actions.write().push(PendingWindowAction {
                            window_id,
                            action: WindowAction::Close,
                        });
                        info!("Queued CloseWindow request for window {}", window_id);
                        EchoMessage::method_return(
                            message_id,
                            json!({ "success": true, "window_id": window_id }),
                        )
                    }
                    Err(e) => {
                        warn!("CloseWindow: {}", e);
                        EchoMessage::method_error(message_id, &e)
                    }
                }
            }

            "MaximizeWindow" | "UnmaximizeWindow" => {
                let action = if method == "MaximizeWindow" {
                    WindowAction::Maximize
                } else {
                    WindowAction::Unmaximize
                };
                match Self::resolve_window_arg(&msg) {
                    Ok(window_id) => {
                        pending_window_actions.write().push(PendingWindowAction {
                            window_id,
                            action,
                        });
                        info!("Queued {} request for window {}", method, window_id);
                        EchoMessage::method_return(
                            message_id,
                            json!({ "success": true, "window_id": window_id }),
                        )
                    }
                    Err(e) => {
                        warn!("{}: {}", method, e);
                        EchoMessage::method_error(message_id, &e)
                    }
                }
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
                match Self::resolve_window_arg(&msg) {
                    Ok(window_id) => {
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
                    Err(e) => {
                        warn!("FocusWindow: {}", e);
                        EchoMessage::method_error(message_id, &e)
                    }
                }
            }

            "MinimizeWindow" => {
                match Self::resolve_window_arg(&msg) {
                    Ok(window_id) => {
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
                    Err(e) => {
                        warn!("MinimizeWindow: {}", e);
                        EchoMessage::method_error(message_id, &e)
                    }
                }
            }

            "RestoreWindow" => {
                match Self::resolve_window_arg(&msg) {
                    Ok(window_id) => {
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
                    Err(e) => {
                        warn!("RestoreWindow: {}", e);
                        EchoMessage::method_error(message_id, &e)
                    }
                }
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

    /// Write a command's message and note what is waiting on the reply.
    /// Nothing is read here: the broker loop is the only reader.
    async fn send_command(
        write_half: &mut tokio::net::unix::OwnedWriteHalf,
        cmd: EchoCommand,
        pending: &mut HashMap<String, PendingReply>,
        registration: &mut Option<ServiceInterface>,
    ) -> Result<()> {
        let (message, waiting) = match cmd {
            EchoCommand::Call { message, reply } => (message, PendingReply::Call(reply)),
            EchoCommand::Register { interface, reply } => {
                let message = Self::register_message(&interface);
                *registration = Some(interface);
                (message, PendingReply::Register(Some(reply)))
            }
            EchoCommand::EmitSignal { signal_name, args } => (
                EchoMessage::method_call(
                    "org.geckoOS.Echo".to_string(),
                    "org.geckoOS.evo".to_string(),
                    "EmitSignal".to_string(),
                    json!({
                        "signal_name": signal_name,
                        "args": args
                    }),
                ),
                PendingReply::Signal(signal_name),
            ),
        };
        let id = message.header.message_id.clone();
        Self::send_echo_message(write_half, &message).await?;
        pending.insert(id, waiting);
        Ok(())
    }

    fn register_message(interface: &ServiceInterface) -> EchoMessage {
        EchoMessage::method_call(
            "org.geckoOS.Echo".to_string(),
            "org.geckoOS.Echo".to_string(),
            "RegisterService".to_string(),
            json!({
                "name": "org.geckoOS.evo",
                "interface": serde_json::to_value(interface).unwrap_or_default()
            }),
        )
    }

    /// One frame off the wire. `Ok(None)` for a frame that arrived whole but
    /// did not parse. It is logged and skipped, since the stream is still in
    /// step, where it used to drop the whole connection. Only a failed read
    /// is an error.
    async fn recv_frame(read_half: &mut tokio::net::unix::OwnedReadHalf) -> Result<Option<EchoMessage>> {
        let len = read_half.read_u32_le().await? as usize;
        if len > MAX_FRAME {
            return Err(anyhow!("Message too large: {} bytes", len));
        }
        let mut buffer = vec![0u8; len];
        read_half.read_exact(&mut buffer).await?;
        debug!("Received Echo message: {} bytes", len);
        match serde_json::from_slice(&buffer) {
            Ok(msg) => Ok(Some(msg)),
            Err(e) => {
                warn!(
                    "Skipping an Echo message that did not parse ({}): {}",
                    e,
                    String::from_utf8_lossy(&buffer[..buffer.len().min(200)])
                );
                Ok(None)
            }
        }
    }

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
        if len > MAX_FRAME {
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
            toplevels: Arc::clone(&self.toplevels),
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

    // ---- the broker against a scripted daemon ----

    async fn write_frame(stream: &mut UnixStream, value: &Value) {
        let data = serde_json::to_vec(value).unwrap();
        stream.write_u32_le(data.len() as u32).await.unwrap();
        stream.write_all(&data).await.unwrap();
    }

    async fn read_frame(stream: &mut UnixStream) -> Value {
        let len = stream.read_u32_le().await.unwrap() as usize;
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).await.unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    fn frame(message_type: &str, fields: Value) -> Value {
        let mut header = json!({
            "message_id": Uuid::new_v4().to_string(),
            "message_type": message_type,
            "destination": "",
            "interface": "",
            "method": "",
            "timestamp": 0,
        });
        for (k, v) in fields.as_object().unwrap() {
            header[k] = v.clone();
        }
        json!({ "header": header, "body": {} })
    }

    /// The failure that cost a device run on 2026-09-25. The broker used to
    /// take the next frame after a request as that request's reply, and it
    /// dropped the connection on any frame it could not parse. It also
    /// never re-registered after reconnecting. So a daemon that interleaved
    /// traffic, which is exactly what a busy bus does, left Evo's calls
    /// unanswered and, sooner or later, `org.geckoOS.evo` unregistered.
    #[tokio::test]
    async fn broker_matches_replies_and_survives_a_busy_bus() {
        // A regression here shows up as a hang (a reply that never comes),
        // so bound it and fail loudly instead.
        tokio::time::timeout(std::time::Duration::from_secs(10), busy_bus())
            .await
            .expect("broker stopped answering: see busy_bus");
    }

    async fn busy_bus() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("echo.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();

        let (command_tx, command_rx) = mpsc::channel(8);
        tokio::spawn(GeckoEchoService::echo_broker_loop(
            command_rx,
            path.clone(),
            Arc::new(RwLock::new(Vec::new())),
            Arc::new(AtomicU32::new(0)),
            Arc::new(RwLock::new(1)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(Vec::new())),
        ));

        let (mut daemon, _) = listener.accept().await.unwrap();
        let hello = read_frame(&mut daemon).await;
        assert_eq!(hello["header"]["message_type"], "HANDSHAKE");
        write_frame(&mut daemon, &frame("METHOD_RETURN", json!({}))).await;

        let interface: ServiceInterface = serde_json::from_value(json!({
            "name": "org.geckoOS.evo", "methods": [], "signals": [], "properties": []
        }))
        .unwrap();
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        command_tx
            .send(EchoCommand::Register { interface, reply: reply_tx })
            .await
            .unwrap();
        let register = read_frame(&mut daemon).await;
        assert_eq!(register["header"]["method"], "RegisterService");
        let register_id = register["header"]["message_id"].clone();

        // Everything a busy bus can put in front of the reply.
        write_frame(&mut daemon, &frame("SIGNAL", json!({ "interface": "org.geckoOS.nova", "method": "RegistryChanged" }))).await;
        daemon.write_u32_le(9).await.unwrap();
        daemon.write_all(b"not json!").await.unwrap();
        let list = frame("METHOD_CALL", json!({ "destination": "org.geckoOS.evo", "interface": "org.geckoOS.evo", "method": "ListWindows" }));
        let list_id = list["header"]["message_id"].clone();
        write_frame(&mut daemon, &list).await;
        write_frame(&mut daemon, &frame("METHOD_RETURN", json!({ "reply_to": register_id }))).await;

        // The forwarded call is answered, not eaten as the reply...
        let answer = read_frame(&mut daemon).await;
        assert_eq!(answer["header"]["reply_to"], list_id);
        assert_eq!(answer["body"]["windows"], json!([]));
        // ...and the registration got its own reply.
        reply_rx.await.unwrap().unwrap();

        // Drop the connection. The broker must come back and register again
        // without being asked.
        drop(daemon);
        let (mut daemon, _) = listener.accept().await.unwrap();
        assert_eq!(read_frame(&mut daemon).await["header"]["message_type"], "HANDSHAKE");
        write_frame(&mut daemon, &frame("METHOD_RETURN", json!({}))).await;
        let again = read_frame(&mut daemon).await;
        assert_eq!(again["header"]["method"], "RegisterService");
        assert_eq!(again["body"]["name"], "org.geckoOS.evo");
    }

    #[test]
    fn a_lost_connection_fails_the_waiting_call_instead_of_leaving_it() {
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        PendingReply::Call(tx).fail();
        assert!(rx.try_recv().unwrap().is_err());
    }

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