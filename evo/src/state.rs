#[cfg(feature = "xwayland")]
use std::os::unix::io::OwnedFd;
use std::{
    collections::HashMap,
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use parking_lot::RwLock;
use tracing::{debug, error, info, warn};

use smithay::{
    backend::{
        input::TabletToolDescriptor,
        renderer::element::{
            default_primary_scanout_output_compare, utils::select_dmabuf_feedback,
            RenderElementStates,
        },
    },
    delegate_compositor, delegate_data_control, delegate_data_device, delegate_fixes,
    delegate_fractional_scale, delegate_input_method_manager, delegate_keyboard_shortcuts_inhibit,
    delegate_layer_shell, delegate_output, delegate_pointer_constraints, delegate_pointer_gestures,
    delegate_presentation, delegate_primary_selection, delegate_relative_pointer, delegate_seat,
    delegate_security_context, delegate_shm, delegate_tablet_manager, delegate_text_input_manager,
    delegate_viewporter, delegate_virtual_keyboard_manager, delegate_xdg_activation,
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        space::SpaceElement,
        utils::{
            surface_presentation_feedback_flags_from_states, surface_primary_scanout_output,
            update_surface_primary_scanout_output, with_surfaces_surface_tree,
            OutputPresentationFeedback,
        },
        PopupKind, PopupManager, Space, Window,
    },
    input::{
        dnd::{DnDGrab, DndGrabHandler, DndTarget, GrabType, Source},
        keyboard::{Keysym, LedState, XkbConfig},
        pointer::{CursorImageStatus, Focus, PointerHandle},
        Seat, SeatHandler, SeatState,
    },
    output::Output,
    reexports::{
        calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction},
        wayland_protocols::xdg::decoration::{
            self as xdg_decoration,
            zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
        },
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
            Client, Display, DisplayHandle, Resource,
        },
    },
    utils::{Clock, Logical, Monotonic, Point, Rectangle, Serial, Time},
    wayland::{
        commit_timing::{CommitTimerBarrierStateUserData, CommitTimingManagerState},
        compositor::{
            get_parent, with_states, CompositorClientState, CompositorHandler, CompositorState,
        },
        dmabuf::DmabufFeedback,
        fifo::{FifoBarrierCachedState, FifoManagerState},
        fixes::FixesState,
        fractional_scale::{
            with_fractional_scale, FractionalScaleHandler, FractionalScaleManagerState,
        },
        idle_notify::{IdleNotifierHandler, IdleNotifierState},
        input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface},
        keyboard_shortcuts_inhibit::{
            KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState,
            KeyboardShortcutsInhibitor,
        },
        output::{OutputHandler, OutputManagerState},
        pointer_constraints::{
            with_pointer_constraint, PointerConstraintsHandler, PointerConstraintsState,
        },
        pointer_gestures::PointerGesturesState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        seat::WaylandFocus,
        security_context::{
            SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
            SecurityContextState,
        },
        selection::{
            data_device::{
                set_data_device_focus, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
            },
            primary_selection::{
                set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
            },
            wlr_data_control::{DataControlHandler, DataControlState},
            SelectionHandler,
        },
        shell::{
            wlr_layer::{
                Anchor, Layer, LayerSurfaceAttributes, LayerSurfaceData, WlrLayerShellState,
            },
            xdg::{
                decoration::{XdgDecorationHandler, XdgDecorationState},
                ToplevelSurface, XdgShellState,
            },
        },
        shm::{ShmHandler, ShmState},
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        tablet_manager::{TabletManagerState, TabletSeatHandler},
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        virtual_keyboard::VirtualKeyboardManagerState,
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
        xdg_foreign::{XdgForeignHandler, XdgForeignState},
    },
};
use wayland_client::protocol::wl_surface;

#[cfg(feature = "xwayland")]
use crate::cursor::Cursor;
use crate::{
    echo_ipc::CreateWindowRequest,
    focus::{KeyboardFocusTarget, PointerFocusTarget},
    shell::{WindowElement, Workspace, WorkspaceId, WorkspaceManager},
};
#[cfg(feature = "xwayland")]
use smithay::{
    delegate_xwayland_keyboard_grab, delegate_xwayland_shell,
    utils::Size,
    wayland::selection::{SelectionSource, SelectionTarget},
    wayland::xwayland_keyboard_grab::{XWaylandKeyboardGrabHandler, XWaylandKeyboardGrabState},
    wayland::xwayland_shell,
    xwayland::{X11Wm, XWayland, XWaylandEvent},
};

#[cfg(feature = "echo-ipc")]
use crate::enhancements::echo_ipc::GeckoEchoService;
#[cfg(feature = "hotplug")]
use crate::enhancements::hotplug::HotPlugManager;
#[cfg(feature = "screencopy")]
use crate::enhancements::screencopy::{
    copy_frame_to_shm_buffer, ScreencopyHandler, ScreencopyManagerState,
};
#[cfg(feature = "advanced-input")]
use crate::input::AdvancedInputManager;
#[cfg(feature = "gecko-surface")]
use crate::surface::GeckoSurface;
#[cfg(feature = "gecko-window-manager")]
use crate::window::WindowStateManager;

#[derive(Debug, Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    pub security_context: Option<SecurityContext>,
}
impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

