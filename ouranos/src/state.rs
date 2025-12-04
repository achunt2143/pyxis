//! Compositor State Management
//!
//! Main compositor state integrating all subsystems

use anyhow::Result;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::{Display, DisplayHandle, Client, backend::ClientData as WaylandClientData};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::{Space, Window};
use smithay::input::{Seat, SeatState, SeatHandler, pointer::CursorImageStatus};
use smithay::reexports::calloop::LoopSignal;
use smithay::wayland::compositor::{CompositorState, CompositorClientState, CompositorHandler};
use smithay::wayland::shell::xdg::{
    XdgShellState, XdgShellHandler, ToplevelSurface, PopupSurface, PositionerState
};
use smithay::wayland::shm::{ShmState, ShmHandler};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::{DataDeviceState as SmithayDataDeviceState, DataDeviceHandler};
use smithay::delegate_compositor;
use smithay::delegate_shm;
use smithay::delegate_seat;
use smithay::delegate_data_device;
use smithay::delegate_output;
use smithay::delegate_xdg_shell;
use smithay::utils::Serial;
use std::time::Instant;

use crate::window::{WindowStateManager, WindowMetadata, WindowState as WinState};
use crate::config::Config;
use crate::hotplug::HotPlugManager;
use crate::damage_tracker::{DamageTracker, Region};
use crate::data_device::DataDeviceState;
use crate::input_advanced::AdvancedInputManager;
use crate::gecko_surface::GeckoSurfaceManager;

/// Window element for rendering
pub type WindowElement = Window;

/// Client data associated with each Wayland client
pub struct ClientData {
    pub compositor_state: CompositorClientState,
}

impl WaylandClientData for ClientData {
    fn initialized(&self, _client_id: smithay::reexports::wayland_server::backend::ClientId) {}
    fn disconnected(&self, _client_id: smithay::reexports::wayland_server::backend::ClientId, _reason: smithay::reexports::wayland_server::backend::DisconnectReason) {}
}

/// Main compositor state
pub struct GeckoOSState {
    // Smithay core
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: SmithayDataDeviceState,
    pub output_manager_state: OutputManagerState,
    
    // Event loop
    pub loop_signal: LoopSignal,
    
    // Rendering
    pub space: Space<WindowElement>,
    pub renderer: Option<GlesRenderer>,
    
    // State managers
    pub window_manager: WindowStateManager,
    pub config: Config,
    pub hotplug_manager: HotPlugManager,
    pub damage_tracker: DamageTracker,
    pub data_device: DataDeviceState,
    pub input_manager: AdvancedInputManager,
    pub gecko_surfaces: GeckoSurfaceManager,
    
    // Seat
    pub seat: Seat<Self>,
    
    // Runtime state
    pub socket_name: String,
    pub running: bool,
    pub last_frame: Instant,
    pub frame_count: u64,
}

impl GeckoOSState {
    pub fn new() -> Result<(Self, smithay::reexports::calloop::EventLoop<'static, Self>, Display<Self>)> {
        let config = crate::config::ConfigLoader::load_default();
        config.validate()?;
        
        tracing::info!("Initializing GeckoOS Compositor");
        tracing::info!("Display mode: {}", config.display_mode_string());
        
        // Create event loop
        let event_loop = smithay::reexports::calloop::EventLoop::try_new()?;
        let loop_signal = event_loop.get_signal();
        
        // Create Wayland display
        let mut display: Display<Self> = Display::new()?;
        let display_handle = display.handle();
        
        // Initialize Smithay states
        let compositor_state = CompositorState::new::<Self>(&display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);
        let data_device_state = SmithayDataDeviceState::new::<Self>(&display_handle);
        
        // Create seat for input
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&display_handle, "seat0");
        
        // Add keyboard capability
        seat.add_keyboard(Default::default(), 200, 25)?;
        // Add pointer capability
        seat.add_pointer();
        
