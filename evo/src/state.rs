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
    delegate_kde_decoration, delegate_viewporter, delegate_virtual_keyboard_manager,
    delegate_xdg_activation, delegate_xdg_decoration, delegate_xdg_shell,
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
        wayland_protocols::xdg::{
            decoration::{
                self as xdg_decoration,
                zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            },
            shell::server::xdg_toplevel,
        },
        wayland_protocols_misc::server_decoration::server::{
            org_kde_kwin_server_decoration::{
                Mode as KdeDecorationMode, OrgKdeKwinServerDecoration,
            },
            org_kde_kwin_server_decoration_manager::Mode as KdeDefaultMode,
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
            kde::decoration::{KdeDecorationHandler, KdeDecorationState},
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
use crate::shell::recede::{MinimizeStyle, RestoreStyle};
use crate::{
    focus::{KeyboardFocusTarget, PointerFocusTarget},
    shell::{WindowElement, WindowLayout, Workspace, WorkspaceId, WorkspaceManager},
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
use crate::enhancements::echo_ipc::{GeckoEchoService, WindowAction, WindowInfo};
#[cfg(feature = "screencopy")]
use crate::enhancements::screencopy::{ScreencopyHandler, ScreencopyManagerState};

#[derive(Debug, Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    pub security_context: Option<SecurityContext>,
}
impl ClientState {
    /// Whether this connection came in on Evo's own socket, as opposed to a
    /// security-context socket made for an app (see `crate::trust`).
    pub fn is_trusted(&self) -> bool {
        self.security_context.is_none()
    }
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
    /// KDE's legacy server-decoration protocol. Carried *in addition to*
    /// `xdg_decoration_state` because GTK3 — and therefore vela — only ever
    /// binds this one; GDK has no `zxdg_decoration_manager_v1` support at all
    /// (verified: `org_kde_kwin_server_decoration_manager` is the sole
    /// decoration global in libgdk-3, and libgtk-3 imports
    /// `gdk_wayland_display_prefers_ssd`, which is driven purely by this
    /// protocol's `default_mode` event).
    pub kde_decoration_state: KdeDecorationState,
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
    /// App windows, most recently focused first. Alt+Tab walks it; unlike
    /// the Space's stacking order it still holds minimized windows.
    pub focus_history: Vec<WindowElement>,
    /// The Alt+Tab in progress (shell/switcher.rs), by window id: the order
    /// it froze when it started, so the focus changes it makes don't
    /// reshuffle the list under it. Cleared when Alt is released.
    pub alt_tab: Option<crate::shell::switcher::Switcher<u32>>,
    /// Super went down and nothing else has happened since, so releasing it
    /// is a tap (the launcher), not the start of a chord.
    pub super_tap: bool,
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

    #[cfg(feature = "echo-ipc")]
    pub echo_service: Arc<RwLock<GeckoEchoService>>,

    pub start_time: Instant,
    pub workspace_manager: WorkspaceManager,

    /// When `sync_toplevels` last published the window list.
    #[cfg(feature = "echo-ipc")]
    last_toplevel_sync: Instant,

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

        let focused_window = wl_surface
            .as_ref()
            .and_then(|surface| self.tracked_window_for_surface(surface));
        if let Some(window) = &focused_window {
            self.note_focused(window);
        }
        // Header tint and shadow depth crossfade between active and inactive.
        crate::anim::depth::focus_changed(focused_window.as_ref());
        self.note_overlay_keyboard(focused_window.as_ref().is_some_and(|w| w.is_overlay()));

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
            // A minimized window is out of the space, so look it up where
            // minimized windows still are. `focus_window` restores it and
            // raises, activates and focuses: raising in the space alone left
            // the keyboard behind, and ignored a minimized window entirely.
            // The overlay is always on top already, and activating it would
            // mark the app being used as inactive (`focus_window` refuses it).
            if let Some(window) = self.tracked_window_for_surface(&surface) {
                self.focus_window(&window);
            }
        }
    }
}
delegate_xdg_activation!(@<BackendData: Backend + 'static> EvoState<BackendData>);

impl<BackendData: Backend> XdgDecorationHandler for EvoState<BackendData> {
    // Evo has full SSD rendering (evo/src/shell/ssd.rs's `HeaderBar`: the
    // rasterized close/maximize/minimize buttons + glass title bar, real
    // hit-testing, move/resize wiring) so its *preferred* mode is
    // ServerSide, not ClientSide — found as a real bug via an actual boot:
    // Firefox always drew its own title bar despite ssd.rs being fully
    // implemented and wired, because `new_decoration`/`unset_mode` both
    // unconditionally defaulted the negotiated mode to ClientSide. Per the
    // xdg-decoration protocol, `new_decoration` sets the compositor's own
    // pending default BEFORE the client has expressed any preference; a
    // client that supports the protocol but never explicitly calls
    // `set_client_side` (GTK/Firefox on Wayland do exactly this — they
    // accept whatever the compositor offers unless they have a specific
    // reason to override it) will simply keep whatever this handler
    // defaults to. Defaulting to ClientSide here meant Evo was silently
    // opting every window into CSD before the client ever got a say, which
    // is what `xdg.rs`'s `is_ssd = configure.state.decoration_mode == ...
    // ServerSide` read back as `false` on ack. `request_mode` (a client's
    // EXPLICIT ask) is unaffected by this fix and still honors
    // `ClientSide` if a client genuinely insists on drawing its own chrome.
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        debug!(
            "xdg new_decoration: client bound zxdg_toplevel_decoration_v1, defaulting to ServerSide"
        );
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });

        // Real bug found via a real boot (2026-07-08): every ack_configure
        // was observed logging `decoration_mode=None`, meaning the pending
        // ServerSide default set above never actually reached the client in
        // a configure event -- unlike `request_mode`/`unset_mode` just
        // below, this handler was missing the matching
        // `send_pending_configure()` call. If the client creates its
        // zxdg_toplevel_decoration_v1 object AFTER the initial configure
        // already went out (a legitimate ordering the protocol allows),
        // the pending value above just sits unflushed forever with no
        // further configure to carry it, so the client never learns the
        // compositor's ServerSide default and neither side's decoration
        // state (Evo's is_ssd nor GTK's own CSD suppression) is properly
        // negotiated.
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;

        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(match mode {
                DecorationMode::ClientSide => Mode::ClientSide,
                _ => Mode::ServerSide,
            });
        });

        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });

        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}