pub struct EvoState<BackendData: Backend + 'static> {
    pub backend_data: BackendData,
    pub socket_name: Option<String>,
    pub display_handle: DisplayHandle,
    pub running: Arc<AtomicBool>,
    pub handle: LoopHandle<'static, EvoState<BackendData>>,

    // desktop
    pub space: Space<WindowElement>,
    pub popups: PopupManager,

    // smithay state
    pub compositor_state: CompositorState,
    pub data_device_state: DataDeviceState,
    pub layer_shell_state: WlrLayerShellState,
    pub output_manager_state: OutputManagerState,
    pub primary_selection_state: PrimarySelectionState,
    pub data_control_state: DataControlState,
    pub seat_state: SeatState<EvoState<BackendData>>,
    pub keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    pub shm_state: ShmState,
    pub viewporter_state: ViewporterState,
    pub xdg_activation_state: XdgActivationState,
    pub xdg_decoration_state: XdgDecorationState,
    pub xdg_shell_state: XdgShellState,
    pub presentation_state: PresentationState,
    pub fractional_scale_manager_state: FractionalScaleManagerState,
    pub xdg_foreign_state: XdgForeignState,
    #[cfg(feature = "xwayland")]
    pub xwayland_shell_state: xwayland_shell::XWaylandShellState,
    pub single_pixel_buffer_state: SinglePixelBufferState,
    pub fifo_manager_state: FifoManagerState,
    pub commit_timing_manager_state: CommitTimingManagerState,
    pub idle_notifier_state: IdleNotifierState<Self>,

    pub dnd_icon: Option<DndIcon>,

    // input-related fields
    pub suppressed_keys: Vec<Keysym>,
    pub cursor_status: CursorImageStatus,
    pub seat_name: String,
    pub seat: Seat<EvoState<BackendData>>,
    pub clock: Clock<Monotonic>,
    pub pointer: PointerHandle<EvoState<BackendData>>,

    #[cfg(feature = "xwayland")]
    pub xwm: Option<X11Wm>,
    #[cfg(feature = "xwayland")]
    pub xdisplay: Option<u32>,

    #[cfg(feature = "debug")]
    pub renderdoc: Option<renderdoc::RenderDoc<renderdoc::V141>>,

    pub show_window_preview: bool,

    #[cfg(feature = "gecko-window-manager")]
    pub window_manager: Arc<RwLock<WindowStateManager>>,

    #[cfg(feature = "hotplug")]
    pub hotplug_manager: Arc<RwLock<HotPlugManager>>,

    #[cfg(feature = "echo-ipc")]
    pub echo_service: Arc<RwLock<GeckoEchoService>>,

    #[cfg(feature = "advanced-input")]
    pub input_manager: Arc<RwLock<AdvancedInputManager>>,

    #[cfg(feature = "gecko-surface")]
    pub gecko_surfaces: Arc<RwLock<HashMap<u32, GeckoSurface>>>,

    pub start_time: Instant,
    pub workspace_manager: WorkspaceManager,

    #[cfg(feature = "screencopy")]
    pub screencopy_state: ScreencopyManagerState,

    // Stable window IDs and registry for Echo IPC
    #[cfg(feature = "echo-ipc")]
    pub next_window_id: AtomicU32,
    #[cfg(feature = "echo-ipc")]
    pub windows_by_id: HashMap<u32, WindowElement>,
    #[cfg(feature = "echo-ipc")]
    pub window_ids_by_element: HashMap<Window, u32>,
}

#[derive(Debug)]
pub struct DndIcon {
    pub surface: WlSurface,
    pub offset: Point<i32, Logical>,
}

delegate_compositor!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> DataDeviceHandler for EvoState<BackendData> {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl<BackendData: Backend> WaylandDndGrabHandler for EvoState<BackendData> {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        self.dnd_icon = icon.map(|surface| DndIcon {
            surface,
            offset: (0, 0).into(),
        });

        match type_ {
            GrabType::Pointer => {
                let pointer = seat.get_pointer().unwrap();
                let start_data = pointer.grab_start_data().unwrap();
                pointer.set_grab(
                    self,
                    DnDGrab::new_pointer(&self.display_handle, start_data, source, seat),
                    serial,
                    Focus::Keep,
                );
            }
            GrabType::Touch => {
                let touch = seat.get_touch().unwrap();
                let start_data = touch.grab_start_data().unwrap();
                touch.set_grab(
                    self,
                    DnDGrab::new_touch(&self.display_handle, start_data, source, seat),
                    serial,
                );
            }
        }
    }
}

impl<BackendData: Backend> DndGrabHandler for EvoState<BackendData> {
    fn dropped(
        &mut self,
        _target: Option<DndTarget<'_, Self>>,
        _validated: bool,
        _seat: Seat<Self>,
        _location: Point<f64, Logical>,
    ) {
        self.dnd_icon = None;
    }
}
delegate_data_device!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> OutputHandler for EvoState<BackendData> {}
delegate_output!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> SelectionHandler for EvoState<BackendData> {
    type SelectionUserData = ();

    #[cfg(feature = "xwayland")]
    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.new_selection(ty, source.map(|source| source.mime_types())) {
                warn!(?err, ?ty, "Failed to set Xwayland selection");
            }
        }
    }

    #[cfg(feature = "xwayland")]
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.send_selection(ty, mime_type, fd) {
                warn!(?err, "Failed to send primary (X11 -> Wayland)");
            }
        }
    }
}

impl<BackendData: Backend> PrimarySelectionHandler for EvoState<BackendData> {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}
delegate_primary_selection!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> DataControlHandler for EvoState<BackendData> {
    fn data_control_state(&mut self) -> &mut DataControlState {
        &mut self.data_control_state
    }
}