        // Initialize managers
        let window_manager = WindowStateManager::new();
        let hotplug_manager = HotPlugManager::new();
        let damage_tracker = DamageTracker::new(
            config.display.default_width,
            config.display.default_height,
        );
        let data_device = DataDeviceState::new();
        let input_manager = AdvancedInputManager::new();
        let gecko_surfaces = GeckoSurfaceManager::new();
        
        let space = Space::default();
        
        // Get socket name
        let listening_socket = smithay::wayland::socket::ListeningSocketSource::new_auto()?;
let socket_name = listening_socket.socket_name().to_string_lossy().into_owned();
tracing::info!("Wayland socket: {}", socket_name);
        
        // Set environment variable for clients
        std::env::set_var("WAYLAND_DISPLAY", &socket_name);
        
        let state = GeckoOSState {
            display_handle,
            compositor_state,
            xdg_shell_state,
            shm_state,
            seat_state,
            data_device_state,
            output_manager_state,
            loop_signal,
            space,
            renderer: None,
            window_manager,
            config,
            hotplug_manager,
            damage_tracker,
            data_device,
            input_manager,
            gecko_surfaces,
            seat,
            socket_name,
            running: true,
            last_frame: Instant::now(),
            frame_count: 0,
        };
        
        Ok((state, event_loop, display))
    }
    
    pub fn socket_name(&self) -> &str {
        &self.socket_name
    }
    
    pub fn render_frame(&mut self) {
        let now = Instant::now();
        let delta = now.duration_since(self.last_frame);
        self.last_frame = now;
        self.frame_count += 1;
        
        if self.frame_count % 60 == 0 {
            let fps = 1.0 / delta.as_secs_f64();
            tracing::debug!("FPS: {:.1}", fps);
        }
        
        // Commit damage and clear for next frame
        self.damage_tracker.commit_damage();
        
        // Render all windows
        for (window_id, window) in self.window_manager.all_windows() {
            let geom = &window.geometry;
            let region = Region::new(geom.x, geom.y, geom.width, geom.height);
            
            if self.damage_tracker.needs_update(&region) {
                tracing::trace!("Rendering window {}", window_id);
            }
        }
        
        self.damage_tracker.clear_damage();
    }
    
    pub fn stop(&mut self) {
        tracing::info!("Stopping compositor");
        self.running = false;
        self.loop_signal.stop();
    }
}

// Smithay protocol delegation
delegate_compositor!(GeckoOSState);
delegate_shm!(GeckoOSState);
delegate_seat!(GeckoOSState);
delegate_data_device!(GeckoOSState);
delegate_output!(GeckoOSState);
delegate_xdg_shell!(GeckoOSState);

// BufferHandler implementation (required by ShmHandler)
impl BufferHandler for GeckoOSState {
    fn buffer_destroyed(&mut self, _buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer) {
        // Handle buffer destruction
        tracing::trace!("Buffer destroyed");
    }
}

// ShmHandler implementation
impl ShmHandler for GeckoOSState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl smithay::wayland::output::OutputHandler for GeckoOSState {
    fn output_bound(
        &mut self,
        _output: smithay::output::Output,
        _global: smithay::reexports::wayland_server::protocol::wl_output::WlOutput,
    ) {
        tracing::trace!("Output bound");
    }
}



// CompositorHandler implementation
impl CompositorHandler for GeckoOSState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientData>()
            .expect("Client data not initialized")
            .compositor_state
    }
    
    fn commit(&mut self, _surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) {
        tracing::trace!("Surface committed");
    }
}

impl smithay::wayland::selection::SelectionHandler for GeckoOSState {
    type SelectionUserData = ();
}

impl smithay::wayland::selection::data_device::ServerDndGrabHandler for GeckoOSState {
    // fn started(&mut self, _source: smithay::wayland::selection::data_device::SelectionSource, _icon: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>) {}
    