delegate_xdg_decoration!(@<BackendData: Backend + 'static> EvoState<BackendData>);

/// The decoration mode last negotiated over KDE's server-decoration protocol,
/// stashed in the surface's data map.
///
/// Needed because that protocol is entirely separate from xdg-shell: the mode
/// is agreed on the `org_kde_kwin_server_decoration` object, but the place
/// that has to *act* on it is `xdg.rs::ack_configure`, which only sees the
/// `WlSurface`. `None` means the client never negotiated (so: no SSD).
#[derive(Debug, Default)]
pub struct KdeDecorationUserData(pub std::sync::Mutex<Option<KdeDecorationMode>>);

/// Reads back what [`KdeDecorationUserData`] recorded for `surface`.
pub fn kde_decoration_mode(surface: &WlSurface) -> Option<KdeDecorationMode> {
    smithay::wayland::compositor::with_states(surface, |states| {
        states
            .data_map
            .get::<KdeDecorationUserData>()
            .and_then(|d| *d.0.lock().unwrap())
    })
}

// The decoration protocol vela actually speaks. See `kde_decoration_state`'s
// field doc: the `XdgDecorationHandler` above is dead code for any GTK client,
// because GDK never binds `zxdg_decoration_manager_v1`.
impl<BackendData: Backend> KdeDecorationHandler for EvoState<BackendData> {
    fn kde_decoration_state(&self) -> &KdeDecorationState {
        &self.kde_decoration_state
    }

    fn new_decoration(&mut self, surface: &WlSurface, _decoration: &OrgKdeKwinServerDecoration) {
        // Deliberately does NOT announce a mode here. The manager's
        // `default_mode` event (sent eagerly on bind) has already told the
        // client we prefer Server, and GDK only creates this object *after*
        // it has made up its mind — it follows `create` immediately with its
        // own `request_mode`. Answering here races that request and loses:
        // observed live as a Server/Client ping-pong
        // (`request_mode(1)` -> `mode(2)` -> `request_mode(1)` -> `mode(1)`)
        // before settling. Let `request_mode` be the only place a mode is
        // ever sent.
        debug!(
            ?surface,
            "kde new_decoration: client created org_kde_kwin_server_decoration"
        );
    }

    fn request_mode(
        &mut self,
        surface: &WlSurface,
        decoration: &OrgKdeKwinServerDecoration,
        mode: smithay::reexports::wayland_server::WEnum<KdeDecorationMode>,
    ) {
        use smithay::reexports::wayland_server::WEnum;
        debug!(?surface, ?mode, "kde request_mode");

        // Echo back exactly what the client asked for, per the protocol's
        // "compositor must prevent feedback loops" note — do NOT re-assert
        // Server here, or a client that insists on Client mode will ping-pong
        // with us forever.
        if let WEnum::Value(mode) = mode {
            decoration.mode(mode);
            smithay::wayland::compositor::with_states(surface, |states| {
                *states
                    .data_map
                    .get_or_insert_threadsafe(KdeDecorationUserData::default)
                    .0
                    .lock()
                    .unwrap() = Some(mode);
            });
        }
    }
}
delegate_kde_decoration!(@<BackendData: Backend + 'static> EvoState<BackendData>);

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

    fn screencopy_supported(&self) -> bool {
        BackendData::HAS_SCREENCOPY
    }
}
#[cfg(feature = "screencopy")]
crate::delegate_screencopy!(@<BackendData: Backend + 'static> EvoState<BackendData>);

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
            // Bind the EXACT socket name Evo advertises via WAYLAND_DISPLAY
            // (main.rs sets this env, defaulting to "wayland-0"), NOT
            // new_auto(). new_auto() calls bind_auto("wayland", 1..33) whose
            // range STARTS AT 1, so it can never produce wayland-0 -- it
            // always yields wayland-1 (or higher). That desync -- Evo logging
            // "Wayland socket: wayland-0" and setting WAYLAND_DISPLAY=wayland-0
            // in its env, but actually binding wayland-1 -- is exactly why a
            // client told WAYLAND_DISPLAY=wayland-0 (e.g. vela launched
            // from geckoOS-boot) failed with "Failed to connect to Wayland
            // display '/run/user/1000/wayland-0' ... No such file or
            // directory". Found via a real boot's evo.log, not by inspection.
            let desired =
                std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
            let source = match ListeningSocketSource::with_name(&desired) {
                Ok(s) => s,
                Err(err) => {
                    // The requested name was somehow already taken (stale
                    // socket, a second compositor, ...). Rather than hard-fail
                    // startup, fall back to auto-naming -- but then update
                    // WAYLAND_DISPLAY below so children still get the real
                    // name.
                    warn!(
                        "Could not bind wayland socket '{}' ({}); falling back to auto-naming",
                        desired, err
                    );
                    ListeningSocketSource::new_auto().unwrap()
                }
            };
            let socket_name = source.socket_name().to_string_lossy().into_owned();
            // Keep this process's WAYLAND_DISPLAY in sync with the socket we
            // ACTUALLY bound, so any child Evo spawns (XWayland, etc.) and
            // anything else reading this env sees the real name even if the
            // fallback above fired.
            std::env::set_var("WAYLAND_DISPLAY", &socket_name);
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
        // A layer surface can cover the screen above every window and take the
        // keyboard exclusively: shell territory.
        let layer_shell_state =
            WlrLayerShellState::new_with_filter::<Self, _>(&dh, crate::trust::trusted_only);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let data_control_state =
            // Data-control reads and writes the clipboard with no focus at all.
            DataControlState::new::<Self, _>(
                &dh,
                Some(&primary_selection_state),
                crate::trust::trusted_only,
            );
        let mut seat_state = SeatState::new();
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let xdg_activation_state = XdgActivationState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        // `Server` is what makes GDK's `gdk_wayland_display_prefers_ssd()`
        // return true, which in turn makes `gtk_window_should_use_csd()`
        // false — i.e. this is the one lever that stops GTK adding its own
        // titlebar widget to a vela toplevel. Smithay sends `default_mode`
        // eagerly on bind, so the client learns it before it maps anything.
        let kde_decoration_state = KdeDecorationState::new::<Self>(&dh, KdeDefaultMode::Server);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let presentation_state = PresentationState::new::<Self>(&dh, clock.id() as u32);
        let fractional_scale_manager_state = FractionalScaleManagerState::new::<Self>(&dh);
        let xdg_foreign_state = XdgForeignState::new::<Self>(&dh);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(&dh);
        let fifo_manager_state = FifoManagerState::new::<Self>(&dh);
        let commit_timing_manager_state = CommitTimingManagerState::new::<Self>(&dh);
        let idle_notifier_state = IdleNotifierState::new(&dh, handle.clone());
        TextInputManagerState::new::<Self>(&dh);
        // Synthetic input and input methods act on whatever has the keyboard,
        // including the root Terminal: not for apps.
        InputMethodManagerState::new::<Self, _>(&dh, crate::trust::trusted_only);
        VirtualKeyboardManagerState::new::<Self, _>(&dh, crate::trust::trusted_only);
        if BackendData::HAS_RELATIVE_MOTION {
            RelativePointerManagerState::new::<Self>(&dh);
        }
        PointerConstraintsState::new::<Self>(&dh);
        if BackendData::HAS_GESTURES {
            PointerGesturesState::new::<Self>(&dh);
        }
        TabletManagerState::new::<Self>(&dh);
        // Only the system mints contexts, or an app could make itself a
        // trusted-looking one (and could mint one with any app id).
        SecurityContextState::new::<Self, _>(&dh, crate::trust::trusted_only);
        FixesState::new::<Self>(&dh);

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

        let workspace_manager = WorkspaceManager::new();
        let space = Space::default();

        #[cfg(feature = "screencopy")]
        let screencopy_state = ScreencopyManagerState::new::<Self, _>(&dh, crate::trust::trusted_only);

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
            kde_decoration_state,
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
            focus_history: Vec::new(),
            alt_tab: None,
            super_tap: false,
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
            #[cfg(feature = "echo-ipc")]
            last_toplevel_sync: Instant::now(),
            #[cfg(feature = "echo-ipc")]
            echo_service,
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

        let (xwayland, client) = match XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            true,
            Stdio::null(),
            Stdio::null(),
            |_| (),
        ) {
            Ok(pair) => pair,
            Err(err) => {
                // X11 app compatibility is optional -- losing it (e.g. no
                // free X11 socket dir at boot) must never take down the
                // Wayland compositor itself.
                warn!(?err, "Failed to start XWayland; X11 app support will be unavailable");
                return;
            }
        };

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

    #[cfg(feature = "echo-ipc")]
    pub fn echo_service(&self) -> Arc<RwLock<GeckoEchoService>> {
        self.echo_service.clone()
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

        // Snapshot where each window is before it leaves the space, so
        // coming back to this workspace puts it where it was, not where it
        // was born.
        let leaving: Vec<WindowElement> = self
            .workspace_manager
            .workspace(current_id)
            .map(|ws| ws.windows().to_vec())
            .unwrap_or_default();
        for window in &leaving {
            if let Some(location) = self.space.element_location(window) {
                self.workspace_manager.record_location(window, location);
            }
            // Its fullscreen slot goes with it, or the output would keep
            // drawing a window from the workspace we left.
            self.clear_exclusive_fullscreen(window);
            self.space.unmap_elem(window);
            debug!("Unmapped window from Space");
        }

        let mut fullscreen_arrivals = Vec::new();
        if let Some(new_workspace) = self.workspace_manager.workspace(workspace_id) {
            for window in new_workspace.visible_windows() {
                if let Some(state) = new_workspace.window_state(window) {
                    if !state.minimized {
                        self.space
                            .map_element(window.clone(), state.location, false);
                        debug!("Mapped window to Space at {:?}", state.location);
                        if state.layout == WindowLayout::Fullscreen {
                            fullscreen_arrivals.push(window.clone());
                        }
                    }
                }
            }

            for window in self.workspace_manager.sticky_windows() {
                if let Some(location) = self.workspace_manager.window_location(window) {
                    self.space.map_element((*window).clone(), location, false);
                }
            }
        }

        for window in fullscreen_arrivals {
            if let Some(output) = self.output_for_fullscreen(&window, None) {
                self.set_exclusive_fullscreen(&window, &output);
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
        self.workspace_manager.set_reported_location(&window, location);
        self.space.map_element(window.clone(), location, true);

        debug!(
            "Added window to workspace {:?} at {:?}",
            workspace_id, location
        );

        // Xdg toplevels and X11 windows are registered the same way, so
        // nothing downstream (ListWindows, WindowCreated/Closed, appHost)
        // can tell which kind a window is.
        #[cfg(feature = "echo-ipc")]
        {
            let geometry = window.geometry();
            let id = self.register_window(window.clone());

            // The app id is usually still unset at this point (GTK/vela set
            // it later), so this is "unknown" for most Wayland windows; the
            // X11 class is known from the start.
            let app_id = window.app_id().unwrap_or_else(|| "unknown".to_string());

            let _ = self.echo_service.read().emit_window_created(
                id,
                &app_id,
                geometry.size.w.max(0) as u32,
                geometry.size.h.max(0) as u32,
            );

            // Binding the surface to its AppHost session deliberately does
            // NOT happen here: at map time the app id is unset, so the bind
            // saw "unknown" and `AppHost::bind_surface` synthesized a junk
            // session for it (one bogus `appId: "unknown"` session holding
            // every surface, while the real app's session kept
            // `surfaces: []` and `nova.Focus` could never find a window).
            // `sync_surface_binding` does it once the id is known, over IPC
            // (not `AppHost::global()`: Evo is a separate process from
            // echo-daemon, so that is an empty AppHost nobody else sees).
            self.sync_surface_binding(&window);
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
        // Before anything is unmapped: a closing window is drawn from a
        // snapshot taken while its buffer and place still exist.
        self.begin_close_animation(window);
        // Whatever the window itself was animating ends with it. A close
        // cancels it when it makes a ghost, but a client killed mid-restore
        // has no buffer left, so no close is made, and an animation completes
        // (its count with it) only when drawn: the restore stayed active.
        window.cancel_animation();
        window.cancel_geometry();
        // A window closed while still minimizing has nothing left to draw.
        drop(crate::anim::ghosts::take_live(window));
        self.clear_exclusive_fullscreen(window);
        self.workspace_manager.remove_window(window);
        self.space.unmap_elem(window);
        debug!("Removed window from workspace and Space");

        #[cfg(feature = "echo-ipc")]
        {
            if let Some(id) = self.unregister_window(window) {
                let _ = self.echo_service.read().emit_window_closed(id);
                // Unbind from the AppHost session so its `surfaces` list
                // and `AppSurfaceUnmapped` signal stay in sync. Over IPC for
                // the same reason as the bind in `sync_surface_binding`.
                let _ = self.echo_service.read().unbind_surface_remote(id);
            }
        }

        // If it held the keyboard, someone else must. Last, so the window
        // is already out of the stack the choice is made from.
        self.refocus_after_loss(window);
    }

    /// A user's move: records the place, marks the window user-positioned,
    /// and announces it. Goes through `place_window` like everything else.
    pub fn set_window_location(&mut self, window: &WindowElement, location: Point<i32, Logical>) {
        self.workspace_manager.set_window_location(window, location);
        self.place_window(window, location, false);
        debug!("Updated window location to {:?}", location);
        self.window_moved(window);
    }

    /// Re-run orphan relocation after an output change (see
    /// `shell::fixup_positions`), and record where each relocated window went.
    pub fn fixup_positions(&mut self, pointer_location: Point<f64, Logical>) {
        for (window, location) in crate::shell::fixup_positions(&mut self.space, pointer_location) {
            self.workspace_manager.record_location(&window, location);
        }
    }

    /// Raise a window to the top of its workspace and the space, restoring
    /// it first if it was minimized. Does not move the keyboard: that is
    /// `focus_window`, which calls this.
    pub fn raise_window(&mut self, window: &WindowElement) {
        // A minimized window was unmapped from `self.space` entirely (see
        // `minimize_window`), so `space.raise_element` below -- which only
        // reorders elements the space already knows about -- is a silent
        // no-op on it. Found via a real report: clicking a minimized app's
        // dock icon (Bloom -> Nova.Focus -> this, via `FocusWindow`'s
        // `WindowAction::Focus` handling in `process_pending_window_actions`)
        // never brought the window back at all. Restoring first means every
        // caller of `raise_window` -- dock clicks, click-to-focus, whatever
        // else raises a window -- can't hit this, rather than special-casing
        // it only in the one call site that happened to surface it.
        if self.workspace_manager.is_minimized(window) {
            self.unminimize_window(window);
        }
        self.workspace_manager.raise_window(window);
        self.space.raise_element(window, true);
        debug!("Raised window to top");
    }

    /// Hide a window, keeping everything needed to bring it back exactly as
    /// it was: where the space has it now goes into the record, and its
    /// layout is already there. It stays in the workspace and in
    /// `windows_by_id`, so it can still be found, restored, listed, and
    /// closed.
    pub fn minimize_window(&mut self, window: &WindowElement) {
        self.minimize_window_after(window, Duration::ZERO);
    }

    /// `minimize_window`, with the drawing starting `delay` from now (show
    /// desktop staggers its windows). Placement is not delayed: the window is
    /// out of the Space, and focus has moved on, at once.
    pub fn minimize_window_after(&mut self, window: &WindowElement, delay: Duration) {
        self.minimize_window_as(window, MinimizeStyle::Droplet(delay));
    }

    /// `minimize_window`, drawn as `style`: a Droplet into the dock icon, or
    /// a show desktop's Recede (`shell/recede.rs`).
    pub fn minimize_window_as(&mut self, window: &WindowElement, style: MinimizeStyle) {
        // The desktop is never minimized, and an untracked window (an X11
        // menu) has no record to come back from: hiding it would lose it.
        if window.is_shell_window() || self.workspace_manager.state(window).is_none() {
            return;
        }
        if self.workspace_manager.is_minimized(window) {
            return;
        }
        let location = self.space.element_location(window);
        // Its card in the overview shows this picture while it is away.
        self.thumbnail_at_minimize(window);
        // Drawn as a ghost springing into its dock icon (or receding); taken
        // while the window is still in the Space, before it is unmapped below.
        match style {
            MinimizeStyle::Droplet(delay) => self.begin_minimize_animation(window, delay),
            MinimizeStyle::Recede(delay) => self.begin_recede_animation(window, delay),
        }
        self.workspace_manager.minimize_window(window, location);
        self.clear_exclusive_fullscreen(window);
        self.space.unmap_elem(window);
        info!("Minimized window");

        // Tell the client it is hidden: it is no longer activated, and
        // `Suspended` is the only background hint it gets (nothing stops or
        // freezes the process any more).
        match window.0.underlying_surface() {
            smithay::desktop::WindowSurface::Wayland(toplevel) => {
                toplevel.with_pending_state(|state| {
                    state.states.unset(xdg_toplevel::State::Activated);
                    state.states.set(xdg_toplevel::State::Suspended);
                });
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_pending_configure();
                }
            }
            #[cfg(feature = "xwayland")]
            smithay::desktop::WindowSurface::X11(surface) => {
                let _ = surface.set_hidden(true);
            }
        }

        #[cfg(feature = "echo-ipc")]
        if let Some(id) = self.window_id_for(window) {
            let _ = self.echo_service.read().emit_window_minimized(id, true);
        }

        self.refocus_after_loss(window);
    }

    /// Bring a minimized window back where, and as, it was: its recorded
    /// place, and maximized or fullscreen again if it was. Does not move
    /// the keyboard; `focus_window` does that.
    pub fn unminimize_window(&mut self, window: &WindowElement) {
        self.unminimize_window_as(window, RestoreStyle::Rise);
    }

    /// `unminimize_window`, drawn as `style`: a Rise out of the dock icon, or
    /// a second Super+D's Emerge (`shell/recede.rs`).
    pub fn unminimize_window_as(&mut self, window: &WindowElement, style: RestoreStyle) {
        if !self.workspace_manager.is_minimized(window) {
            return;
        }
        // A minimize still being drawn is turned round, not restarted.
        let ghost = crate::anim::ghosts::take_live(window);
        let Some((location, layout)) = self.workspace_manager.unminimize_window(window) else {
            return;
        };

        if self.workspace_manager.workspace_for_window(window) == Some(self.workspace_manager.active_workspace_id()) {
            self.space.map_element(window.clone(), location, false);
            if layout == WindowLayout::Fullscreen && !window.is_shell_window() {
                if let Some(output) = self.output_for_fullscreen(window, None) {
                    self.set_exclusive_fullscreen(window, &output);
                }
            }
            info!("Unminimized window at {:?} as {:?}", location, layout);
        }
        match style {
            RestoreStyle::Rise => self.begin_restore_animation(window, ghost),
            RestoreStyle::Emerge(delay) => self.begin_emerge_animation(window, ghost, delay),
        }

        match window.0.underlying_surface() {
            smithay::desktop::WindowSurface::Wayland(toplevel) => {
                toplevel.with_pending_state(|state| {
                    state.states.unset(xdg_toplevel::State::Suspended);
                });
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_pending_configure();
                }
            }
            #[cfg(feature = "xwayland")]
            smithay::desktop::WindowSurface::X11(surface) => {
                let _ = surface.set_hidden(false);
            }
        }

        #[cfg(feature = "echo-ipc")]
        if let Some(id) = self.window_id_for(window) {
            let _ = self.echo_service.read().emit_window_minimized(id, false);
        }
    }

    /// Restores some currently-minimized window in the active workspace and
    /// focuses it.
    /// There's no taskbar to click a specific one back yet (see
    /// `KeyAction::RestoreMinimized`, Logo+Shift+U), so this is an interim
    /// stand-in -- once a real dock exists, it should call
    /// `focus_window` directly for the window the user actually clicked.
    pub fn restore_last_minimized(&mut self) {
        let target = {
            let workspace = self.workspace_manager.active_workspace();
            workspace
                .windows()
                .iter()
                .rev()
                .find(|w| workspace.window_state(w).is_some_and(|s| s.minimized))
                .cloned()
        };
        if let Some(window) = target {
            self.focus_window(&window);
        } else {
            debug!("RestoreMinimized: no minimized windows in the active workspace");
        }
    }

    /// Drains FocusWindow/MinimizeWindow/RestoreWindow requests queued by
    /// Echo IPC (see `echo_ipc.rs`'s `handle_incoming_message`) and applies
    /// them through the same `raise_window`/`minimize_window`/
    /// `unminimize_window` machinery the internal minimize keybinding and
    /// click-to-focus input handling already use. Called once per
    /// event-loop iteration by each backend (winit/udev/x11), since the
    /// broker that receives these IPC requests runs on a separate tokio
    /// thread with no direct access to `Space`/`Seat`.
    /// Publish every live toplevel to the IPC side, for `ListWindows` and
    /// `GetRenderingStatus` (see `GeckoEchoService::toplevels`). Each backend
    /// calls this from its event loop, next to
    /// `process_pending_window_actions`. It rebuilds at most four times a
    /// second, and `set_toplevels` writes only on change.
    ///
    /// Geometry is the frame's, header bar included, at the frame's top-left.
    /// A minimized window is unmapped from the space, so it reports
    /// `visible: false` at 0,0.
    #[cfg(feature = "echo-ipc")]
    pub fn sync_toplevels(&mut self) {
        if self.last_toplevel_sync.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_toplevel_sync = Instant::now();

        let focused = self.seat.get_keyboard().and_then(|keyboard| keyboard.current_focus());
        let mut toplevels: Vec<WindowInfo> = self
            .windows_by_id
            .iter()
            .filter(|(_, window)| !window.is_overlay())
            .map(|(&window_id, window)| {
                let location = self.space.element_location(window);
                let size = window.geometry().size;
                WindowInfo {
                    window_id,
                    app_id: window.app_id().unwrap_or_default(),
                    width: size.w.max(0) as u32,
                    height: size.h.max(0) as u32,
                    x: location.map_or(0, |l| l.x),
                    y: location.map_or(0, |l| l.y),
                    focused: matches!(&focused, Some(KeyboardFocusTarget::Window(w)) if *w == window.0),
                    visible: location.is_some(),
                }
            })
            .collect();
        toplevels.sort_by_key(|w| w.window_id);
        self.echo_service.read().set_toplevels(toplevels);
    }

    #[cfg(feature = "echo-ipc")]
    pub fn process_pending_window_actions(&mut self) {
        self.switcher_tick();
        self.minimize_landed_tick();
        self.serve_thumbnail_requests();
        if self.echo_service.read().take_rebind_request() {
            self.replay_surface_bindings();
        }
        let actions = self.echo_service.read().take_pending_window_actions();
        for pending in actions {
            // The shell's own requests are about no window.
            match pending.action {
                WindowAction::SetWorkArea(insets) => {
                    self.set_work_area(insets);
                    continue;
                }
                WindowAction::OverlayKeyboard(wanted) => {
                    self.set_overlay_keyboard(wanted);
                    continue;
                }
                _ => {}
            }
            let Some(window) = self.windows_by_id.get(&pending.window_id).cloned() else {
                warn!(
                    "Ignoring {:?} for unknown window {}",
                    pending.action, pending.window_id
                );
                continue;
            };
            match pending.action {
                WindowAction::Focus => {
                    self.focus_window(&window);
                    // A card clicked while Alt+Tab is on is its commit.
                    self.switcher_chose(&window);
                }
                WindowAction::Minimize => self.minimize_window(&window),
                // Restoring a window presents it, so it takes focus too.
                WindowAction::Restore => {
                    self.focus_window(&window);
                    self.switcher_chose(&window);
                }
                // Same call the SSD close button makes (shell/ssd.rs's
                // handle_close): ask the client to close and let it run its
                // own teardown, rather than signalling the process.
                // Straight through to the same requests the SSD button
                // defers to. No insert_idle needed here: ssd.rs defers
                // because it runs while a decoration_state RefMut is still
                // held by the pointer-button path, and maximize_request
                // re-borrows it via Space::map_element. This runs from the
                // event loop with no such borrow outstanding.
                WindowAction::Maximize | WindowAction::Unmaximize => {
                    if let smithay::desktop::WindowSurface::Wayland(w) =
                        window.0.underlying_surface()
                    {
                        use smithay::wayland::shell::xdg::XdgShellHandler;
                        let surface = w.clone();
                        if matches!(pending.action, WindowAction::Maximize) {
                            XdgShellHandler::maximize_request(self, surface);
                        } else {
                            XdgShellHandler::unmaximize_request(self, surface);
                        }
                    } else {
                        warn!("{:?} is only implemented for Wayland toplevels", pending.action);
                    }
                }
                WindowAction::Close => self.close_window(&window),
                WindowAction::SetWorkArea(_) | WindowAction::OverlayKeyboard(_) => {}
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
                Some(crate::shell::work_area::usable(geo, zone))
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
    /// Whether the backend fulfils queued screencopy captures in its repaint
    /// (see enhancements/screencopy.rs). Only udev does.
    const HAS_SCREENCOPY: bool = false;
    fn seat_name(&self) -> String;
    fn reset_buffers(&mut self, output: &Output);
    fn early_import(&mut self, surface: &WlSurface);
    fn update_led_state(&mut self, led_state: LedState);
    /// Draw `window` (SSD included) to pixels, now, at output scale `scale`.
    /// Needs the renderer, which only the backend holds. `None` means this
    /// backend cannot, and the window then closes without an animation. Only
    /// udev implements it today.
    fn snapshot_window(&mut self, _window: &WindowElement, _scale: f64) -> Option<crate::anim::Snapshot> {
        None
    }
}