delegate_data_control!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> ShmHandler for EvoState<BackendData> {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
delegate_shm!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> SeatHandler for EvoState<BackendData> {
    type KeyboardFocus = KeyboardFocusTarget;
    type PointerFocus = PointerFocusTarget;
    type TouchFocus = PointerFocusTarget;

    fn seat_state(&mut self) -> &mut SeatState<EvoState<BackendData>> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, target: Option<&KeyboardFocusTarget>) {
        let dh = &self.display_handle;

        let wl_surface = target.and_then(WaylandFocus::wl_surface);
        let focus = wl_surface.clone().and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, focus.clone());
        set_primary_focus(dh, seat, focus);

        #[cfg(feature = "gecko-window-manager")]
        {
            if let Some(surface) = wl_surface {
                // Convert Cow<'_, WlSurface> to WlSurface by cloning the inner value
                if let Some(window) = self.window_for_surface(surface.clone().into_owned()) {
                    if let Some(window_id) =
                        self.window_manager.read().id_for_smithay_window(&window)
                    {
                        self.window_manager.write().set_focus(Some(window_id));
                    }
                } else {
                    self.window_manager.write().set_focus(None);
                }
            } else {
                self.window_manager.write().set_focus(None);
            }
        }

        #[cfg(feature = "echo-ipc")]
        {
            let workspace_id = self.workspace_manager.active_workspace_id().0;

            // Get the focused window ID if there is a focused surface
            let focused_window_id = wl_surface
                .and_then(|surface| {
                    // Convert Cow<'_, WlSurface> to WlSurface
                    self.window_for_surface(surface.clone().into_owned())
                })
                .and_then(|window| self.window_id_for(&window));

            // Emit focus change event with workspace context
            let _ = self
                .echo_service
                .read()
                .emit_window_focus_changed(focused_window_id, workspace_id);
        }
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }

    fn led_state_changed(&mut self, _seat: &Seat<Self>, led_state: LedState) {
        self.backend_data.update_led_state(led_state)
    }
}
delegate_seat!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> TabletSeatHandler for EvoState<BackendData> {
    fn tablet_tool_image(&mut self, _tool: &TabletToolDescriptor, image: CursorImageStatus) {
        self.cursor_status = image;
    }
}
delegate_tablet_manager!(@<BackendData: Backend + 'static> EvoState<BackendData>);

delegate_text_input_manager!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> InputMethodHandler for EvoState<BackendData> {
    fn new_popup(&mut self, surface: PopupSurface) {
        if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
            warn!("Failed to track popup: {}", err);
        }
    }

    fn popup_repositioned(&mut self, _: PopupSurface) {}

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        if let Some(parent) = surface.get_parent().map(|parent| parent.surface.clone()) {
            let _ = PopupManager::dismiss_popup(&parent, &PopupKind::from(surface));
        }
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, smithay::utils::Logical> {
        self.space
            .elements()
            .find_map(|window| {
                (window.wl_surface().as_deref() == Some(parent)).then(|| window.geometry())
            })
            .unwrap_or_default()
    }
}

delegate_input_method_manager!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> KeyboardShortcutsInhibitHandler for EvoState<BackendData> {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        inhibitor.activate();
    }
}

delegate_keyboard_shortcuts_inhibit!(@<BackendData: Backend + 'static> EvoState<BackendData>);

delegate_virtual_keyboard_manager!(@<BackendData: Backend + 'static> EvoState<BackendData>);

delegate_pointer_gestures!(@<BackendData: Backend + 'static> EvoState<BackendData>);

delegate_relative_pointer!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> PointerConstraintsHandler for EvoState<BackendData> {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        let Some(current_focus) = pointer.current_focus() else {
            return;
        };
        if current_focus.wl_surface().as_deref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                constraint.unwrap().activate();
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        if with_pointer_constraint(surface, pointer, |constraint| {
            constraint.is_some_and(|c| c.is_active())
        }) {
            let origin = self
                .space
                .elements()
                .find_map(|window| {
                    (window.wl_surface().as_deref() == Some(surface)).then(|| window.geometry())
                })
                .unwrap_or_default()
                .loc
                .to_f64();

            pointer.set_location(origin + location);
        }
    }
}
delegate_pointer_constraints!(@<BackendData: Backend + 'static> EvoState<BackendData>);

delegate_viewporter!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> XdgActivationHandler for EvoState<BackendData> {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        if let Some((serial, seat)) = data.serial {
            let keyboard = self.seat.get_keyboard().unwrap();
            Seat::from_resource(&seat) == Some(self.seat.clone())
                && keyboard
                    .last_enter()
                    .map(|last_enter| serial.is_no_older_than(&last_enter))
                    .unwrap_or(false)
        } else {
            false
        }
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if token_data.timestamp.elapsed().as_secs() < 10 {
            let w = self
                .space
                .elements()
                .find(|window| window.wl_surface().map(|s| *s == surface).unwrap_or(false))
                .cloned();
            if let Some(window) = w {
                self.space.raise_element(&window, true);
            }
        }
    }
}
delegate_xdg_activation!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> XdgDecorationHandler for EvoState<BackendData> {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ClientSide);
        });
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;

        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(match mode {
                DecorationMode::ServerSide => Mode::ServerSide,
                _ => Mode::ClientSide,
            });
        });

        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ClientSide);
        });

        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}
