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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
    /// Bloom's `SetWorkArea`: not about any window (`window_id` is 0). The
    /// edges apps must leave to the shell, see `shell/work_area.rs`.
    SetWorkArea(crate::shell::work_area::Insets),
    /// Bloom's `SetOverlayFocus` (`window_id` is 0): give the overlay the
    /// keyboard, or take it back.
    OverlayKeyboard(bool),
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
    /// A method call nobody waits on. The broker logs how it turned out
    /// (see `notify`).
    Notify { message: EchoMessage, what: String },
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
    /// A call made with `notify`: the failure is reported here, since the
    /// caller has gone.
    Notify(String),
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
            PendingReply::Notify(what) => {
                if ok {
                    debug!("✓ {} done", what);
                } else {
                    warn!("{} failed: {:?}", what, reply.body);
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
            PendingReply::Notify(what) => warn!("{} lost: Echo connection dropped first", what),
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
    /// Every live toplevel, as the compositor last saw it: what
    /// `ListWindows` and `GetRenderingStatus` report. Written by
    /// `EvoState::sync_toplevels`.
    toplevels: Arc<RwLock<Vec<WindowInfo>>>,
    pub command_tx: mpsc::Sender<EchoCommand>,
    pub runtime_handle: tokio::runtime::Handle,

    /// Queue of FocusWindow/MinimizeWindow/RestoreWindow requests received
    /// over Echo IPC, awaiting pickup by `EvoState::process_pending_window_actions`.
    pending_window_actions: Arc<RwLock<Vec<PendingWindowAction>>>,

    /// Raised by the broker each time it has reconnected to echo-daemon and
    /// re-registered. A restarted daemon's appHost has no bindings, so the
    /// compositor thread replays them: see `take_rebind_request`.
    rebind_requested: Arc<AtomicBool>,

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
    let toplevels = Arc::new(RwLock::new(Vec::new()));
    let pending_window_actions = Arc::new(RwLock::new(Vec::new()));
    let last_fps = Arc::new(AtomicU32::new(0));

    let (command_tx, command_rx) = mpsc::channel::<EchoCommand>(32);

    // Clone for broker
    let toplevels_clone = Arc::clone(&toplevels);
    let last_fps_clone = Arc::clone(&last_fps);
    let pending_window_actions_clone = Arc::clone(&pending_window_actions);
    let rebind_requested = Arc::new(AtomicBool::new(false));
    let rebind_clone = Arc::clone(&rebind_requested);

    let socket_path = Self::echo_socket_path();

    // Spawn broker with only the parts it needs
    runtime.spawn(async move {
        Self::echo_broker_loop(
            command_rx,
            socket_path,
            toplevels_clone,
            last_fps_clone,
            pending_window_actions_clone,
            rebind_clone,
        )
        .await;
    });

    std::mem::forget(runtime);

    info!("✓ Echo broker spawned in background runtime");

    Ok(GeckoEchoService {
        service_name: "org.geckoOS.evo".to_string(),
        ready,
        toplevels,
        command_tx,
        runtime_handle,
        pending_window_actions,
        rebind_requested,
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

    /// Queue a method call and return at once. Same as `emit` but for a call
    /// to another service: the broker logs the outcome, nothing waits for it.
    /// Calls queue in order with the signals, so a `BindSurface` still lands
    /// before the `UnbindSurface` that follows it.
    fn notify(&self, message: EchoMessage, what: &str) -> Result<()> {
        self.command_tx
            .try_send(EchoCommand::Notify {
                message,
                what: what.to_string(),
            })
            .map_err(|e| anyhow!("{} not queued: {}", what, e))
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

    /// Whether the broker reconnected since the last call, so appHost's
    /// bindings need replaying. Polled by `EvoState::process_pending_window_actions`.
    pub fn take_rebind_request(&self) -> bool {
        self.rebind_requested.swap(false, Ordering::AcqRel)
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
    ///
    /// Fire-and-forget (`notify`). This used to wait for the daemon's reply on
    /// the compositor thread, up to 3 s, and `ack_configure` called it on
    /// every configure ack. The caller now binds once per window, when its app
    /// id becomes known or changes, and a failure is logged by the broker.
    ///
    /// `pid` is the client's, from the Wayland socket credentials. appHost
    /// matches a surface to the session it launched by it.
    pub fn bind_surface_remote(&self, app_id: &str, surface_id: u32, pid: Option<u32>) -> Result<()> {
        self.notify(
            EchoMessage::method_call(
                "org.geckoOS.appHost".to_string(),
                "org.geckoOS.appHost".to_string(),
                "BindSurface".to_string(),
                bind_surface_body(app_id, surface_id, pid),
            ),
            "BindSurface",
        )
    }

    /// Detach a surface from its AppHost session. See `bind_surface_remote`
    /// for why this must not touch `AppHost::global()`, and why it does not
    /// wait.
    pub fn unbind_surface_remote(&self, surface_id: u32) -> Result<()> {
        self.notify(
            EchoMessage::method_call(
                "org.geckoOS.appHost".to_string(),
                "org.geckoOS.appHost".to_string(),
                "UnbindSurface".to_string(),
                json!({ "surfaceId": surface_id }),
            ),
            "UnbindSurface",
        )
    }

    /// A global shortcut the shell should act on: `launcher`, `overview`,
    /// `search` or `show-desktop`. Evo sees the keys whichever app has the
    /// keyboard, which Bloom's own hotkeys never do.
    pub fn emit_shortcut_triggered(&self, action: &str) -> Result<()> {
        self.emit("ShortcutTriggered", json!({
                "action": action,
                "timestamp": now_ms()
            }))
    }

    /// Alt+Tab, for the shell to draw: `phase` is `open`, `step`, `commit` or
    /// `cancel`, `selected` the window the walk is on and `order` every
    /// switchable window, most recently used first (shell/switcher.rs).
    pub fn emit_switcher_changed(&self, phase: &str, selected: u32, order: &[u32]) -> Result<()> {
        self.emit("SwitcherChanged", json!({
                "phase": phase,
                "selected": selected,
                "order": order,
                "timestamp": now_ms()
            }))
    }

    /// A minimize's orb reached its dock icon (`phase` `land`, at 90% of the
    /// run) or a restore left it (`rise`, as it starts): Bloom ripples the
    /// icon (shell/minimize.rs).
    pub fn emit_minimize_landed(&self, app_id: &str, window_id: u32, phase: &str) -> Result<()> {
        self.emit("MinimizeLanded", json!({
                "app_id": app_id,
                "window_id": window_id,
                "phase": phase,
                "timestamp": now_ms()
            }))
    }

    /// The overlay gained or lost the keyboard. It loses it when Bloom gives
    /// it back, and also when anything else takes it (an Alt+Tab, a dock
    /// click on an app), which is how Bloom learns to close its panels.
    pub fn emit_overlay_focus_changed(&self, focused: bool) -> Result<()> {
        self.emit("OverlayFocusChanged", json!({
                "focused": focused,
                "timestamp": now_ms()
            }))
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
    /// from here is the wrong one: the broker task has no window map at
    /// all. Client toplevels — every Nova-launched app — live in
    /// `EvoState::windows_by_id` over in `state.rs`, which this async broker
    /// task cannot reach. Checking the
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

    // ========================================================================
    // ASYNC BROKER LOOP (runs in background runtime)
    // ========================================================================

    async fn echo_broker_loop(
    mut command_rx: mpsc::Receiver<EchoCommand>,
    socket_path: PathBuf,
    toplevels: Arc<RwLock<Vec<WindowInfo>>>,
    last_fps: Arc<AtomicU32>,
    pending_window_actions: Arc<RwLock<Vec<PendingWindowAction>>>,
    rebind_requested: Arc<AtomicBool>,
) {
    let mut connected_before = false;
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

                // A connection after the first means echo-daemon restarted
                // (or dropped us), and appHost's surface bindings went with
                // it. The broker keeps no window list: it asks the
                // compositor thread, which has the real one.
                if connected_before {
                    rebind_requested.store(true, Ordering::Release);
                }
                connected_before = true;

                // Frames are read in a task of their own and the loop waits on
                // its channel. Reading inside the `select!` was not
                // cancel-safe: when a command (every signal Evo emits is one)
                // won the select part-way through a frame, the bytes read so
                // far were lost and the stream fell out of step. The task
                // ends with this connection (aborted on drop).
                let (frames_tx, mut frames_rx) = tokio::sync::mpsc::channel::<Result<Option<EchoMessage>>>(32);
                let reader = tokio::spawn(async move {
                    loop {
                        let frame = Self::recv_frame(&mut read_half).await;
                        let failed = frame.is_err();
                        if frames_tx.send(frame).await.is_err() || failed {
                            break;
                        }
                    }
                });
                struct AbortOnDrop(tokio::task::JoinHandle<()>);
                impl Drop for AbortOnDrop {
                    fn drop(&mut self) {
                        self.0.abort();
                    }
                }
                let _reader = AbortOnDrop(reader);

                loop {
                    tokio::select! {
                        Some(incoming) = frames_rx.recv() => {
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

            // Was a stub that answered from a map holding only windows Evo
            // itself created, so an ordinary app window could not be closed
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

            // Where Bloom's overlay window currently draws, and so takes
            // input (see shell/overlay.rs). Bloom sends it whenever its toast
            // stack changes. Every other point of the overlay passes clicks
            // through to the window beneath.
            "SetOverlayRegion" => {
                let rects = msg.body.get("rects").cloned().unwrap_or_else(|| json!([]));
                match crate::shell::overlay::parse_rects(&rects) {
                    Ok(rects) => {
                        let count = rects.len();
                        crate::shell::overlay::set_region(rects);
                        debug!("Overlay input region: {} rect(s)", count);
                        EchoMessage::method_return(
                            message_id,
                            json!({ "success": true, "rects": count }),
                        )
                    }
                    Err(e) => {
                        warn!("SetOverlayRegion: {}", e);
                        EchoMessage::method_error(message_id, &e)
                    }
                }
            }

            // The edges of the output the shell occupies (top bar, dock).
            // Apps are kept out of them: maximize, first placement and the
            // move/resize clamp all use the output minus these. Applied on
            // the compositor thread, which also re-fits open windows.
            "SetWorkArea" => match crate::shell::work_area::parse_insets(&msg.body) {
                Ok(insets) => {
                    pending_window_actions.write().push(PendingWindowAction {
                        window_id: 0,
                        action: WindowAction::SetWorkArea(insets),
                    });
                    EchoMessage::method_return(
                        message_id,
                        json!({
                            "success": true,
                            "top": insets.top,
                            "bottom": insets.bottom,
                            "left": insets.left,
                            "right": insets.right,
                        }),
                    )
                }
                Err(e) => {
                    warn!("SetWorkArea: {}", e);
                    EchoMessage::method_error(message_id, &e)
                }
            },

            // Where each running app's dock icon is (output-logical pixels,
            // from the output's origin), so a window can minimize into it and
            // restore out of it (shell/minimize.rs). Bloom's dock sends the
            // whole table whenever its icons move; it replaces the last one.
            // An app with no entry minimizes toward the bottom centre. The
            // table is only read when a window minimizes, so it is set here
            // like the overlay's region rather than queued.
            // Each entry may carry the app's `accent` ("#rrggbb"), the colour
            // of its Lumen Droplet; a bad or missing one falls back to the
            // window's own average colour.
            "SetMinimizeTargets" => match crate::shell::minimize::parse_targets(&msg.body) {
                Ok(targets) => {
                    let count = targets.len();
                    crate::shell::minimize::set_targets(targets);
                    crate::shell::minimize::set_accents(crate::shell::minimize::parse_accents(&msg.body));
                    debug!("Minimize targets: {} app(s)", count);
                    EchoMessage::method_return(message_id, json!({ "ok": true, "targets": count }))
                }
                Err(e) => {
                    warn!("SetMinimizeTargets: {}", e);
                    EchoMessage::method_error(message_id, &e)
                }
            },

            // The only way the overlay gets the keyboard, or gives it back
            // (shell/overlay.rs). Panels that take typing ask for it while
            // open. `OverlayFocusChanged` reports what actually happened.
            "SetOverlayFocus" => match msg.body.get("focused").and_then(Value::as_bool) {
                Some(focused) => {
                    pending_window_actions.write().push(PendingWindowAction {
                        window_id: 0,
                        action: WindowAction::OverlayKeyboard(focused),
                    });
                    EchoMessage::method_return(message_id, json!({ "success": true, "focused": focused }))
                }
                None => {
                    warn!("SetOverlayFocus: focused must be a boolean");
                    EchoMessage::method_error(message_id, "focused must be a boolean")
                }
            },

            // What the overlay currently has: the keyboard, the input
            // region and the reserved edges. Read-only, for the shell and
            // for on-device tests.
            "GetOverlayState" => {
                let insets = crate::shell::work_area::insets();
                let rects: Vec<Value> = crate::shell::overlay::region()
                    .iter()
                    .map(|r| json!({ "x": r.loc.x, "y": r.loc.y, "width": r.size.w, "height": r.size.h }))
                    .collect();
                EchoMessage::method_return(
                    message_id,
                    json!({
                        "keyboard": crate::shell::overlay::keyboard_held(),
                        "rects": rects,
                        "workArea": {
                            "top": insets.top,
                            "bottom": insets.bottom,
                            "left": insets.left,
                            "right": insets.right,
                        },
                    }),
                )
            }

            // A picture of each window for Bloom's overview and switcher
            // (shell/thumbnails.rs). Drawn on the compositor thread at card
            // size, encoded here on a blocking worker, cached per window.
            // A window with nothing drawn yet is left out, so the reply has
            // at most one entry per window asked for.
            "GetWindowThumbnails" => match crate::shell::thumbnails::parse_request(&msg.body) {
                Ok((ids, bounds)) => {
                    let started = std::time::Instant::now();
                    let rx = crate::shell::thumbnails::submit(ids, bounds);
                    match tokio::time::timeout(std::time::Duration::from_secs(3), rx).await {
                        Ok(Ok(slots)) => {
                            let jobs: Vec<_> = slots
                                .into_iter()
                                .map(|slot| match slot {
                                    crate::shell::thumbnails::Slot::Ready(id, t) => {
                                        tokio::task::spawn_blocking(move || Ok((id, t)))
                                    }
                                    crate::shell::thumbnails::Slot::Raw(raw) => {
                                        tokio::task::spawn_blocking(move || crate::shell::thumbnails::encode(raw))
                                    }
                                })
                                .collect();
                            let mut thumbnails = Vec::new();
                            for job in jobs {
                                match job.await {
                                    Ok(Ok((id, t))) => thumbnails.push(json!({
                                        "window_id": id,
                                        "width": t.width,
                                        "height": t.height,
                                        "data": t.data,
                                    })),
                                    Ok(Err(e)) => warn!("GetWindowThumbnails: {}", e),
                                    Err(e) => warn!("GetWindowThumbnails: encoder died: {}", e),
                                }
                            }
                            debug!(
                                "GetWindowThumbnails: {} in {} ms",
                                thumbnails.len(),
                                started.elapsed().as_millis()
                            );
                            EchoMessage::method_return(message_id, json!({ "thumbnails": thumbnails }))
                        }
                        _ => EchoMessage::method_error(message_id, "compositor did not answer in time"),
                    }
                }
                Err(e) => {
                    warn!("GetWindowThumbnails: {}", e);
                    EchoMessage::method_error(message_id, &e)
                }
            },

            // Where Alt+Tab is (shell/switcher.rs): whether a walk is on,
            // whether the shell was told to show it, the selection and order,
            // and how many of each `SwitcherChanged` phase went out. Read-only,
            // for on-device tests.
            "GetSwitcherState" => {
                let s = crate::shell::switcher::snapshot();
                EchoMessage::method_return(
                    message_id,
                    json!({
                        "active": s.active,
                        "shown": s.shown,
                        "selected": s.selected,
                        "order": s.order,
                        "lastPhase": s.last_phase,
                        "lastCommit": s.last_commit,
                        "counts": {
                            "open": s.counts[0],
                            "step": s.counts[1],
                            "commit": s.counts[2],
                            "cancel": s.counts[3],
                        },
                    }),
                )
            }

            // Window animation mode and speed (see `crate::anim`). Either
            // field may be left out. Applies to animations started from now
            // on. Process-wide state like the overlay's region, so it is set
            // here, not queued for the compositor thread.
            "SetAnimationSettings" => {
                let mode = match msg.body.get("mode") {
                    None | Some(Value::Null) => Ok(None),
                    Some(v) => v
                        .as_str()
                        .and_then(crate::anim::Mode::parse)
                        .map(Some)
                        .ok_or_else(|| "mode must be \"full\", \"reduced\" or \"off\"".to_string()),
                };
                let scale = match msg.body.get("scale") {
                    None | Some(Value::Null) => Ok(None),
                    Some(v) => v
                        .as_f64()
                        .and_then(crate::anim::settings::clamp_scale)
                        .map(Some)
                        .ok_or_else(|| "scale must be a finite number".to_string()),
                };
                match mode.and_then(|m| scale.map(|s| (m, s))) {
                    Ok((mode, scale)) => {
                        let now = crate::anim::settings::update(mode, scale);
                        info!("animation settings: {} x{}", now.mode.as_str(), now.scale);
                        EchoMessage::method_return(
                            message_id,
                            json!({ "ok": true, "mode": now.mode.as_str(), "scale": now.scale }),
                        )
                    }
                    Err(e) => {
                        warn!("SetAnimationSettings: {}", e);
                        EchoMessage::method_error(message_id, &e)
                    }
                }
            }

            // The settings in force and the animation counters, which exist
            // for the on-device suite: `started` and `completed` per kind,
            // and `active`, the animations in flight now.
            "GetAnimationState" => {
                let settings = crate::anim::settings();
                let stats = crate::anim::stats();
                let per_kind = |count: &dyn Fn(crate::anim::AnimKind) -> u64| {
                    let mut map = serde_json::Map::new();
                    for kind in crate::anim::AnimKind::ALL {
                        map.insert(kind.name().to_string(), json!(count(kind)));
                    }
                    Value::Object(map)
                };
                EchoMessage::method_return(
                    message_id,
                    json!({
                        "mode": settings.mode.as_str(),
                        "scale": settings.scale,
                        "active": stats.active(),
                        "started": per_kind(&|k| stats.started(k)),
                        "completed": per_kind(&|k| stats.completed(k)),
                    }),
                )
            }

            // Depth (drop shadows, rounded bottom corners) on or off, and
            // whether its shaders compiled. For the on-device suite and the
            // before/after frame rate; off is also what a failed compile draws.
            "SetDepth" => match msg.body.get("enabled").and_then(Value::as_bool) {
                Some(enabled) => {
                    crate::anim::shader::set_depth_enabled(enabled);
                    info!("depth: {}", if enabled { "on" } else { "off" });
                    EchoMessage::method_return(
                        message_id,
                        json!({ "ok": true, "enabled": enabled, "shaders": crate::anim::shader::shader_status() }),
                    )
                }
                None => EchoMessage::method_error(message_id, "enabled must be a boolean"),
            },
            "GetDepthState" => EchoMessage::method_return(
                message_id,
                json!({
                    "enabled": crate::anim::shader::depth_enabled(),
                    "shaders": crate::anim::shader::shader_status(),
                }),
            ),

            // Lumen on or off (off, every effect draws its plain fallback:
            // genie-lite for minimize), and whether its program built, how
            // many frames went through each effect, each effect's last
            // progress and the MinimizeLanded signals sent. For the suites
            // and the fps comparison; echo-cli only (no bridge entry).
            "SetLumen" => match msg.body.get("enabled").and_then(Value::as_bool) {
                Some(enabled) => {
                    if let Some(mips) = msg.body.get("mipmaps").and_then(Value::as_bool) {
                        crate::anim::lumen::set_mipmaps(mips);
                    }
                    // Bloom's live-surface blur on its own (lane L3), for the
                    // fps comparison.
                    if let Some(blur) = msg.body.get("bloom_blur").and_then(Value::as_bool) {
                        crate::anim::bloom::set_blur_enabled(blur);
                    }
                    crate::anim::lumen::set_enabled(enabled);
                    EchoMessage::method_return(
                        message_id,
                        json!({ "ok": true, "enabled": enabled, "program": crate::anim::lumen::status() }),
                    )
                }
                None => EchoMessage::method_error(message_id, "enabled must be a boolean"),
            },
            "GetLumenState" => {
                let mut state = crate::anim::lumen::state_json();
                let (land, rise) = crate::shell::minimize::landed_counts();
                state["landed"] = json!({ "land": land, "rise": rise });
                // Show desktop (L4): the Recede/Emerge set and order, and the
                // Settle glint's own program (frames are under `frames.glint`).
                state["recede"] = crate::shell::recede::state_json();
                state["glint_program"] = json!(crate::anim::glint::program_status());
                // Evaporate draws through its own program (lane L2).
                state["evaporate_program"] = json!(crate::anim::evaporate::status());
                // Bloom's texture shader (lane L3): {bloom_program, bloom_blur,
                // bloom_blurred_frames}, merged in at the top level.
                if let (Some(obj), Value::Object(bloom)) = (state.as_object_mut(), crate::anim::bloom::state_json()) {
                    obj.extend(bloom);
                }
                EchoMessage::method_return(message_id, state)
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

            // No workspace is real yet (EvoState has one active workspace and
            // nothing moves windows between them), so these refuse instead of
            // reporting a switch that never happened.
            "SwitchWorkspace" | "CreateWorkspace" => EchoMessage::method_error(
                message_id,
                &format!("{method} is not implemented: workspaces are not real yet"),
            ),

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
            EchoCommand::Notify { message, what } => (message, PendingReply::Notify(what)),
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
                    name: "SetOverlayRegion".to_string(),
                    input_args: vec![ArgumentDefinition {
                        name: "rects".to_string(),
                        arg_type: "a(iiii)".to_string(),
                    }],
                    output_args: vec![ArgumentDefinition {
                        name: "success".to_string(),
                        arg_type: "b".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "SetWorkArea".to_string(),
                    input_args: vec![
                        ArgumentDefinition { name: "top".to_string(), arg_type: "i".to_string() },
                        ArgumentDefinition { name: "bottom".to_string(), arg_type: "i".to_string() },
                        ArgumentDefinition { name: "left".to_string(), arg_type: "i".to_string() },
                        ArgumentDefinition { name: "right".to_string(), arg_type: "i".to_string() },
                    ],
                    output_args: vec![ArgumentDefinition {
                        name: "success".to_string(),
                        arg_type: "b".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "SetMinimizeTargets".to_string(),
                    input_args: vec![ArgumentDefinition {
                        name: "targets".to_string(),
                        arg_type: "a(siiii)".to_string(),
                    }],
                    output_args: vec![ArgumentDefinition {
                        name: "ok".to_string(),
                        arg_type: "b".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "SetOverlayFocus".to_string(),
                    input_args: vec![ArgumentDefinition {
                        name: "focused".to_string(),
                        arg_type: "b".to_string(),
                    }],
                    output_args: vec![ArgumentDefinition {
                        name: "success".to_string(),
                        arg_type: "b".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "GetOverlayState".to_string(),
                    input_args: vec![],
                    output_args: vec![ArgumentDefinition {
                        name: "keyboard".to_string(),
                        arg_type: "b".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "GetWindowThumbnails".to_string(),
                    input_args: vec![
                        ArgumentDefinition { name: "window_ids".to_string(), arg_type: "au".to_string() },
                        ArgumentDefinition { name: "max_width".to_string(), arg_type: "u".to_string() },
                        ArgumentDefinition { name: "max_height".to_string(), arg_type: "u".to_string() },
                    ],
                    output_args: vec![ArgumentDefinition {
                        name: "thumbnails".to_string(),
                        arg_type: "a(uuus)".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "GetSwitcherState".to_string(),
                    input_args: vec![],
                    output_args: vec![ArgumentDefinition {
                        name: "active".to_string(),
                        arg_type: "b".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "SetAnimationSettings".to_string(),
                    input_args: vec![
                        ArgumentDefinition { name: "mode".to_string(), arg_type: "s".to_string() },
                        ArgumentDefinition { name: "scale".to_string(), arg_type: "d".to_string() },
                    ],
                    output_args: vec![ArgumentDefinition {
                        name: "ok".to_string(),
                        arg_type: "b".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "GetAnimationState".to_string(),
                    input_args: vec![],
                    output_args: vec![ArgumentDefinition {
                        name: "active".to_string(),
                        arg_type: "u".to_string(),
                    }],
                },
                MethodDefinition {
                    name: "SetLumen".to_string(),
                    input_args: vec![ArgumentDefinition { name: "enabled".to_string(), arg_type: "b".to_string() }],
                    output_args: vec![ArgumentDefinition { name: "ok".to_string(), arg_type: "b".to_string() }],
                },
                MethodDefinition {
                    name: "GetLumenState".to_string(),
                    input_args: vec![],
                    output_args: vec![ArgumentDefinition { name: "program".to_string(), arg_type: "s".to_string() }],
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
                    name: "ShortcutTriggered".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "action".to_string(),
                            arg_type: "s".to_string(),
                        },
                        ArgumentDefinition {
                            name: "timestamp".to_string(),
                            arg_type: "t".to_string(),
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
                    name: "SwitcherChanged".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "phase".to_string(),
                            arg_type: "s".to_string(),
                        },
                        ArgumentDefinition {
                            name: "selected".to_string(),
                            arg_type: "u".to_string(),
                        },
                        ArgumentDefinition {
                            name: "order".to_string(),
                            arg_type: "au".to_string(),
                        },
                        ArgumentDefinition {
                            name: "timestamp".to_string(),
                            arg_type: "t".to_string(),
                        },
                    ],
                },
                SignalDefinition {
                    name: "MinimizeLanded".to_string(),
                    arguments: vec![
                        ArgumentDefinition { name: "app_id".to_string(), arg_type: "s".to_string() },
                        ArgumentDefinition { name: "window_id".to_string(), arg_type: "u".to_string() },
                        ArgumentDefinition { name: "phase".to_string(), arg_type: "s".to_string() },
                        ArgumentDefinition { name: "timestamp".to_string(), arg_type: "t".to_string() },
                    ],
                },
                SignalDefinition {
                    name: "OverlayFocusChanged".to_string(),
                    arguments: vec![
                        ArgumentDefinition {
                            name: "focused".to_string(),
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
            toplevels: Arc::clone(&self.toplevels),
            command_tx: self.command_tx.clone(),
            runtime_handle: self.runtime_handle.clone(),
            pending_window_actions: Arc::clone(&self.pending_window_actions),
            rebind_requested: Arc::clone(&self.rebind_requested),
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

/// The `BindSurface` arguments. `pid` is left out when unknown, since the
/// daemon reads an absent one as "match by app id alone".
fn bind_surface_body(app_id: &str, surface_id: u32, pid: Option<u32>) -> Value {
    let mut body = json!({ "appId": app_id, "surfaceId": surface_id });
    if let Some(pid) = pid {
        body["pid"] = json!(pid);
    }
    body
}

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
        let rebind = Arc::new(AtomicBool::new(false));
        tokio::spawn(GeckoEchoService::echo_broker_loop(
            command_rx,
            path.clone(),
            Arc::new(RwLock::new(Vec::new())),
            Arc::new(AtomicU32::new(0)),
            Arc::new(RwLock::new(Vec::new())),
            Arc::clone(&rebind),
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
        assert!(!rebind.load(Ordering::Acquire), "first connection is not a reconnect");

        // Drop the connection. The broker must come back and register again
        // without being asked.
        drop(daemon);
        let (mut daemon, _) = listener.accept().await.unwrap();
        assert_eq!(read_frame(&mut daemon).await["header"]["message_type"], "HANDSHAKE");
        write_frame(&mut daemon, &frame("METHOD_RETURN", json!({}))).await;
        let again = read_frame(&mut daemon).await;
        assert_eq!(again["header"]["method"], "RegisterService");
        // The restarted daemon's appHost has no bindings: the broker asks the
        // compositor thread to replay them, and only after a *re*connect.
        assert!(rebind.swap(false, Ordering::AcqRel), "no rebind request after reconnect");
        assert_eq!(again["body"]["name"], "org.geckoOS.evo");
    }

    /// A frame that arrives in pieces, with a signal of Evo's sent while it
    /// is half read. Reading inside the broker's `select!` lost the half that
    /// was read when the signal won, so the next "length" came from the middle
    /// of the frame and the connection was dropped (echo-daemon had the same
    /// bug the other way round).
    #[tokio::test]
    async fn broker_keeps_a_frame_split_by_its_own_signal() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("echo.sock");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let (command_tx, command_rx) = mpsc::channel(8);
            let rebind = Arc::new(AtomicBool::new(false));
            tokio::spawn(GeckoEchoService::echo_broker_loop(
                command_rx,
                path.clone(),
                Arc::new(RwLock::new(Vec::new())),
                Arc::new(AtomicU32::new(0)),
                Arc::new(RwLock::new(Vec::new())),
                Arc::clone(&rebind),
            ));
            let (mut daemon, _) = listener.accept().await.unwrap();
            assert_eq!(read_frame(&mut daemon).await["header"]["message_type"], "HANDSHAKE");
            write_frame(&mut daemon, &frame("METHOD_RETURN", json!({}))).await;

            // A ListWindows call, written as its length and first half...
            let list = frame("METHOD_CALL", json!({ "destination": "org.geckoOS.evo", "interface": "org.geckoOS.evo", "method": "ListWindows" }));
            let list_id = list["header"]["message_id"].clone();
            let data = serde_json::to_vec(&list).unwrap();
            let (first, rest) = data.split_at(data.len() / 2);
            daemon.write_u32_le(data.len() as u32).await.unwrap();
            daemon.write_all(first).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            // ...then Evo emits a signal while the broker waits for the rest...
            command_tx
                .send(EchoCommand::EmitSignal { signal_name: "WindowFocused".into(), args: json!({}) })
                .await
                .unwrap();
            assert_eq!(read_frame(&mut daemon).await["header"]["method"], "EmitSignal");
            // ...and only then the second half.
            daemon.write_all(rest).await.unwrap();

            let answer = read_frame(&mut daemon).await;
            assert_eq!(answer["header"]["reply_to"], list_id, "the split call was answered");
            assert!(!rebind.load(Ordering::Acquire), "the broker did not reconnect");
        })
        .await
        .expect("the broker lost the split frame");
    }

    #[test]
    fn bind_surface_carries_the_pid_when_it_has_one() {
        assert_eq!(
            bind_surface_body("com.pyxis.notes", 7, Some(4242)),
            json!({ "appId": "com.pyxis.notes", "surfaceId": 7, "pid": 4242 })
        );
        assert_eq!(
            bind_surface_body("com.pyxis.notes", 7, None),
            json!({ "appId": "com.pyxis.notes", "surfaceId": 7 })
        );
    }

    /// Binding used to wait up to 3 s for the daemon on the compositor
    /// thread. With no daemon at all it must return at once.
    #[test]
    fn binding_a_surface_does_not_wait_for_a_reply() {
        std::env::set_var("XDG_RUNTIME_DIR", std::env::temp_dir().join("evo-no-echo-daemon"));
        let echo = GeckoEchoService::new().unwrap();
        let start = Instant::now();
        echo.bind_surface_remote("com.pyxis.notes", 1, Some(1)).unwrap();
        echo.unbind_surface_remote(1).unwrap();
        assert!(start.elapsed() < std::time::Duration::from_millis(500), "waited {:?}", start.elapsed());
    }

    #[test]
    fn a_lost_connection_fails_the_waiting_call_instead_of_leaving_it() {
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        PendingReply::Call(tx).fail();
        assert!(rx.try_recv().unwrap().is_err());
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