    fn accept(&mut self, mime_type: Option<String>, seat: Seat<Self>) {}
    
    fn action(&mut self, action: smithay::reexports::wayland_server::protocol::wl_data_device_manager::DndAction, seat: Seat<Self>) {}
    
    fn dropped(&mut self, seat: Seat<Self>) {}
    
    fn cancelled(&mut self, seat: Seat<Self>) {}
    
    fn send(&mut self, mime_type: String, fd: std::os::unix::prelude::OwnedFd, _seat: Seat<Self>) {
    use std::io::Write;
    
    tracing::debug!("DnD send: mime_type={}", mime_type);
    
    // Convert OwnedFd to a file for writing
    let mut file = std::fs::File::from(fd);
    
    // Determine what data to send based on mime type
    let data = match mime_type.as_str() {
        "text/plain" => b"Sample text data".to_vec(),
        "text/uri-list" => b"file:///path/to/file\n".to_vec(),
        "text/html" => b"<html><body>Sample HTML</body></html>".to_vec(),
        _ => {
            tracing::warn!("Unsupported mime type: {}", mime_type);
            return;
        }
    };
    
    // Write data to the file descriptor
    match file.write_all(&data) {
        Ok(()) => {
            tracing::debug!("DnD data sent successfully ({} bytes)", data.len());
        }
        Err(e) => {
            tracing::error!("Failed to send DnD data: {}", e);
        }
    }
    
    // File is automatically closed when dropped
}

    
    fn finished(&mut self, seat: Seat<Self>) {}
}

impl smithay::wayland::selection::data_device::ClientDndGrabHandler for GeckoOSState {
    // fn action(&mut self, _action: smithay::wayland::selection::data_device::DndAction) {}
    // fn cancelled(&mut self) {}
    // fn finished(&mut self) {}
    fn dropped(&mut self, surface: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>, success: bool, seat: Seat<Self>) {}

    
    fn started(&mut self, source: Option<smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource>, icon: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>, seat: Seat<Self>) {}
}

// DataDeviceHandler implementation
impl DataDeviceHandler for GeckoOSState {
    fn data_device_state(&self) -> &SmithayDataDeviceState {
        &self.data_device_state
    }
}

// XdgShellHandler implementation
impl XdgShellHandler for GeckoOSState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }
    
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        tracing::info!("New XDG toplevel surface");
        
        // Get title and app_id from the surface
        // In smithay 0.7, you access these via getter functions
        let title = surface.xdg_toplevel().id().interface().name.to_string();
        
        let app_id = surface.xdg_toplevel().id().protocol_id().to_string();
        
        // Create window metadata
        let metadata = WindowMetadata {
            title,
            app_id,
            state: WinState::Normal,
            geometry: Default::default(),
            parent_id: None,
            is_focused: false,
            is_decorated: self.config.features.enable_server_side_decorations,
            workspace: 0,
        };
        
        let window_id = self.window_manager.create_window(metadata);
        
        // Add damage for new window
        if let Some(window) = self.window_manager.get_window(window_id) {
            let geom = &window.geometry;
            self.damage_tracker.add_damage(Region::new(
                geom.x, geom.y, geom.width, geom.height
            ));
        }
    }
    
    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {
        tracing::info!("New XDG popup surface");
    }
    
    fn grab(&mut self, _surface: PopupSurface, _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat, _serial: Serial) {
        tracing::debug!("Popup grab requested");
    }
    
    fn reposition_request(&mut self, _surface: PopupSurface, _positioner: PositionerState, _token: u32) {
        tracing::debug!("Popup reposition requested");
    }
}

// SeatHandler implementation
impl SeatHandler for GeckoOSState {
    type KeyboardFocus = smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
    type PointerFocus = smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
    type TouchFocus = smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
    
    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    
    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&Self::KeyboardFocus>) {
        // Handle focus changes
    }
    
    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {
        // Handle cursor image changes
    }
}