delegate_xdg_decoration!(@<BackendData: Backend + 'static> EvoState<BackendData>);

delegate_xdg_shell!(@<BackendData: Backend + 'static> EvoState<BackendData>);
delegate_layer_shell!(@<BackendData: Backend + 'static> EvoState<BackendData>);
delegate_presentation!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> FractionalScaleHandler for EvoState<BackendData> {
    fn new_fractional_scale(
        &mut self,
        surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) {
        #[allow(clippy::redundant_clone)]
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        with_states(&surface, |states| {
            let primary_scanout_output = surface_primary_scanout_output(&surface, states)
                .or_else(|| {
                    if root != surface {
                        with_states(&root, |states| {
                            surface_primary_scanout_output(&root, states).or_else(|| {
                                self.window_for_surface(root.clone()).and_then(|window| {
                                    self.space.outputs_for_element(&window).first().cloned()
                                })
                            })
                        })
                    } else {
                        self.window_for_surface(root.clone()).and_then(|window| {
                            self.space.outputs_for_element(&window).first().cloned()
                        })
                    }
                })
                .or_else(|| self.space.outputs().next().cloned());
            if let Some(output) = primary_scanout_output {
                with_fractional_scale(states, |fractional_scale| {
                    fractional_scale.set_preferred_scale(output.current_scale().fractional_scale());
                });
            }
        });
    }
}
delegate_fractional_scale!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend + 'static> SecurityContextHandler for EvoState<BackendData> {
    fn context_created(
        &mut self,
        source: SecurityContextListenerSource,
        security_context: SecurityContext,
    ) {
        self.handle
            .insert_source(source, move |client_stream, _, data| {
                let client_state = ClientState {
                    security_context: Some(security_context.clone()),
                    ..ClientState::default()
                };
                if let Err(err) = data
                    .display_handle
                    .insert_client(client_stream, Arc::new(client_state))
                {
                    warn!("Error adding wayland client: {}", err);
                };
            })
            .expect("Failed to init wayland socket source");
    }
}
delegate_security_context!(@<BackendData: Backend + 'static> EvoState<BackendData>);

#[cfg(feature = "xwayland")]
impl<BackendData: Backend + 'static> XWaylandKeyboardGrabHandler for EvoState<BackendData> {
    fn keyboard_focus_for_xsurface(&self, surface: &WlSurface) -> Option<KeyboardFocusTarget> {
        let elem = self
            .space
            .elements()
            .find(|elem| elem.wl_surface().as_deref() == Some(surface))?;
        Some(KeyboardFocusTarget::Window(elem.0.clone()))
    }
}
#[cfg(feature = "xwayland")]
delegate_xwayland_keyboard_grab!(@<BackendData: Backend + 'static> EvoState<BackendData>);

#[cfg(feature = "xwayland")]
delegate_xwayland_shell!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> XdgForeignHandler for EvoState<BackendData> {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.xdg_foreign_state
    }
}

#[cfg(feature = "screencopy")]
impl<BackendData: Backend> ScreencopyHandler for EvoState<BackendData> {
    fn screencopy_state(&mut self) -> &mut ScreencopyManagerState {
        &mut self.screencopy_state
    }

    fn copy_frame(
        &mut self,
        output: &Output,
        buffer: &WlBuffer,
        region: Option<Rectangle<i32, Logical>>,
        overlay_cursor: bool,
    ) -> Result<(), crate::shell::screencopy::ScreencopyError> {
        copy_frame_to_shm_buffer(self, output, buffer, region, overlay_cursor)
    }
}
#[cfg(feature = "screencopy")]
delegate_screencopy!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> IdleNotifierHandler for EvoState<BackendData> {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier_state
    }
}

smithay::delegate_idle_notify!(@<BackendData: Backend + 'static> EvoState<BackendData>);

smithay::delegate_xdg_foreign!(@<BackendData: Backend + 'static> EvoState<BackendData>);

smithay::delegate_single_pixel_buffer!(@<BackendData: Backend + 'static> EvoState<BackendData>);

smithay::delegate_fifo!(@<BackendData: Backend + 'static> EvoState<BackendData>);

smithay::delegate_commit_timing!(@<BackendData: Backend + 'static> EvoState<BackendData>);

delegate_fixes!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend + 'static> EvoState<BackendData> {
    pub fn init(
        display: Display<EvoState<BackendData>>,
        handle: LoopHandle<'static, EvoState<BackendData>>,
        backend_data: BackendData,
        listen_on_socket: bool,
    ) -> EvoState<BackendData> {
        let dh = display.handle();

        let clock = Clock::new();

        let socket_name = if listen_on_socket {
            let source = ListeningSocketSource::new_auto().unwrap();
            let socket_name = source.socket_name().to_string_lossy().into_owned();
            handle
                .insert_source(source, |client_stream, _, data| {
                    if let Err(err) = data
                        .display_handle
                        .insert_client(client_stream, Arc::new(ClientState::default()))
                    {
                        warn!("Error adding wayland client: {}", err);
                    };
                })
                .expect("Failed to init wayland socket source");
            info!(name = socket_name, "Listening on wayland socket");
            Some(socket_name)
        } else {
            None
        };
        handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, data| {
                    profiling::scope!("dispatch_clients");
                    unsafe {
                        display.get_mut().dispatch_clients(data).unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .expect("Failed to init wayland server source");

        let compositor_state = CompositorState::new::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let data_control_state =
            DataControlState::new::<Self, _>(&dh, Some(&primary_selection_state), |_| true);
        let mut seat_state = SeatState::new();
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let xdg_activation_state = XdgActivationState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let presentation_state = PresentationState::new::<Self>(&dh, clock.id() as u32);
        let fractional_scale_manager_state = FractionalScaleManagerState::new::<Self>(&dh);
        let xdg_foreign_state = XdgForeignState::new::<Self>(&dh);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(&dh);
        let fifo_manager_state = FifoManagerState::new::<Self>(&dh);
        let commit_timing_manager_state = CommitTimingManagerState::new::<Self>(&dh);
        let idle_notifier_state = IdleNotifierState::new(&dh, handle.clone());
        TextInputManagerState::new::<Self>(&dh);
        InputMethodManagerState::new::<Self, _>(&dh, |_client| true);
        VirtualKeyboardManagerState::new::<Self, _>(&dh, |_client| true);
        if BackendData::HAS_RELATIVE_MOTION {
            RelativePointerManagerState::new::<Self>(&dh);
        }
        PointerConstraintsState::new::<Self>(&dh);
        if BackendData::HAS_GESTURES {
            PointerGesturesState::new::<Self>(&dh);
        }
        TabletManagerState::new::<Self>(&dh);
        SecurityContextState::new::<Self, _>(&dh, |client| {
            client
                .get_data::<ClientState>()
                .is_none_or(|client_state| client_state.security_context.is_none())
        });
        FixesState::new::<Self>(&dh);
        IdleNotifierState::new(&dh, handle.clone());

        let seat_name = backend_data.seat_name();
        let mut seat = seat_state.new_wl_seat(&dh, seat_name.clone());

        let pointer = seat.add_pointer();
        seat.add_keyboard(XkbConfig::default(), 200, 25)
            .expect("Failed to initialize the keyboard");

        let keyboard_shortcuts_inhibit_state = KeyboardShortcutsInhibitState::new::<Self>(&dh);

        #[cfg(feature = "xwayland")]
        let xwayland_shell_state = xwayland_shell::XWaylandShellState::new::<Self>(&dh.clone());

        #[cfg(feature = "xwayland")]
        XWaylandKeyboardGrabState::new::<Self>(&dh.clone());

        let start_time = Instant::now();

        #[cfg(feature = "gecko-window-manager")]
        let window_manager = Arc::new(RwLock::new(WindowStateManager::new()));

        #[cfg(feature = "hotplug")]
        let hotplug_manager = Arc::new(RwLock::new(HotPlugManager::new()));

        #[cfg(feature = "advanced-input")]
        let input_manager = Arc::new(RwLock::new(AdvancedInputManager::new()));

        #[cfg(feature = "echo-ipc")]
        let echo_service = {
            match GeckoEchoService::new() {
                Ok(service) => {
                    info!("✓ Echo service created");
                    if let Err(e) = service.register() {
                        warn!("Failed to register Echo service: {}", e);
                    } else {
                        info!("✓ Echo service registered with daemon");
                        if let Err(e) = service.signal_ready() {
                            warn!("Failed to signal Echo ready: {}", e);
                        } else {
                            info!("✓ Echo ready signal sent");
                        }
                    }
                    Arc::new(RwLock::new(service))
                }
                Err(e) => {
                    warn!("Failed to create Echo service: {}", e);
                    warn!("Continuing without Echo IPC support");
                    Arc::new(RwLock::new(GeckoEchoService::default()))
                }
            }
        };

        #[cfg(all(feature = "echo-ipc", feature = "gecko-surface"))]
        let gecko_surfaces = Arc::new(RwLock::new(HashMap::new()));

        let workspace_manager = WorkspaceManager::new();
        let space = Space::default();

        #[cfg(feature = "screencopy")]
        let screencopy_state = ScreencopyManagerState::new::<Self>(&dh);

        #[cfg(feature = "echo-ipc")]
        {
            let wayland_display =
                std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());

            echo_service.read().set_create_window_handler({
                let display = wayland_display.clone();
                move |req: &CreateWindowRequest| -> Result<u32, anyhow::Error> {
                    debug!(
                        "CreateWindow request: app_id='{}', app_type='{}', width={}, height={}",
                        req.app_id, req.app_type, req.width, req.height
                    );

                    let executable = match req.app_id.as_str() {
                        "gaia" | "gaia-shell" => "/usr/bin/gaia-shell".to_string(),
                        "terminal" => "/usr/bin/geeko-terminal".to_string(),

                        // Add test applications
                        "test.gaia-app" => "/usr/bin/firefox".to_string(), // ✅ Test with Firefox
                        "test" => "/usr/bin/firefox".to_string(),          // ✅ Or just "test"

                        app_id if app_id.starts_with("native:") => {
                            let name = app_id.trim_start_matches("native:");
                            format!("/usr/bin/{}", name)
                        }

                        // For Gaia apps, try to launch them through gaia-shell
                        app_id if app_id.contains(".gaia-app") => "/usr/bin/gaia-shell".to_string(),

                        app_id => {
                            warn!(
                                "Unknown app_id: '{}', defaulting to Firefox for testing",
                                app_id
                            );
                            "/usr/bin/firefox".to_string() // ✅ Default test app
                        }
                    };

                    let mut cmd = std::process::Command::new(&executable);
                    cmd.env("WAYLAND_DISPLAY", &display);
                    cmd.env("GAIA_WIDTH", req.width.to_string());
                    cmd.env("GAIA_HEIGHT", req.height.to_string());

                    // Pass the app info to the launched app
                    cmd.env("GAIA_APP_ID", &req.app_id);
                    cmd.env("GAIA_APP_TYPE", &req.app_type);

                    if let Some(url) = &req.url {
                        cmd.env("GAIA_URL", url);
                    }

                    match cmd.spawn() {
                        Ok(child) => {
                            info!(
                                "✓ Spawned {} (PID: {}) for app_id: {}",
                                executable,
                                child.id(),
                                req.app_id
                            );
                            Ok(0)
                        }
                        Err(e) => {
                            error!("✗ Failed to spawn {} for {}: {}", executable, req.app_id, e);
                            Err(anyhow::anyhow!("Failed to spawn {}: {}", req.app_id, e))
                        }
                    }
                }
            });
        }

        EvoState {
            backend_data,
            display_handle: dh,
            socket_name,
            running: Arc::new(AtomicBool::new(true)),
            handle,
            space,
            popups: PopupManager::default(),
            compositor_state,
            data_device_state,
            layer_shell_state,
            output_manager_state,
            primary_selection_state,
            data_control_state,
            seat_state,
            keyboard_shortcuts_inhibit_state,
            shm_state,
            viewporter_state,
            xdg_activation_state,
            xdg_decoration_state,
            xdg_shell_state,
            presentation_state,
            fractional_scale_manager_state,
            xdg_foreign_state,
            single_pixel_buffer_state,
            fifo_manager_state,
            commit_timing_manager_state,
            idle_notifier_state,
            dnd_icon: None,
            suppressed_keys: Vec::new(),
            cursor_status: CursorImageStatus::default_named(),
            seat_name,
            seat,
            pointer,
            clock,

            #[cfg(feature = "xwayland")]
            xwayland_shell_state,
            #[cfg(feature = "xwayland")]
            xwm: None,
            #[cfg(feature = "xwayland")]
            xdisplay: None,
            #[cfg(feature = "debug")]
            renderdoc: renderdoc::RenderDoc::new().ok(),
            show_window_preview: false,
            start_time,
            #[cfg(feature = "gecko-window-manager")]
            window_manager,
            #[cfg(feature = "hotplug")]
            hotplug_manager,
            #[cfg(feature = "advanced-input")]
            input_manager,
            #[cfg(feature = "echo-ipc")]
            echo_service,
            #[cfg(all(feature = "echo-ipc", feature = "gecko-surface"))]
            gecko_surfaces,
            #[cfg(all(not(feature = "echo-ipc"), feature = "gecko-surface"))]
            gecko_surfaces: Arc::new(RwLock::new(HashMap::new())),
            workspace_manager,
            #[cfg(feature = "screencopy")]
            screencopy_state,

            #[cfg(feature = "echo-ipc")]
            next_window_id: AtomicU32::new(1),
            #[cfg(feature = "echo-ipc")]
            windows_by_id: HashMap::new(),
            #[cfg(feature = "echo-ipc")]
            window_ids_by_element: HashMap::new(),
        }
    }

    #[cfg(feature = "xwayland")]
    pub fn start_xwayland(&mut self) {
        use smithay::wayland::compositor::CompositorHandler;
        use std::process::Stdio;

        let (xwayland, client) = XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            true,
            Stdio::null(),
            Stdio::null(),
            |_| (),
        )
        .expect("failed to start XWayland");

        let display_handle = self.display_handle.clone();
        let ret = self
            .handle
            .insert_source(xwayland, move |event, _, data| match event {
                XWaylandEvent::Ready {
                    x11_socket,
                    display_number,
                } => {
                    let xwayland_scale = std::env::var("EVO_XWAYLAND_SCALE")
                        .ok()
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(1.);
                    data.client_compositor_state(&client)
                        .set_client_scale(xwayland_scale);
                    let mut wm = X11Wm::start_wm(
                        data.handle.clone(),
                        &display_handle,
                        x11_socket,
                        client.clone(),
                    )
                    .expect("Failed to attach X11 Window Manager");

                    let cursor = Cursor::load();
                    let image = cursor.get_image(1, Duration::ZERO);
                    wm.set_cursor(
                        &image.pixels_rgba,
                        Size::from((image.width as u16, image.height as u16)),
                        Point::from((image.xhot as u16, image.yhot as u16)),
                    )
                    .expect("Failed to set xwayland default cursor");
                    data.xwm = Some(wm);
                    data.xdisplay = Some(display_number);
                }
                XWaylandEvent::Error => {
                    warn!("XWayland crashed on startup");
                }
            });
        if let Err(e) = ret {
            tracing::error!(
                "Failed to insert the XWaylandSource into the event loop: {}",
                e
            );
        }
    }
}

impl<BackendData: Backend + 'static> EvoState<BackendData> {
    #[cfg(feature = "echo-ipc")]
    fn allocate_window_id(&self) -> u32 {
        self.next_window_id.fetch_add(1, Ordering::Relaxed)
    }

    #[cfg(feature = "echo-ipc")]
    pub fn register_window(&mut self, window: WindowElement) -> u32 {
        let id = self.allocate_window_id();
        self.window_ids_by_element.insert(window.0.clone(), id);
        self.windows_by_id.insert(id, window.clone());
        id
    }

    #[cfg(feature = "echo-ipc")]
    pub fn unregister_window(&mut self, window: &WindowElement) -> Option<u32> {
        if let Some(id) = self.window_ids_by_element.remove(&window.0) {
            self.windows_by_id.remove(&id);
            Some(id)
        } else {
            None
        }
    }

    #[cfg(feature = "echo-ipc")]
    pub fn window_id_for(&self, window: &WindowElement) -> Option<u32> {
        self.window_ids_by_element.get(&window.0).copied()
    }

    pub fn pre_repaint(&mut self, output: &Output, frame_target: impl Into<Time<Monotonic>>) {
        let frame_target = frame_target.into();

        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();
        self.space.elements().for_each(|window| {
            window.with_surfaces(|surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                {
                    commit_timer_state.signal_until(frame_target);
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        });
        let map = smithay::desktop::layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.with_surfaces(|surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                {
                    commit_timer_state.signal_until(frame_target);
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        }
        std::mem::drop(map);

        if let CursorImageStatus::Surface(ref surface) = self.cursor_status {
            with_surfaces_surface_tree(surface, |surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                {
                    commit_timer_state.signal_until(frame_target);
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        }

        if let Some(surface) = self.dnd_icon.as_ref().map(|icon| &icon.surface) {
            with_surfaces_surface_tree(surface, |surface, states| {
                if let Some(mut commit_timer_state) = states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .map(|commit_timer| commit_timer.lock().unwrap())
                {
                    commit_timer_state.signal_until(frame_target);
                    let client = surface.client().unwrap();
                    clients.insert(client.id(), client);
                }
            });
        }

        let dh = self.display_handle.clone();
        for client in clients.into_values() {
            self.client_compositor_state(&client)
                .blocker_cleared(self, &dh);
        }
    }

    pub fn post_repaint(
        &mut self,
        output: &Output,
        time: impl Into<Duration>,
        dmabuf_feedback: Option<SurfaceDmabufFeedback>,
        render_element_states: &RenderElementStates,
    ) {
        let time = time.into();
        let throttle = Some(Duration::from_secs(1));

        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();

        self.space.elements().for_each(|window| {
            window.with_surfaces(|surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);

                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fraction_scale| {
                        fraction_scale
                            .set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }

                if primary_scanout_output
                    .as_ref()
                    .map(|o| o == output)
                    .unwrap_or(true)
                {
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();

                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                }
            });

            if self.space.outputs_for_element(window).contains(output) {
                window.send_frame(output, time, throttle, surface_primary_scanout_output);
                if let Some(dmabuf_feedback) = dmabuf_feedback.as_ref() {
                    window.send_dmabuf_feedback(
                        output,
                        surface_primary_scanout_output,
                        |surface, _| {
                            select_dmabuf_feedback(
                                surface,
                                render_element_states,
                                &dmabuf_feedback.render_feedback,
                                &dmabuf_feedback.scanout_feedback,
                            )
                        },
                    );
                }
            }
        });
        let map = smithay::desktop::layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.with_surfaces(|surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);

                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fraction_scale| {
                        fraction_scale
                            .set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }

                if primary_scanout_output
                    .as_ref()
                    .map(|o| o == output)
                    .unwrap_or(true)
                {
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();

                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                }
            });

            layer_surface.send_frame(output, time, throttle, surface_primary_scanout_output);
            if let Some(dmabuf_feedback) = dmabuf_feedback.as_ref() {
                layer_surface.send_dmabuf_feedback(
                    output,
                    surface_primary_scanout_output,
                    |surface, _| {
                        select_dmabuf_feedback(
                            surface,
                            render_element_states,
                            &dmabuf_feedback.render_feedback,
                            &dmabuf_feedback.scanout_feedback,
                        )
                    },
                );
            }
        }
        std::mem::drop(map);

        if let CursorImageStatus::Surface(ref surface) = self.cursor_status {
            with_surfaces_surface_tree(surface, |surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);

                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fraction_scale| {
                        fraction_scale
                            .set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }

                if primary_scanout_output
                    .as_ref()
                    .map(|o| o == output)
                    .unwrap_or(true)
                {
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();

                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                }
            });
        }

        if let Some(surface) = self.dnd_icon.as_ref().map(|icon| &icon.surface) {
            with_surfaces_surface_tree(surface, |surface, states| {
                let primary_scanout_output = surface_primary_scanout_output(surface, states);

                if let Some(output) = primary_scanout_output.as_ref() {
                    with_fractional_scale(states, |fraction_scale| {
                        fraction_scale
                            .set_preferred_scale(output.current_scale().fractional_scale());
                    });
                }

                if primary_scanout_output
                    .as_ref()
                    .map(|o| o == output)
                    .unwrap_or(true)
                {
                    let fifo_barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();

                    if let Some(fifo_barrier) = fifo_barrier {
                        fifo_barrier.signal();
                        let client = surface.client().unwrap();
                        clients.insert(client.id(), client);
                    }
                }
            });
        }

        let dh = self.display_handle.clone();
        for client in clients.into_values() {
            self.client_compositor_state(&client)
                .blocker_cleared(self, &dh);
        }
    }

    pub fn window_count(&self) -> usize {
        self.space.elements().count()
    }

    pub fn window_for_surface(&self, surface: WlSurface) -> Option<WindowElement> {
        if let Some(window) = self
            .space
            .elements()
            .find(|w| w.wl_surface().as_deref() == Some(&surface))
            .cloned()
        {
            return Some(window);
        }

        self.space
            .elements()
            .find(|window| {
                if let Some(root) = window.wl_surface() {
                    let mut found = false;
                    with_surfaces_surface_tree(&root, |s, _| {
                        if &surface == s {
                            found = true;
                        }
                    });
                    found
                } else {
                    false
                }
            })
            .cloned()
    }

    #[cfg(feature = "gecko-window-manager")]
    pub fn window_manager(&self) -> Arc<RwLock<WindowStateManager>> {
        self.window_manager.clone()
    }

    #[cfg(feature = "hotplug")]
    pub fn hotplug_manager(&self) -> Arc<RwLock<HotPlugManager>> {
        self.hotplug_manager.clone()
    }

    #[cfg(feature = "echo-ipc")]
    pub fn echo_service(&self) -> Arc<RwLock<GeckoEchoService>> {
        self.echo_service.clone()
    }

    #[cfg(feature = "advanced-input")]
    pub fn input_manager(&self) -> Arc<RwLock<AdvancedInputManager>> {
        self.input_manager.clone()
    }

    pub fn switch_workspace(&mut self, workspace_id: WorkspaceId) -> bool {
        let current_id = self.workspace_manager.active_workspace_id();
        if !self.workspace_manager.switch_to(workspace_id) {
            return false;
        }

        info!(
            "Switching from workspace {:?} to {:?}",
            current_id, workspace_id
        );

        if let Some(old_workspace) = self.workspace_manager.workspace(current_id) {
            for window in old_workspace.windows() {
                self.space.unmap_elem(window);
                debug!("Unmapped window from Space");
            }
        }

        if let Some(new_workspace) = self.workspace_manager.workspace(workspace_id) {
            for window in new_workspace.visible_windows() {
                if let Some(state) = new_workspace.window_state(window) {
                    if !state.minimized {
                        self.space
                            .map_element(window.clone(), state.location, false);
                        debug!("Mapped window to Space at {:?}", state.location);
                    }
                }
            }

            for window in self.workspace_manager.sticky_windows() {
                if let Some(location) = self.workspace_manager.window_location(window) {
                    self.space.map_element((*window).clone(), location, false);
                }
            }
        }

        #[cfg(feature = "echo-ipc")]
        {
            if let Some(ws) = self.workspace_manager.workspace(workspace_id) {
                let _ = self
                    .echo_service
                    .read()
                    .emit_workspace_changed(workspace_id.0, &ws.name);
            }
        }

        for output in self.space.outputs() {
            self.backend_data.reset_buffers(output);
        }

        true
    }

    pub fn switch_to_next_workspace(&mut self) {
        let old_id = self.workspace_manager.active_workspace_id();
        self.workspace_manager.switch_to_next();
        let new_id = self.workspace_manager.active_workspace_id();

        if old_id != new_id {
            self.switch_workspace(new_id);
        }
    }

    pub fn switch_to_previous_workspace(&mut self) {
        let old_id = self.workspace_manager.active_workspace_id();
        self.workspace_manager.switch_to_previous();
        let new_id = self.workspace_manager.active_workspace_id();

        if old_id != new_id {
            self.switch_workspace(new_id);
        }
    }

    pub fn add_window_to_workspace(
        &mut self,
        window: WindowElement,
        location: Point<i32, Logical>,
    ) {
        let workspace_id = self.workspace_manager.active_workspace_id();

        self.workspace_manager.add_window(window.clone(), location);
        self.space.map_element(window.clone(), location, true);

        debug!(
            "Added window to workspace {:?} at {:?}",
            workspace_id, location
        );

        #[cfg(feature = "echo-ipc")]
        {
            if let Some(_wl_surface) = window.wl_surface() {
                if let Some(toplevel) = window.0.toplevel() {
                    let geometry = window.geometry();
                    let id = self.register_window(window.clone());

                    // ✅ Get app_id from the surface's XdgToplevelSurfaceData
                    let app_id = with_states(toplevel.wl_surface(), |states| {
                        use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

                        states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .and_then(|data| {
                                let data = data.lock().unwrap();
                                data.app_id.clone()
                            })
                            .unwrap_or_else(|| "unknown".to_string())
                    });

                    let _ = self.echo_service.read().emit_window_created(
                        id,
                        &app_id,
                        geometry.size.w as u32,
                        geometry.size.h as u32,
                    );

                    // Bind the freshly mapped Wayland surface to its AppHost
                    // session. Same-process call — AppHost is a global —
                    // so this is cheap and safe to issue from the
                    // compositor's hot path. We pass `pid: None` because
                    // `WindowElement` doesn't surface the client pid; the
                    // session that LaunchApp created already carries it.
                    geckoOS_echo::AppHost::global().bind_surface(
                        &app_id,
                        id,
                        None,
                        None,
                    );
                }
            }
        }
    }

    pub fn move_window_to_workspace(
        &mut self,
        window: &WindowElement,
        target_workspace: WorkspaceId,
    ) {
        let current_workspace = self.workspace_manager.active_workspace_id();

        if self
            .workspace_manager
            .move_window_to_workspace(window, target_workspace)
        {
            if current_workspace != target_workspace {
                self.space.unmap_elem(window);
                debug!("Unmapped window when moving to different workspace");
            }

            info!("Moved window to workspace {:?}", target_workspace);
        }
    }

    pub fn remove_window_from_workspace(&mut self, window: &WindowElement) {
        self.workspace_manager.remove_window(window);
        self.space.unmap_elem(window);
        debug!("Removed window from workspace and Space");

        #[cfg(feature = "echo-ipc")]
        {
            if let Some(id) = self.unregister_window(window) {
                let _ = self.echo_service.read().emit_window_closed(id);
                // Unbind from the AppHost session so its `surfaces` list
                // and `AppSurfaceUnmapped` signal stay in sync.
                let _ = geckoOS_echo::AppHost::global().unbind_surface(id);
            }
        }
    }

    pub fn set_window_location(&mut self, window: &WindowElement, location: Point<i32, Logical>) {
        self.workspace_manager.set_window_location(window, location);

        if self.space.elements().any(|w| w == window) {
            self.space.map_element(window.clone(), location, false);
            debug!("Updated window location to {:?}", location);
        }

        #[cfg(feature = "echo-ipc")]
        if let Some(id) = self.window_id_for(window) {
            let _ = self
                .echo_service
                .read()
                .emit_window_moved(id, location.x, location.y);
        }
    }

    pub fn raise_window(&mut self, window: &WindowElement) {
        self.workspace_manager.raise_window(window);
        self.space.raise_element(window, true);
        debug!("Raised window to top");
    }

    pub fn minimize_window(&mut self, window: &WindowElement) {
        self.workspace_manager.minimize_window(window);
        self.space.unmap_elem(window);
        info!("Minimized window");
    }

    pub fn unminimize_window(&mut self, window: &WindowElement) {
        self.workspace_manager.unminimize_window(window);

        if let Some(ws_id) = self.workspace_manager.workspace_for_window(window) {
            if ws_id == self.workspace_manager.active_workspace_id() {
                if let Some(location) = self.workspace_manager.window_location(window) {
                    self.space.map_element(window.clone(), location, false);
                    info!("Unminimized window");
                }
            }
        }
    }

    pub fn toggle_window_sticky(&mut self, window: &WindowElement) {
        self.workspace_manager.toggle_sticky(window);

        if let Some(ws_id) = self.workspace_manager.workspace_for_window(window) {
            if let Some(workspace) = self.workspace_manager.workspace(ws_id) {
                if let Some(state) = workspace.window_state(window) {
                    if state.sticky {
                        if let Some(location) = self.workspace_manager.window_location(window) {
                            self.space.map_element(window.clone(), location, false);
                            info!("Window is now sticky (visible on all workspaces)");
                        }
                    }
                }
            }
        }
    }

    pub fn active_workspace_id(&self) -> WorkspaceId {
        self.workspace_manager.active_workspace_id()
    }

    pub fn active_workspace_windows(&self) -> Vec<&WindowElement> {
        self.workspace_manager.active_visible_windows()
    }
}

impl<BackendData: Backend + 'static> EvoState<BackendData> {
    pub fn compute_new_window_location(
        &self,
        pointer_location: Point<f64, Logical>,
        window: &WindowElement,
    ) -> Point<i32, Logical> {
        use rand::distributions::{Distribution, Uniform};

        let output = self
            .space
            .output_under(pointer_location)
            .next()
            .or_else(|| self.space.outputs().next())
            .cloned();

        let output_geometry = output
            .and_then(|o| {
                let geo = self.space.output_geometry(&o)?;
                let map = smithay::desktop::layer_map_for_output(&o);
                let zone = map.non_exclusive_zone();
                Some(Rectangle::new(geo.loc + zone.loc, zone.size))
            })
            .unwrap_or_else(|| Rectangle::from_size((800, 800).into()));

        // Set initial toplevel bounds
        #[allow(irrefutable_let_patterns)]
        if let Some(toplevel) = window.0.toplevel() {
            toplevel.with_pending_state(|state| {
                state.bounds = Some(output_geometry.size);
            });
        }

        let max_x = output_geometry.loc.x + (((output_geometry.size.w as f32) / 3.0) * 2.0) as i32;
        let max_y = output_geometry.loc.y + (((output_geometry.size.h as f32) / 3.0) * 2.0) as i32;
        let x_range = Uniform::new(output_geometry.loc.x, max_x);
        let y_range = Uniform::new(output_geometry.loc.y, max_y);
        let mut rng = rand::thread_rng();
        let x = x_range.sample(&mut rng);
        let y = y_range.sample(&mut rng);

        Point::from((x, y))
    }
}

pub fn update_primary_scanout_output(
    space: &Space<WindowElement>,
    output: &Output,
    dnd_icon: &Option<DndIcon>,
    cursor_status: &CursorImageStatus,
    render_element_states: &RenderElementStates,
) {
    space.elements().for_each(|window| {
        window.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    });
    let map = smithay::desktop::layer_map_for_output(output);
    for layer_surface in map.layers() {
        layer_surface.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }

    if let CursorImageStatus::Surface(ref surface) = cursor_status {
        with_surfaces_surface_tree(surface, |surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }

    if let Some(surface) = dnd_icon.as_ref().map(|icon| &icon.surface) {
        with_surfaces_surface_tree(surface, |surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }
}

#[derive(Debug, Clone)]
pub struct SurfaceDmabufFeedback {
    pub render_feedback: DmabufFeedback,
    pub scanout_feedback: DmabufFeedback,
}

#[profiling::function]
pub fn take_presentation_feedback(
    output: &Output,
    space: &Space<WindowElement>,
    render_element_states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut output_presentation_feedback = OutputPresentationFeedback::new(output);

    space.elements().for_each(|window| {
        if space.outputs_for_element(window).contains(output) {
            window.take_presentation_feedback(
                &mut output_presentation_feedback,
                surface_primary_scanout_output,
                |surface, _| {
                    surface_presentation_feedback_flags_from_states(surface, render_element_states)
                },
            );
        }
    });
    let map = smithay::desktop::layer_map_for_output(output);
    for layer_surface in map.layers() {
        layer_surface.take_presentation_feedback(
            &mut output_presentation_feedback,
            surface_primary_scanout_output,
            |surface, _| {
                surface_presentation_feedback_flags_from_states(surface, render_element_states)
            },
        );
    }

    output_presentation_feedback
}

pub trait Backend {
    const HAS_RELATIVE_MOTION: bool = false;
    const HAS_GESTURES: bool = false;
    fn seat_name(&self) -> String;
    fn reset_buffers(&mut self, output: &Output);
    fn early_import(&mut self, surface: &WlSurface);
    fn update_led_state(&mut self, led_state: LedState);
}
