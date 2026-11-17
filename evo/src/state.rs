#[cfg(feature = "xwayland")]
use std::os::unix::io::OwnedFd;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use parking_lot::RwLock;
use tracing::{debug, info, warn};

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
    utils::{Clock, IsAlive, Logical, Monotonic, Point, Rectangle, Serial, Time},
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
            wlr_layer::WlrLayerShellState,
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

#[cfg(feature = "xwayland")]
use crate::cursor::Cursor;
use crate::shell::recede::{MinimizeStyle, RestoreStyle};
use crate::{
    focus::{KeyboardFocusTarget, PointerFocusTarget},
    shell::{FullscreenOutputName, WindowElement, WindowLayout, WorkspaceId, WorkspaceManager},
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
use crate::enhancements::echo_ipc::{
    GeckoEchoService, OutputConfigInfo, OutputModeInfo, WindowAction, WindowInfo,
};
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
        // A window taking the keyboard is a moment to ask whether its app
        // still answers (shell/busy.rs).
        if let Some(window) = &focused_window {
            self.ping_window(window, true);
        }
        self.note_overlay_keyboard(focused_window.as_ref().is_some_and(|w| w.is_overlay()));

        #[cfg(feature = "echo-ipc")]
        if !focused_window.as_ref().is_some_and(|w| w.is_shell_window()) {
            let workspace_id = self.workspace_manager.active_workspace_id().0;
            let focused_window_id = focused_window
                .as_ref()
                .and_then(|window| self.window_id_for(window));

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
        self.focus_history.retain(|w| w != window);
        crate::shell::recede::forget_window(window);
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
                        if state.layout == WindowLayout::Fullscreen && !window.is_shell_window() {
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
        self.focus_history.retain(|w| w != window);
        crate::shell::recede::forget_window(window);
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
    /// `shell::fixup_positions`), reconfigure fullscreen and shell windows to
    /// match the new output geometry, and refit maximized/floating windows to
    /// the updated work area.
    fn resolve_fullscreen_or_shell_output(
        &self,
        window: &WindowElement,
        fallback: &Output,
    ) -> Output {
        if let Some(name) = window
            .user_data()
            .get::<FullscreenOutputName>()
            .and_then(FullscreenOutputName::get)
        {
            if let Some(named) = self.space.outputs().find(|o| o.name() == name) {
                return named.clone();
            }
        }
        self.output_for_fullscreen(window, None)
            .unwrap_or_else(|| fallback.clone())
    }

    pub fn fixup_positions(&mut self, pointer_location: Point<f64, Logical>) {
        for (window, location) in crate::shell::fixup_positions(&mut self.space, pointer_location) {
            self.workspace_manager.record_location(&window, location);
        }

        let first_output = self.space.outputs().next().cloned();
        if let Some(first_output) = first_output {
            let mapped: Vec<WindowElement> = self.space.elements().cloned().collect();
            for window in &mapped {
                let is_shell = window.is_shell_window();
                let is_fullscreen = self
                    .workspace_manager
                    .state(window)
                    .is_some_and(|s| s.layout == WindowLayout::Fullscreen);
                if !is_shell && !is_fullscreen {
                    continue;
                }
                let target_output = self.resolve_fullscreen_or_shell_output(window, &first_output);
                self.reconfigure_fullscreen_window(window, &target_output, true);
            }

            let unmapped_fullscreen: Vec<WindowElement> = self
                .workspace_manager
                .all_windows()
                .filter(|w| {
                    w.alive()
                        && !w.is_shell_window()
                        && !mapped.contains(w)
                        && self
                            .workspace_manager
                            .state(w)
                            .is_some_and(|s| s.layout == WindowLayout::Fullscreen)
                })
                .cloned()
                .collect();
            for window in unmapped_fullscreen {
                let target_output = self.resolve_fullscreen_or_shell_output(&window, &first_output);
                self.reconfigure_fullscreen_window(&window, &target_output, false);
            }
        }

        self.refit_to_work_area();
    }

    fn reconfigure_fullscreen_window(
        &mut self,
        window: &WindowElement,
        output: &Output,
        mapped: bool,
    ) {
        let Some(output_geo) = self.space.output_geometry(output) else {
            return;
        };
        window.user_data().insert_if_missing(FullscreenOutputName::default);
        if let Some(slot) = window.user_data().get::<FullscreenOutputName>() {
            slot.set(Some(output.name()));
        }
        self.set_window_layout(window, WindowLayout::Fullscreen);
        match window.0.underlying_surface() {
            smithay::desktop::WindowSurface::Wayland(toplevel) => {
                toplevel.with_pending_state(|state| {
                    state.size = Some(output_geo.size);
                    state.states.set(xdg_toplevel::State::Fullscreen);
                });
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_pending_configure();
                }
            }
            #[cfg(feature = "xwayland")]
            smithay::desktop::WindowSurface::X11(surface) => {
                let _ = surface.configure(output_geo);
            }
        }
        self.workspace_manager.record_location(window, output_geo.loc);
        if mapped {
            self.space.map_element(window.clone(), output_geo.loc, false);
            if !window.is_shell_window() {
                self.clear_exclusive_fullscreen(window);
                self.set_exclusive_fullscreen(window, output);
            }
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
    pub fn is_window_minimized(&self, window: &WindowElement) -> bool {
        self.workspace_manager.is_minimized(window)
    }

    pub fn unminimize_window_as(&mut self, window: &WindowElement, style: RestoreStyle) {
        if !self.workspace_manager.is_minimized(window) {
            return;
        }
        // A minimize still being drawn is turned round, not restarted.
        let ghost = crate::anim::ghosts::take_live(window);
        let Some((orig_location, layout)) = self.workspace_manager.unminimize_window(window) else {
            return;
        };
        if layout == WindowLayout::Floating {
            self.fit_to_work_area_inner(window, false);
        }
        let location = self
            .workspace_manager
            .state(window)
            .map_or(orig_location, |s| s.location);

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
    fn collect_toplevel_infos(&self) -> Vec<WindowInfo> {
        let focused = self.seat.get_keyboard().and_then(|keyboard| keyboard.current_focus());
        let mut toplevels: Vec<WindowInfo> = self
            .windows_by_id
            .iter()
            // The overlay is left out (it is the shell's chrome, not a
            // window); the desktop stays, as suites read and hand it focus.
            .filter(|(_, window)| !window.is_overlay())
            .map(|(&window_id, window)| {
                let location = self.space.element_location(window);
                let size = window.geometry().size;
                let minimized = self.is_window_minimized(window);
                let layout = self
                    .workspace_manager
                    .state(window)
                    .map_or(WindowLayout::Floating, |s| s.layout);
                WindowInfo {
                    window_id,
                    app_id: window.app_id().unwrap_or_default(),
                    width: size.w.max(0) as u32,
                    height: size.h.max(0) as u32,
                    x: location.map_or(0, |l| l.x),
                    y: location.map_or(0, |l| l.y),
                    focused: matches!(&focused, Some(KeyboardFocusTarget::Window(w)) if *w == window.0),
                    visible: location.is_some(),
                    minimized,
                    maximized: layout == WindowLayout::Maximized,
                    // The desktop is fullscreen only to lose its titlebar: it
                    // sits beneath apps, and never has the output to itself.
                    fullscreen: layout == WindowLayout::Fullscreen && !window.is_shell(),
                    busy: window.is_busy(),
                    title_label: window.title_source().label.text(),
                }
            })
            .collect();
        toplevels.sort_by_key(|w| w.window_id);
        toplevels
    }

    #[cfg(feature = "echo-ipc")]
    fn collect_output_configs(&self) -> Vec<OutputConfigInfo> {
        self.space
            .outputs()
            .filter_map(|o| {
                let mode = o.current_mode()?;
                let loc = self.space.output_geometry(o).map(|g| g.loc).unwrap_or((0, 0).into());
                let modes = o
                    .modes()
                    .into_iter()
                    .map(|m| OutputModeInfo {
                        width: m.size.w,
                        height: m.size.h,
                        refresh_mhz: m.refresh,
                    })
                    .collect();
                Some(OutputConfigInfo {
                    name: o.name(),
                    description: o.description(),
                    width: mode.size.w,
                    height: mode.size.h,
                    refresh_mhz: mode.refresh,
                    scale: o.current_scale().fractional_scale(),
                    x: loc.x,
                    y: loc.y,
                    modes,
                })
            })
            .collect()
    }

    #[cfg(feature = "echo-ipc")]
    pub(crate) fn mark_toplevel_sync_dirty(&mut self) {
        self.last_toplevel_sync = Instant::now() - Duration::from_secs(1);
    }

    #[cfg(feature = "echo-ipc")]
    pub fn sync_toplevels(&mut self) {
        if self.last_toplevel_sync.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_toplevel_sync = Instant::now();

        let toplevels = self.collect_toplevel_infos();
        self.echo_service.read().set_toplevels(toplevels);

        let outputs = self.collect_output_configs();
        if crate::enhancements::echo_ipc::sync_outputs_from_space(outputs.clone()) {
            let _ = self.echo_service.read().emit_outputs_changed(&outputs);
        }
    }

    #[cfg(feature = "echo-ipc")]
    pub fn process_pending_window_actions(&mut self) {
        // First: a lock begins (and an unlock's Clear starts) before anything
        // else this pass touches the windows.
        self.lock_tick();
        self.switcher_tick();
        self.minimize_landed_tick();
        self.busy_tick();
        self.frost_tick();
        self.serve_thumbnail_requests();
        if self.echo_service.read().take_rebind_request() {
            self.replay_surface_bindings();
            // A restarted echo-daemon thinks the screen is unlocked. Evo
            // knows better, and says so, so Echo agrees (and Bloom's page,
            // asking Echo, comes up locked).
            if crate::shell::lock::input_locked() {
                if let Err(e) = self.echo_service.read().session_lock_remote() {
                    warn!("lock: could not tell the restarted Echo the screen is locked: {e:#}");
                }
            }
        }
        let actions = self.echo_service.read().take_pending_window_actions();
        for pending in actions {
            // While locked the only requests honoured are the shell/display
            // layout ones: no window may be focused, restored, closed or
            // moved by whoever asks.
            if crate::shell::lock::input_locked()
                && !matches!(
                    pending.action,
                    WindowAction::SetWorkArea(_) | WindowAction::ApplyOutputConfigs
                )
            {
                debug!("lock: refused {:?} while the screen is locked", pending.action);
                continue;
            }
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
                WindowAction::ApplyOutputConfigs => {
                    let configs = crate::enhancements::echo_ipc::get_outputs_snapshot();
                    let single_output_fallback =
                        self.space.outputs().count() == 1 && configs.len() == 1;
                    for cfg in configs {
                        let target_output = self
                            .space
                            .outputs()
                            .find(|o| o.name() == cfg.name)
                            .or_else(|| {
                                single_output_fallback
                                    .then(|| self.space.outputs().next())
                                    .flatten()
                            })
                            .cloned();
                        if let Some(output) = target_output {
                            let new_mode = Some(smithay::output::Mode {
                                size: (cfg.width, cfg.height).into(),
                                refresh: cfg.refresh_mhz,
                            });
                            let new_scale = Some(smithay::output::Scale::Fractional(cfg.scale));
                            let new_loc = Some((cfg.x, cfg.y).into());
                            output.change_current_state(new_mode, None, new_scale, new_loc);
                            self.space.map_output(&output, (cfg.x, cfg.y));
                            smithay::desktop::layer_map_for_output(&output).arrange();
                            self.backend_data.reset_buffers(&output);
                        }
                    }
                    self.fixup_positions(self.pointer.current_location());
                    let outputs = self.collect_output_configs();
                    crate::enhancements::echo_ipc::sync_outputs_from_space(outputs.clone());
                    let _ = self.echo_service.read().emit_outputs_changed(&outputs);
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
                WindowAction::Maximize => self.maximize_window(&window),
                WindowAction::Unmaximize => self.unmaximize_window(&window),
                WindowAction::Close => self.close_window(&window),
                WindowAction::SetWorkArea(_)
                | WindowAction::OverlayKeyboard(_)
                | WindowAction::ApplyOutputConfigs => {}
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

        crate::shell::random_point_in_area(output_geometry)
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

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::{
        output::{Mode, PhysicalProperties, Subpixel},
        reexports::calloop::EventLoop,
        utils::Transform,
        wayland::shell::xdg::XdgToplevelSurfaceData,
    };
    use smithay_client_toolkit::reexports::protocols::xdg::shell::client::{
        xdg_surface, xdg_toplevel as client_xdg_toplevel, xdg_wm_base,
    };
    use std::os::unix::{io::AsFd, net::UnixStream};
    use wayland_client::{
        delegate_noop,
        protocol::{wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface},
        Connection, Dispatch, EventQueue, QueueHandle,
    };

    struct DummyBackend;
    impl Backend for DummyBackend {
        fn seat_name(&self) -> String {
            "seat0".to_string()
        }
        fn reset_buffers(&mut self, _output: &Output) {}
        fn early_import(&mut self, _surface: &WlSurface) {}
        fn update_led_state(&mut self, _led_state: LedState) {}
    }

    #[derive(Default)]
    struct TestClientData {
        compositor: Option<wl_compositor::WlCompositor>,
        shm: Option<wl_shm::WlShm>,
        wm_base: Option<xdg_wm_base::XdgWmBase>,
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for TestClientData {
        fn event(
            state: &mut Self,
            registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global { name, interface, version } = event {
                match interface.as_str() {
                    "wl_compositor" => {
                        state.compositor = Some(registry.bind(name, version.min(6), qh, ()));
                    }
                    "wl_shm" => {
                        state.shm = Some(registry.bind(name, version.min(1), qh, ()));
                    }
                    "xdg_wm_base" => {
                        state.wm_base = Some(registry.bind(name, version.min(6), qh, ()));
                    }
                    _ => {}
                }
            }
        }
    }

    impl Dispatch<xdg_wm_base::XdgWmBase, ()> for TestClientData {
        fn event(
            _: &mut Self,
            wm_base: &xdg_wm_base::XdgWmBase,
            event: xdg_wm_base::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let xdg_wm_base::Event::Ping { serial } = event {
                wm_base.pong(serial);
            }
        }
    }

    impl Dispatch<xdg_surface::XdgSurface, ()> for TestClientData {
        fn event(
            _: &mut Self,
            surface: &xdg_surface::XdgSurface,
            event: xdg_surface::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let xdg_surface::Event::Configure { serial } = event {
                surface.ack_configure(serial);
            }
        }
    }

    delegate_noop!(TestClientData: ignore wl_compositor::WlCompositor);
    delegate_noop!(TestClientData: ignore wl_surface::WlSurface);
    delegate_noop!(TestClientData: ignore wl_shm::WlShm);
    delegate_noop!(TestClientData: ignore wl_shm_pool::WlShmPool);
    delegate_noop!(TestClientData: ignore wl_buffer::WlBuffer);
    delegate_noop!(TestClientData: ignore client_xdg_toplevel::XdgToplevel);

    fn pump(
        event_loop: &mut EventLoop<'static, EvoState<DummyBackend>>,
        state: &mut EvoState<DummyBackend>,
        conn: &Connection,
        queue: &mut EventQueue<TestClientData>,
        cdata: &mut TestClientData,
    ) {
        for _ in 0..8 {
            let _ = conn.flush();
            let _ = event_loop.dispatch(Some(Duration::from_millis(2)), state);
            state.space.refresh();
            let _ = state.display_handle.flush_clients();
            if let Some(guard) = conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = queue.dispatch_pending(cdata);
        }
    }

    fn connect(
        event_loop: &mut EventLoop<'static, EvoState<DummyBackend>>,
        state: &mut EvoState<DummyBackend>,
        security_context: Option<SecurityContext>,
    ) -> (Connection, EventQueue<TestClientData>, TestClientData) {
        let (server_end, client_end) = UnixStream::pair().unwrap();
        let client_data: Arc<dyn ClientData> = Arc::new(ClientState {
            security_context,
            ..ClientState::default()
        });
        state
            .display_handle
            .insert_client(server_end, client_data)
            .unwrap();
        let conn = Connection::from_socket(client_end).unwrap();
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let _registry = conn.display().get_registry(&qh, ());
        let mut cdata = TestClientData::default();
        pump(event_loop, state, &conn, &mut queue, &mut cdata);
        (conn, queue, cdata)
    }

    fn spawn_mapped_window(
        event_loop: &mut EventLoop<'static, EvoState<DummyBackend>>,
        state: &mut EvoState<DummyBackend>,
        conn: &Connection,
        queue: &mut EventQueue<TestClientData>,
        cdata: &mut TestClientData,
        app_id: &str,
        width: i32,
        height: i32,
        setup_toplevel: impl FnOnce(&client_xdg_toplevel::XdgToplevel),
    ) -> (WindowElement, wl_surface::WlSurface, client_xdg_toplevel::XdgToplevel, tempfile::NamedTempFile, wl_buffer::WlBuffer) {
        let qh = queue.handle();
        let compositor = cdata.compositor.as_ref().unwrap();
        let shm = cdata.shm.as_ref().unwrap().clone();
        let wm_base = cdata.wm_base.as_ref().unwrap();

        let wl_surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&wl_surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.to_string());
        setup_toplevel(&toplevel);
        wl_surface.commit();
        pump(event_loop, state, conn, queue, cdata);

        let file = tempfile::NamedTempFile::new().unwrap();
        let stride = width * 4;
        file.as_file().set_len((stride * height) as u64).unwrap();
        let pool = shm.create_pool(file.as_file().as_fd(), stride * height, &qh, ());
        let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        wl_surface.attach(Some(&buffer), 0, 0);
        wl_surface.damage_buffer(0, 0, width, height);
        wl_surface.commit();
        pump(event_loop, state, conn, queue, cdata);

        let window = state
            .space
            .elements()
            .find(|w| w.claimed_app_id().as_deref() == Some(app_id))
            .cloned()
            .unwrap_or_else(|| panic!("expected mapped window for {app_id}"));
        (window, wl_surface, toplevel, file, buffer)
    }

    fn pending_toplevel_state(
        window: &WindowElement,
    ) -> (Option<smithay::utils::Size<i32, Logical>>, bool, usize) {
        let toplevel = window.0.toplevel().unwrap();
        let (size, fullscreen) = toplevel.with_pending_state(|s| {
            (s.size, s.states.contains(xdg_toplevel::State::Fullscreen))
        });
        let pending_count = smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .pending_configures()
                .len()
        });
        (size, fullscreen, pending_count)
    }

    #[test]
    fn fixup_positions_resizes_shell_fullscreen_maximized_and_floating_windows() {
        let mut event_loop: EventLoop<'static, EvoState<DummyBackend>> =
            EventLoop::try_new().unwrap();
        let display = Display::<EvoState<DummyBackend>>::new().unwrap();
        let mut state = EvoState::init(display, event_loop.handle(), DummyBackend, false);

        let mode_1080 = Mode {
            size: (1920, 1080).into(),
            refresh: 60_000,
        };
        let output = Output::new(
            "TEST-1".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Test".into(),
                model: "Test".into(),
                serial_number: "0".into(),
            },
        );
        output.change_current_state(Some(mode_1080), Some(Transform::Normal), None, Some((0, 0).into()));
        output.set_preferred(mode_1080);
        state.space.map_output(&output, (0, 0));

        let (t_conn, mut t_queue, mut t_data) = connect(&mut event_loop, &mut state, None);

        let (bloom, _bs, _bt, _bf, _bb) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &t_conn,
            &mut t_queue,
            &mut t_data,
            "bloom",
            1920,
            1080,
            |_| {},
        );
        let (overlay, _os, _ot, _of, _ob) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &t_conn,
            &mut t_queue,
            &mut t_data,
            "bloom-overlay",
            1920,
            1080,
            |_| {},
        );

        let creator_client_id = {
            let (s, _c) = UnixStream::pair().unwrap();
            std::mem::forget(_c);
            state
                .display_handle
                .insert_client(s, Arc::new(ClientState::default()))
                .unwrap()
                .id()
        };
        let (a_conn, mut a_queue, mut a_data) = connect(
            &mut event_loop,
            &mut state,
            Some(SecurityContext {
                sandbox_engine: Some("pyxis".into()),
                app_id: Some("app-suite".into()),
                instance_id: Some("1".into()),
                creator_client_id,
            }),
        );
        let (fs_win, fs_s, fs_t, _fsf, _fsb) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            "fullscreen-app",
            1920,
            1080,
            |_| {},
        );
        fs_t.set_fullscreen(None);
        fs_s.commit();
        let (min_fs_win, min_fs_s, min_fs_t, _mfsf, _mfsb) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            "minimized-fs-app",
            1920,
            1080,
            |_| {},
        );
        min_fs_t.set_fullscreen(None);
        min_fs_s.commit();
        pump(&mut event_loop, &mut state, &a_conn, &mut a_queue, &mut a_data);
        let (max_win, _ms, _mt, _mf, _mb) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            "maximized-app",
            1920,
            1080,
            |t| t.set_maximized(),
        );
        let (float_win, _fls, _flt, _flf, _flb) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            "floating-app",
            1000,
            800,
            |_| {},
        );
        state.place_window(&float_win, (1400, 600).into(), false);
        state.minimize_window(&min_fs_win);
        pump(&mut event_loop, &mut state, &t_conn, &mut t_queue, &mut t_data);
        pump(&mut event_loop, &mut state, &a_conn, &mut a_queue, &mut a_data);

        // Sanity check initial fullscreen / shell state at 1920x1080.
        assert_eq!(pending_toplevel_state(&bloom).0, Some((1920, 1080).into()));
        assert_eq!(pending_toplevel_state(&overlay).0, Some((1920, 1080).into()));
        assert_eq!(pending_toplevel_state(&fs_win).0, Some((1920, 1080).into()));

        // Simulate an output resolution change to 1280x720 and reposition to (0, 0).
        let mode_720 = Mode {
            size: (1280, 720).into(),
            refresh: 60_000,
        };
        output.change_current_state(Some(mode_720), None, None, Some((0, 0).into()));
        output.set_preferred(mode_720);

        state.fixup_positions((100.0, 100.0).into());

        // Bloom, bloom-overlay, and fullscreen app windows are reconfigured to 1280x720 + Fullscreen.
        for win in [&bloom, &overlay, &fs_win] {
            let (size, is_fs, pending_count) = pending_toplevel_state(win);
            assert_eq!(size, Some((1280, 720).into()));
            assert!(is_fs);
            assert!(pending_count >= 1, "expected pending configure to be sent");
            assert_eq!(state.space.element_location(win), Some((0, 0).into()));
            assert_eq!(
                state.workspace_manager.state(win).map(|s| s.layout),
                Some(WindowLayout::Fullscreen)
            );
        }

        // Minimized fullscreen window gets resized too, without being unminimized.
        assert!(!state.is_window_mapped(&min_fs_win));
        assert_eq!(pending_toplevel_state(&min_fs_win).0, Some((1280, 720).into()));
        assert!(pending_toplevel_state(&min_fs_win).1);

        // Maximized and floating windows are refitted to the new work area.
        let new_work_area =
            crate::shell::work_area::of_output(Rectangle::new((0, 0).into(), (1280, 720).into()));
        let expected_max_client: smithay::utils::Size<i32, Logical> = (
            new_work_area.size.w,
            (new_work_area.size.h - max_win.decoration_height()).max(1),
        )
            .into();
        assert_eq!(pending_toplevel_state(&max_win).0, Some(expected_max_client));
        assert_eq!(state.space.element_location(&max_win), Some(new_work_area.loc));

        let (float_pending_size, _, _) = pending_toplevel_state(&float_win);
        let float_client_size = float_pending_size.expect("floating window should be resized to fit");
        assert!(float_client_size.w <= new_work_area.size.w);
        assert!(float_client_size.h + float_win.decoration_height() <= new_work_area.size.h);
        let float_loc = state.space.element_location(&float_win).unwrap();
        assert!(float_loc.x >= new_work_area.loc.x);
        assert!(float_loc.y >= new_work_area.loc.y);
        assert!(float_loc.x + float_client_size.w <= new_work_area.loc.x + new_work_area.size.w);
        assert!(
            float_loc.y + float_client_size.h + float_win.decoration_height()
                <= new_work_area.loc.y + new_work_area.size.h
        );
    }

    #[cfg(feature = "echo-ipc")]
    #[test]
    fn sync_toplevels_lists_the_desktop_but_not_the_overlay() {
        let mut event_loop: EventLoop<'static, EvoState<DummyBackend>> =
            EventLoop::try_new().unwrap();
        let display = Display::<EvoState<DummyBackend>>::new().unwrap();
        let mut state = EvoState::init(display, event_loop.handle(), DummyBackend, false);

        let (t_conn, mut t_queue, mut t_data) = connect(&mut event_loop, &mut state, None);
        let _bloom = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &t_conn,
            &mut t_queue,
            &mut t_data,
            "bloom",
            800,
            600,
            |_| {},
        );
        let _overlay = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &t_conn,
            &mut t_queue,
            &mut t_data,
            "bloom-overlay",
            800,
            600,
            |_| {},
        );

        let creator_client_id = {
            let (s, _c) = UnixStream::pair().unwrap();
            std::mem::forget(_c);
            state
                .display_handle
                .insert_client(s, Arc::new(ClientState::default()))
                .unwrap()
                .id()
        };
        let (a_conn, mut a_queue, mut a_data) = connect(
            &mut event_loop,
            &mut state,
            Some(SecurityContext {
                sandbox_engine: Some("pyxis".into()),
                app_id: Some("com.pyxis.notes".into()),
                instance_id: Some("1".into()),
                creator_client_id,
            }),
        );
        let _app = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            "com.pyxis.notes",
            600,
            400,
            |_| {},
        );

        assert_eq!(state.windows_by_id.len(), 3);
        let infos = state.collect_toplevel_infos();
        let mut ids: Vec<&str> = infos.iter().map(|i| i.app_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["bloom", "com.pyxis.notes"]);
        assert!(infos.iter().all(|i| !i.fullscreen));

        state.last_toplevel_sync = Instant::now() - Duration::from_secs(1);
        state.sync_toplevels();
    }

    #[cfg(feature = "echo-ipc")]
    fn make_output(name: &str, w: i32, h: i32, x: i32, y: i32) -> Output {
        let mode = Mode {
            size: (w, h).into(),
            refresh: 60_000,
        };
        let output = Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Test".into(),
                model: "Test".into(),
                serial_number: name.to_string(),
            },
        );
        output.change_current_state(Some(mode), Some(Transform::Normal), None, Some((x, y).into()));
        output.set_preferred(mode);
        output
    }

    #[cfg(feature = "echo-ipc")]
    fn attach_shm_buffer(
        event_loop: &mut EventLoop<'static, EvoState<DummyBackend>>,
        state: &mut EvoState<DummyBackend>,
        conn: &Connection,
        queue: &mut EventQueue<TestClientData>,
        cdata: &mut TestClientData,
        wl_surface: &wl_surface::WlSurface,
        width: i32,
        height: i32,
    ) -> (tempfile::NamedTempFile, wl_buffer::WlBuffer) {
        let qh = queue.handle();
        let shm = cdata.shm.as_ref().unwrap().clone();
        let file = tempfile::NamedTempFile::new().unwrap();
        let stride = width * 4;
        file.as_file().set_len((stride * height) as u64).unwrap();
        let pool = shm.create_pool(file.as_file().as_fd(), stride * height, &qh, ());
        let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        wl_surface.attach(Some(&buffer), 0, 0);
        wl_surface.damage_buffer(0, 0, width, height);
        wl_surface.commit();
        pump(event_loop, state, conn, queue, cdata);
        (file, buffer)
    }

    #[cfg(feature = "echo-ipc")]
    #[test]
    fn wave_a_list_windows_flags_and_shell_focus_suppression() {
        let _lock = crate::shell::lock::test_lock_guard();
        let mut event_loop: EventLoop<'static, EvoState<DummyBackend>> =
            EventLoop::try_new().unwrap();
        let display = Display::<EvoState<DummyBackend>>::new().unwrap();
        let mut state = EvoState::init(display, event_loop.handle(), DummyBackend, false);
        let (echo_srv, mut spy) = GeckoEchoService::new_for_test();
        state.echo_service = Arc::new(RwLock::new(echo_srv));

        let output = make_output("TEST-LIST", 1280, 800, 0, 0);
        state.space.map_output(&output, (0, 0));

        let (t_conn, mut t_queue, mut t_data) = connect(&mut event_loop, &mut state, None);
        let (bloom, _bs, _bt, _bf, _bb) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &t_conn,
            &mut t_queue,
            &mut t_data,
            "bloom",
            1280,
            800,
            |_| {},
        );
        let (overlay, _os, _ot, _of, _ob) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &t_conn,
            &mut t_queue,
            &mut t_data,
            "bloom-overlay",
            1280,
            800,
            |_| {},
        );

        let creator_client_id = {
            let (s, _c) = UnixStream::pair().unwrap();
            std::mem::forget(_c);
            state
                .display_handle
                .insert_client(s, Arc::new(ClientState::default()))
                .unwrap()
                .id()
        };
        let (a_conn, mut a_queue, mut a_data) = connect(
            &mut event_loop,
            &mut state,
            Some(SecurityContext {
                sandbox_engine: Some("pyxis".into()),
                app_id: Some("com.wavea.notes".into()),
                instance_id: Some("1".into()),
                creator_client_id,
            }),
        );
        let (app_win, _as, _at, _af, _ab) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            "com.wavea.notes",
            600,
            400,
            |_| {},
        );

        // The desktop is listed too (suites read its focus); these checks are the app's.
        fn app_infos(state: &EvoState<DummyBackend>) -> Vec<WindowInfo> {
            state
                .collect_toplevel_infos()
                .into_iter()
                .filter(|i| i.app_id != "bloom")
                .collect()
        }
        let initial = app_infos(&state);
        assert_eq!(initial.len(), 1);
        assert!(!initial[0].minimized);
        assert!(!initial[0].maximized);
        assert!(!initial[0].fullscreen);
        assert!(initial[0].visible);

        // Maximize -> minimized keeps real flags -> unminimize -> fullscreen.
        state.maximize_window(&app_win);
        let max_infos = app_infos(&state);
        assert!(max_infos[0].maximized);
        assert!(!max_infos[0].minimized);
        assert!(!max_infos[0].fullscreen);

        state.minimize_window(&app_win);
        let min_infos = app_infos(&state);
        assert!(min_infos[0].minimized);
        assert!(min_infos[0].maximized);
        assert!(!min_infos[0].visible);

        state.unminimize_window(&app_win);
        let unmax_win = app_win.clone();
        state.unmaximize_window(&unmax_win);
        if let Some(toplevel) = app_win.0.toplevel().cloned() {
            use smithay::wayland::shell::xdg::XdgShellHandler;
            XdgShellHandler::fullscreen_request(&mut state, toplevel, None);
        }
        let fs_infos = app_infos(&state);
        assert!(fs_infos[0].fullscreen);
        assert!(!fs_infos[0].maximized);
        assert!(!fs_infos[0].minimized);

        // Focusing shell windows (`bloom`, `bloom-overlay`) must NEVER emit WindowFocusChanged.
        let _ = spy.drain_signals();
        let keyboard = state.seat.get_keyboard().unwrap();
        keyboard.set_focus(
            &mut state,
            Some(bloom.clone().into()),
            smithay::utils::SERIAL_COUNTER.next_serial(),
        );
        keyboard.set_focus(
            &mut state,
            Some(overlay.clone().into()),
            smithay::utils::SERIAL_COUNTER.next_serial(),
        );
        let shell_focus_signals: Vec<_> = spy
            .drain_signals()
            .into_iter()
            .filter(|(name, _)| name == "WindowFocusChanged")
            .collect();
        assert!(
            shell_focus_signals.is_empty(),
            "shell focus must not emit WindowFocusChanged, saw {shell_focus_signals:?}"
        );

        // Focusing a real app window DOES emit WindowFocusChanged.
        keyboard.set_focus(
            &mut state,
            Some(app_win.clone().into()),
            smithay::utils::SERIAL_COUNTER.next_serial(),
        );
        let app_focus_signals: Vec<_> = spy
            .drain_signals()
            .into_iter()
            .filter(|(name, _)| name == "WindowFocusChanged")
            .collect();
        assert_eq!(app_focus_signals.len(), 1);
        assert_eq!(app_focus_signals[0].1["window_id"], state.window_id_for(&app_win).unwrap());
    }

    #[cfg(feature = "echo-ipc")]
    #[test]
    fn wave_a_resize_cancel_unset_top_left_commit_and_self_unmaximize() {
        let _lock = crate::shell::lock::test_lock_guard();
        let mut event_loop: EventLoop<'static, EvoState<DummyBackend>> =
            EventLoop::try_new().unwrap();
        let display = Display::<EvoState<DummyBackend>>::new().unwrap();
        let mut state = EvoState::init(display, event_loop.handle(), DummyBackend, false);

        let output = make_output("TEST-RESIZE", 1280, 800, 0, 0);
        state.space.map_output(&output, (0, 0));

        let (a_conn, mut a_queue, mut a_data) = connect(&mut event_loop, &mut state, None);
        let (win, wl_s, wl_t, _f1, _b1) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            "com.wavea.resize",
            800,
            600,
            |_| {},
        );
        state.place_window(&win, (400, 150).into(), false);
        let root_surface = win.wl_surface().unwrap().into_owned();

        // (a) Simulate an interrupted resize grab (e.g. lock screen release_grabs) calling unset.
        smithay::wayland::compositor::with_states(&root_surface, |states| {
            let cell = states
                .data_map
                .get::<std::cell::RefCell<crate::shell::SurfaceData>>()
                .unwrap();
            cell.borrow_mut().resize_state = crate::shell::ResizeState::Resizing(crate::shell::ResizeData {
                edges: crate::shell::ResizeEdge::TOP_LEFT,
                initial_window_location: (400, 150).into(),
                initial_window_size: (800, 600).into(),
            });
        });
        if let Some(xdg) = win.0.toplevel() {
            xdg.with_pending_state(|s| s.states.set(xdg_toplevel::State::Resizing));
        }
        crate::shell::cancel_resize_on_unset(&mut state, &win);
        smithay::wayland::compositor::with_states(&root_surface, |states| {
            let cell = states
                .data_map
                .get::<std::cell::RefCell<crate::shell::SurfaceData>>()
                .unwrap();
            assert_eq!(cell.borrow().resize_state, crate::shell::ResizeState::NotResizing);
        });
        if let Some(xdg) = win.0.toplevel() {
            assert!(!xdg.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Resizing)));
        }

        // (b) WaitingForFinalAck / WaitingForCommit adjusts origin on commit for TOP_LEFT edge.
        let initial_geo_size = win.geometry().size;
        smithay::wayland::compositor::with_states(&root_surface, |states| {
            let cell = states
                .data_map
                .get::<std::cell::RefCell<crate::shell::SurfaceData>>()
                .unwrap();
            cell.borrow_mut().resize_state = crate::shell::ResizeState::WaitingForCommit(crate::shell::ResizeData {
                edges: crate::shell::ResizeEdge::TOP_LEFT,
                initial_window_location: (400, 150).into(),
                initial_window_size: initial_geo_size,
            });
        });
        let (_f2, _b2) = attach_shm_buffer(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            &wl_s,
            700,
            500,
        );
        // Client shrank by 100x100 from top-left, so origin shifts +100,+100 cleanly.
        assert_eq!(state.space.element_location(&win), Some((500, 250).into()));

        // (c) Self-maximized window (`OldGeometry` is None) computes centered 85% floating size.
        let _ = wl_t;
        let (max_win, _ms, _mt, _mf, _mb) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            "com.wavea.selfmax",
            1280,
            768,
            |t| t.set_maximized(),
        );
        assert!(max_win.user_data().get::<crate::shell::OldGeometry>().is_none());
        let area = crate::shell::work_area::of_output(Rectangle::new((0, 0).into(), (1280, 800).into()));
        let expected_frame = crate::shell::placement::default_restore_frame_rect(area);
        state.unmaximize_window(&max_win);
        let (pending_size, _, _) = pending_toplevel_state(&max_win);
        assert_eq!(
            pending_size,
            Some((
                expected_frame.size.w,
                (expected_frame.size.h - max_win.decoration_height()).max(1)
            ).into())
        );
        assert_eq!(state.space.element_location(&max_win), Some(expected_frame.loc));
    }

    #[cfg(feature = "echo-ipc")]
    #[test]
    fn wave_a_multi_output_xy_shell_outputs_edge_flush_and_minimized_refit() {
        let _lock = crate::shell::lock::test_lock_guard();
        let mut event_loop: EventLoop<'static, EvoState<DummyBackend>> =
            EventLoop::try_new().unwrap();
        let display = Display::<EvoState<DummyBackend>>::new().unwrap();
        let mut state = EvoState::init(display, event_loop.handle(), DummyBackend, false);
        let (echo_srv, mut spy) = GeckoEchoService::new_for_test();
        state.echo_service = Arc::new(RwLock::new(echo_srv));

        // Vertically stacked outputs: secondary output lives at (0, 1080).
        let out1 = make_output("OUT-1", 1920, 1080, 0, 0);
        let out2 = make_output("OUT-2", 1920, 1080, 0, 1080);
        state.space.map_output(&out1, (0, 0));
        state.space.map_output(&out2, (0, 1080));

        let (t_conn, mut t_queue, mut t_data) = connect(&mut event_loop, &mut state, None);
        let (bloom2, _bs, _bt, _bf, _bb) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &t_conn,
            &mut t_queue,
            &mut t_data,
            "bloom",
            1920,
            1080,
            |_| {},
        );
        // Pin secondary shell window onto OUT-2.
        state.place_window(&bloom2, (0, 1080).into(), false);
        if let Some(slot) = bloom2.user_data().get::<FullscreenOutputName>() {
            slot.set(Some("OUT-2".to_string()));
        }

        let (a_conn, mut a_queue, mut a_data) = connect(&mut event_loop, &mut state, None);
        // Edge-flush SSD window at (0, 0) (has -8px resize band bbox loc).
        let (edge_win, _es, _et, _ef, _eb) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            "com.wavea.edgeflush",
            800,
            600,
            |_| {},
        );
        state.place_window(&edge_win, (0, 0).into(), false);

        // Minimized floating window sized large (1400x900) on OUT-1 before shrink.
        let (min_win, _ms, _mt, _mf, _mb) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            "com.wavea.minimized",
            1400,
            900,
            |_| {},
        );
        state.place_window(&min_win, (100, 100).into(), false);
        state.minimize_window(&min_win);

        // Shrink OUT-1 while keeping OUT-2 stacked at (0, 1080), trigger lock + ApplyOutputConfigs.
        let _ = spy.drain_signals();
        let mode_small = Mode {
            size: (1024, 600).into(),
            refresh: 60_000,
        };
        out1.change_current_state(Some(mode_small), None, None, Some((0, 0).into()));
        out1.set_preferred(mode_small);
        crate::shell::lock::request_lock(crate::shell::lock::Source::Key);
        state.lock_tick();
        assert!(crate::shell::lock::input_locked());

        state
            .echo_service
            .read()
            .push_window_action_for_test(0, WindowAction::ApplyOutputConfigs);
        state.process_pending_window_actions();

        // Stacked output (0, 1080) preserved and secondary shell window remains on OUT-2!
        assert_eq!(state.space.output_geometry(&out2).map(|g| g.loc), Some((0, 1080).into()));
        assert_eq!(state.space.element_location(&bloom2), Some((0, 1080).into()));
        assert_eq!(pending_toplevel_state(&bloom2).0, Some((1920, 1080).into()));

        // Edge-flush window at (0, 0) stayed anchored without random teleporting!
        assert_eq!(state.space.element_location(&edge_win), Some((0, 0).into()));

        // Minimized floating window got shrunk while minimized, and fits after restore.
        let shrunk_area = crate::shell::work_area::of_output(Rectangle::new((0, 0).into(), (1024, 600).into()));
        let (min_pending, _, _) = pending_toplevel_state(&min_win);
        let min_pending = min_pending.expect("minimized oversized window reconfigured");
        assert!(min_pending.w <= shrunk_area.size.w);
        assert!(min_pending.h + min_win.decoration_height() <= shrunk_area.size.h);

        // OutputsChanged signal emitted cleanly even if transitioned while locked.
        let output_signals: Vec<_> = spy
            .drain_signals()
            .into_iter()
            .filter(|(name, _)| name == "OutputsChanged")
            .collect();
        assert!(!output_signals.is_empty());

        // Restore minimized window cleanly inside shrunk work area.
        state.unminimize_window(&min_win);
        let restored_loc = state.space.element_location(&min_win).unwrap();
        assert!(restored_loc.x >= shrunk_area.loc.x);
        assert!(restored_loc.y >= shrunk_area.loc.y);
    }

    #[cfg(feature = "echo-ipc")]
    #[test]
    fn wave_a_window_ux_tiling_lowering_app_cycling_and_cleanup() {
        let _lock = crate::shell::lock::test_lock_guard();
        crate::shell::recede::clear_receded_for_test();
        let mut event_loop: EventLoop<'static, EvoState<DummyBackend>> =
            EventLoop::try_new().unwrap();
        let display = Display::<EvoState<DummyBackend>>::new().unwrap();
        let mut state = EvoState::init(display, event_loop.handle(), DummyBackend, false);

        let output = make_output("TEST-UX", 1280, 800, 0, 0);
        state.space.map_output(&output, (0, 0));

        let (a_conn, mut a_queue, mut a_data) = connect(&mut event_loop, &mut state, None);
        let (doc1, _, doc1_t, _f1, _b1) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            "com.wavea.docs",
            640,
            400,
            |t| t.set_title("Doc One".to_string()),
        );
        // Spawn 2nd docs window and 1 terminal window with distinct app_ids/titles.
        let doc2_s = a_data.compositor.as_ref().unwrap().create_surface(&a_queue.handle(), ());
        let doc2_xdg = a_data.wm_base.as_ref().unwrap().get_xdg_surface(&doc2_s, &a_queue.handle(), ());
        let doc2_t = doc2_xdg.get_toplevel(&a_queue.handle(), ());
        doc2_t.set_app_id("com.wavea.docs".to_string());
        doc2_t.set_title("Doc Two".to_string());
        doc2_s.commit();
        pump(&mut event_loop, &mut state, &a_conn, &mut a_queue, &mut a_data);
        let (_df2, _db2) = attach_shm_buffer(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            &doc2_s,
            640,
            400,
        );
        let doc2 = state
            .space
            .elements()
            .find(|w| **w != doc1 && w.claimed_app_id().as_deref() == Some("com.wavea.docs"))
            .unwrap()
            .clone();

        let (term, _, _tt, _tf, _tb) = spawn_mapped_window(
            &mut event_loop,
            &mut state,
            &a_conn,
            &mut a_queue,
            &mut a_data,
            "com.wavea.term",
            600,
            400,
            |_| {},
        );
        let _ = doc1_t;

        state.place_window(&doc1, (100, 120).into(), false);
        state.focus_window(&term);
        state.focus_window(&doc2);
        state.focus_window(&doc1);

        // Super+Left tiles to left half of work area; pressing again restores.
        let area = crate::shell::work_area::of_output(Rectangle::new((0, 0).into(), (1280, 800).into()));
        state.tile_focused_window(crate::shell::TileSide::Left);
        assert_eq!(state.workspace_manager.state(&doc1).map(|s| s.layout), Some(WindowLayout::Tiled));
        assert_eq!(state.space.element_location(&doc1), Some(area.loc));
        assert_eq!(
            pending_toplevel_state(&doc1).0,
            Some((area.size.w / 2, (area.size.h - doc1.decoration_height()).max(1)).into())
        );

        state.tile_focused_window(crate::shell::TileSide::Left);
        assert_eq!(state.workspace_manager.state(&doc1).map(|s| s.layout), Some(WindowLayout::Floating));
        assert_eq!(state.space.element_location(&doc1), Some((100, 120).into()));

        // Super+Up toggles maximize; Super+Down unmaximizes then minimizes.
        state.toggle_maximize_focused_window();
        assert_eq!(state.workspace_manager.state(&doc1).map(|s| s.layout), Some(WindowLayout::Maximized));
        state.unmaximize_or_minimize_focused_window();
        assert_eq!(state.workspace_manager.state(&doc1).map(|s| s.layout), Some(WindowLayout::Floating));
        state.unmaximize_or_minimize_focused_window();
        assert!(state.is_window_minimized(&doc1));

        // Restore doc1, test Alt+Grave only cycles same `app_id` ("com.wavea.docs", skipping `term`).
        state.focus_window(&doc2);
        state.focus_window(&doc1);
        state.cycle_app_windows(false);
        assert_eq!(state.focused_app_window().as_ref(), Some(&doc2));

        // Lower doc2 puts it behind other app windows and moves focus to doc1.
        state.lower_window(&doc2);
        let first_app = state
            .space
            .elements()
            .find(|w| !w.is_shell_window())
            .cloned();
        assert_eq!(first_app.as_ref(), Some(&doc2));
        assert_ne!(state.focused_app_window().as_ref(), Some(&doc2));

        // Removing/unregistering a window scrubs it immediately from `focus_history` and `RECEDED`.
        crate::shell::recede::receded_for_test_push(doc2.clone());
        assert!(crate::shell::recede::receded_contains(&doc2));
        assert!(state.focus_history.contains(&doc2));
        state.remove_window_from_workspace(&doc2);
        assert!(!crate::shell::recede::receded_contains(&doc2));
        assert!(!state.focus_history.contains(&doc2));
    }
}
