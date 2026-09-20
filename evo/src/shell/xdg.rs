use std::cell::RefCell;

use smithay::{
    desktop::{
        find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output,
        space::SpaceElement, PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy,
        Space, Window, WindowSurfaceType,
    },
    input::{pointer::Focus, Seat},
    output::Output,
    reexports::{
        wayland_protocols::xdg::{decoration as xdg_decoration, shell::server::xdg_toplevel},
        wayland_server::{
            protocol::{wl_output, wl_seat, wl_surface::WlSurface},
            Resource,
        },
    },
    utils::{Logical, Point, Serial},
    wayland::{
        compositor::{self, with_states},
        seat::WaylandFocus,
        shell::xdg::{
            Configure, PopupSurface, PositionerState, ToplevelCachedState, ToplevelSurface,
            XdgShellHandler, XdgShellState,
        },
    },
};
use tracing::{debug, trace, warn};

use crate::{
    focus::KeyboardFocusTarget,
    shell::{TouchMoveSurfaceGrab, TouchResizeSurfaceGrab},
    state::{Backend, EvoState},
};

use super::{
    fullscreen_output_geometry, place_new_window, FullscreenSurface, OldGeometry, PointerMoveSurfaceGrab,
    PointerResizeSurfaceGrab, ResizeData, ResizeEdge, ResizeState, SurfaceData, WindowElement,
};
use super::ssd::HEADER_BAR_HEIGHT;

/// Wayland `xdg_toplevel` app_id of the desktop shell (Bloom), stamped by
/// `browser-init.js`'s PYXIS block via `gdk_wayland_window_set_application_id`
/// to exactly equal `PYXIS_APP_ID` (see that file's comment) -- so this is a
/// real, already-plumbed identity channel, not a guess. Used in
/// `fullscreen_request` to give the shell a NON-exclusive fullscreen (full
/// output size + chromeless, but composited under app windows) instead of the
/// exclusive single-surface scanout an ordinary fullscreen app gets, and in
/// `ack_configure` to *initiate* that fullscreen from the compositor side.
pub(crate) const SHELL_APP_ID: &str = "bloom";

/// Marker stored in a shell window's `user_data` once Evo has issued its
/// one compositor-initiated fullscreen for it (see `ack_configure`), so the
/// trigger fires exactly once rather than on every configure ack.
struct ShellFullscreenedMarker;

impl<BackendData: Backend> XdgShellHandler for EvoState<BackendData> {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
    let window = WindowElement(Window::new_wayland_window(surface.clone()));

    // Compute placement location
    let pointer_location = self.pointer.current_location();
    let location = self.compute_new_window_location(pointer_location, &window);

    // Add to workspace manager which will also register with Echo and map to space
    self.add_window_to_workspace(window.clone(), location);

    debug!("Created new XDG toplevel window at {:?}", location);

    // Set up commit handler for window resize/move updates
    compositor::add_post_commit_hook::<EvoState<BackendData>, _>(
        surface.wl_surface(),
        |state, _, surface| {
            handle_toplevel_commit(&mut state.space, surface);
            // Also (re)check whether this is the shell that needs its
            // compositor-driven fullscreen -- app_id is often set AFTER the
            // initial configures, so ack_configure alone can miss it; commits
            // keep coming, so this catches it whenever set_app_id lands.
            state.maybe_init_shell_fullscreen(surface);
        },
    );
}


    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);

        if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
            warn!("Failed to track popup: {}", err);
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            let geometry = positioner.get_geometry();
            state.geometry = geometry;
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat: Seat<EvoState<BackendData>> = Seat::from_resource(&seat).unwrap();
        self.move_request_xdg(&surface, &seat, serial)
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let seat: Seat<EvoState<BackendData>> = Seat::from_resource(&seat).unwrap();

        if let Some(touch) = seat.get_touch() {
            if touch.has_grab(serial) {
                let start_data = touch.grab_start_data().unwrap();

                let Some(window) = self.window_for_surface(surface.wl_surface().clone()) else {
                    return;
                };

                if start_data.focus.is_none()
                    || !start_data
                        .focus
                        .as_ref()
                        .unwrap()
                        .0
                        .same_client_as(&surface.wl_surface().id())
                {
                    return;
                }
                let geometry = window.geometry();
                let loc = self.space.element_location(&window).unwrap();
                let (initial_window_location, initial_window_size) = (loc, geometry.size);

                with_states(surface.wl_surface(), move |states| {
                    states
                        .data_map
                        .get::<RefCell<SurfaceData>>()
                        .unwrap()
                        .borrow_mut()
                        .resize_state = ResizeState::Resizing(ResizeData {
                        edges: edges.into(),
                        initial_window_location,
                        initial_window_size,
                    });
                });

                let grab = TouchResizeSurfaceGrab {
                    start_data,
                    window,
                    edges: edges.into(),
                    initial_window_location,
                    initial_window_size,
                    last_window_size: initial_window_size,
                };

                touch.set_grab(self, grab, serial);
                return;
            }
        }

        let pointer = seat.get_pointer().unwrap();

        if !pointer.has_grab(serial) {
            return;
        }

        let start_data = pointer.grab_start_data().unwrap();

        let window = self
            .window_for_surface(surface.wl_surface().clone())
            .unwrap();

        if start_data.focus.is_none()
            || !start_data
                .focus
                .as_ref()
                .unwrap()
                .0
                .same_client_as(&surface.wl_surface().id())
        {
            return;
        }

        let geometry = window.geometry();
        let loc = self.space.element_location(&window).unwrap();
        let (initial_window_location, initial_window_size) = (loc, geometry.size);

        with_states(surface.wl_surface(), move |states| {
            states
                .data_map
                .get::<RefCell<SurfaceData>>()
                .unwrap()
                .borrow_mut()
                .resize_state = ResizeState::Resizing(ResizeData {
                edges: edges.into(),
                initial_window_location,
                initial_window_size,
            });
        });

        let grab = PointerResizeSurfaceGrab {
            start_data,
            window,
            edges: edges.into(),
            initial_window_location,
            initial_window_size,
            last_window_size: initial_window_size,
        };

        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn ack_configure(&mut self, surface: WlSurface, configure: Configure) {
        if let Configure::Toplevel(configure) = configure {
            if let Some(serial) = with_states(&surface, |states| {
                if let Some(data) = states.data_map.get::<RefCell<SurfaceData>>() {
                    if let ResizeState::WaitingForFinalAck(_, serial) = data.borrow().resize_state
                    {
                        return Some(serial);
                    }
                }

                None
            }) {
                let is_resizing = with_states(&surface, |states| {
                    states
                        .cached_state
                        .get::<ToplevelCachedState>()
                        .current()
                        .last_acked
                        .as_ref()
                        .is_some_and(|c| c.state.states.contains(xdg_toplevel::State::Resizing))
                });

                if configure.serial >= serial && is_resizing {
                    with_states(&surface, |states| {
                        let mut data = states
                            .data_map
                            .get::<RefCell<SurfaceData>>()
                            .unwrap()
                            .borrow_mut();
                        if let ResizeState::WaitingForFinalAck(resize_data, _) = data.resize_state
                        {
                            data.resize_state = ResizeState::WaitingForCommit(resize_data);
                        } else {
                            unreachable!()
                        }
                    });
                }
            }

            let window = self
                .space
                .elements()
                .find(|element| element.wl_surface().as_deref() == Some(&surface));
            if let Some(window) = window.cloned() {
                use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
                use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::Mode as KdeMode;

                // A client is server-decorated if it negotiated ServerSide on
                // *either* decoration protocol. In practice the two are
                // mutually exclusive by toolkit: xdg-decoration is what
                // non-GTK clients use, and the KDE one is the only one GTK3
                // (and therefore vela) ever binds -- GDK has no
                // zxdg_decoration_manager_v1 support at all, which is why the
                // xdg branch alone read `decoration_mode=None` forever and no
                // vela window was ever server-decorated.
                //
                // Neither path is a compositor-side default: both require the
                // client to actually ask. vela only asks for Server when
                // MOZ_GTK_TITLEBAR_DECORATION is set (the rootfs sets `none`);
                // without it Firefox forces its own CSD, requests Client, and
                // keeps drawing its own titlebar -- which is correct, and is
                // what stops the double-decoration this used to produce.
                let xdg_ssd = configure.state.decoration_mode == Some(Mode::ServerSide);
                let kde_mode = crate::state::kde_decoration_mode(&surface);

                let app_id = with_states(&surface, |states| {
                    use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
                    states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .and_then(|d| d.lock().unwrap().app_id.clone())
                });

                // The shell is never decorated, by anyone. Bloom is the
                // desktop itself -- a titlebar on it is always wrong.
                //
                // This has to be an explicit exemption rather than something
                // the negotiation falls out of: bloom is launched with the
                // same `MOZ_GTK_TITLEBAR_DECORATION=none` as every app
                // (create-rootfs.sh), so it asks for Server just like they
                // do. Only `--kiosk` makes it ask for Client instead
                // (gKioskMode -> gtk_window_set_decorated(false) at widget
                // creation), and create-rootfs.sh only passes --kiosk when
                // BOOT_DISPLAY_MODE=fullscreen -- so without this guard a
                // windowed-mode boot would put an Evo header bar across the
                // top of the desktop.
                let is_shell = app_id.as_deref() == Some(SHELL_APP_ID);
                let is_ssd = !is_shell && (xdg_ssd || kde_mode == Some(KdeMode::Server));

                tracing::debug!(
                    "xdg ack_configure: app_id={:?} xdg_decoration={:?} kde_decoration={:?} -> is_ssd={}",
                    app_id,
                    configure.state.decoration_mode,
                    kde_mode,
                    is_ssd
                );
                window.set_ssd(is_ssd);

                // Bind the surface to its AppHost session HERE, not at map
                // time, because this is the first point at which the app id is
                // actually known.
                //
                // The map path (state.rs) reads `XdgToplevelSurfaceData.app_id`
                // while the toplevel is still being set up, and GTK/vela don't
                // set it until later (it arrives via `windowclass` ->
                // SyncAttributesToWidget -> nsWindow::SetWindowClass ->
                // gdk_wayland_window_set_application_id). Binding there
                // therefore saw `None`, fell back to the literal "unknown",
                // and `AppHost::bind_surface` *synthesized* a session for it --
                // measured on-device as one bogus `appId: "unknown"` session
                // holding surfaces [1,2,3] while the real `files` session had
                // `surfaces: []`. That empty list is why `nova.Focus` (which
                // reads `surfaces.first()`) always answered "no mapped
                // compositor window yet" and restore-from-dock never worked.
                //
                // `bind_surface` is idempotent (it skips a surface already in
                // the session's list), so re-running on every configure ack is
                // cheap and self-healing: whichever ack first carries a real
                // app id is the one that binds.
                #[cfg(feature = "echo-ipc")]
                {
                    if let Some(app_id) = app_id.as_deref() {
                        // Only bind toplevels that are actually apps.
                        //
                        // `AppHost::bind_surface` SYNTHESIZES a session for any
                        // app id it doesn't already know, and the shell renders
                        // every session as a dock entry — so anything bound
                        // here that isn't a real app becomes a phantom running
                        // app the user never launched. Excluded:
                        //   - `bloom`, the shell itself: it's the desktop, not
                        //     an app, and has no appHost session to bind to;
                        //   - `unknown`, the fallback for a toplevel with no
                        //     xdg app_id at all;
                        //   - `vela-default`, vela's own default app id, which
                        //     is what any window that never ran browser-init's
                        //     PYXIS block reports. Nova's profile prefs now
                        //     disable the WebExtensions dummy window that was
                        //     producing these, but this stays as a backstop:
                        //     any future stray vela toplevel must not be able
                        //     to invent a dock entry.
                        let bindable = !app_id.is_empty()
                            && app_id != SHELL_APP_ID
                            && app_id != "unknown"
                            && app_id != "vela-default";
                        if bindable {
                            if let Some(window_id) = self.window_id_for(&window) {
                                if let Err(e) = self
                                    .echo_service
                                    .read()
                                    .bind_surface_remote(app_id, window_id)
                                {
                                    tracing::warn!(
                                        "bind_surface({}, {}) failed: {}",
                                        app_id,
                                        window_id,
                                        e
                                    );
                                }
                            }
                        }
                    }
                }
            }
            self.maybe_init_shell_fullscreen(&surface);
        }
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        mut wl_output: Option<wl_output::WlOutput>,
    ) {
        let wl_surface = surface.wl_surface();

        let output_geometry =
            fullscreen_output_geometry(wl_surface, wl_output.as_ref(), &mut self.space);

        if let Some(geometry) = output_geometry {
            let output = wl_output
                .as_ref()
                .and_then(Output::from_resource)
                .unwrap_or_else(|| self.space.outputs().next().unwrap().clone());
            let client = match self.display_handle.get_client(wl_surface.id()) {
                Ok(client) => client,
                Err(_) => return,
            };
            for output in output.client_outputs(&client) {
                wl_output = Some(output);
            }
            // Clone out of the space so the immutable `elements()` borrow is
            // released before the `map_element` below needs `&mut self.space`.
            let window = self
                .space
                .elements()
                .find(|window| {
                    window
                        .wl_surface()
                        .map(|s| &*s == wl_surface)
                        .unwrap_or(false)
                })
                .cloned()
                .unwrap();

            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Fullscreen);
                state.size = Some(geometry.size);
                state.fullscreen_output = wl_output;
            });

            // The desktop shell (Bloom) is launched PYXIS_DISPLAY_MODE=
            // fullscreen purely so GTK drops its titlebar (a Wayland toplevel
            // is only chromeless when fullscreen/kiosk/undecorated -- see
            // vela-gtk-titlebar-decoration). But unlike a real fullscreen app
            // the shell must sit BEHIND ordinary app windows, not take over
            // the output. Evo's exclusive-fullscreen render path (render.rs's
            // FullscreenSurface) draws ONLY that one surface, which is exactly
            // what made every app launched from the shell invisible. So for
            // the shell we still send the fullscreen state+size above (that's
            // what makes Firefox undecorate), but instead of registering it as
            // the output's exclusive FullscreenSurface we map it at the output
            // origin as an ordinary bottom-of-stack space window
            // (activate=false, so app windows raised later stay on top).
            // Real fullscreen apps (anything that isn't the shell) keep the
            // exclusive scanout path. See evo-firefox-decoration-arch.
            let app_id = with_states(wl_surface, |states| {
                use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().unwrap().app_id.clone())
            });
            if app_id.as_deref() == Some(SHELL_APP_ID) {
                self.space.map_element(window.clone(), geometry.loc, false);
                trace!("Shell fullscreen (non-exclusive): {:?}", window);
            } else {
                output
                    .user_data()
                    .insert_if_missing(FullscreenSurface::default);
                output
                    .user_data()
                    .get::<FullscreenSurface>()
                    .unwrap()
                    .set(window.clone());
                trace!("Fullscreening: {:?}", window);
            }
        }

        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let ret = surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Fullscreen);
            state.size = None;
            state.fullscreen_output.take()
        });
        if let Some(output) = ret {
            let output = Output::from_resource(&output).unwrap();
            if let Some(fullscreen) = output.user_data().get::<FullscreenSurface>() {
                trace!("Unfullscreening: {:?}", fullscreen.get());
                fullscreen.clear();
                self.backend_data.reset_buffers(&output);
            }
        }

        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        // Defensive: a compositor must never panic on a maximize (it takes
        // down every client with it -- "dumps you to a shell"). Every lookup
        // here is a graceful early-return instead of unwrap()/expect().
        //
        // Two different buttons land here. A *client*-decorated window (the
        // boot shell, which runs --kiosk and so negotiates Client) drives it
        // via GTK's own titlebar. A *server*-decorated window -- every
        // Nova-launched app, which negotiates Server over
        // org_kde_kwin_server_decoration (see ack_configure) -- drives it via
        // Evo's own `HeaderBar::handle_maximize`, deferred through
        // `insert_idle` to avoid a decoration_state double-borrow. Both must
        // be bulletproof; the SSD path is the one alpha2 #7 reports crashing.
        let Some(window) = self.window_for_surface(surface.wl_surface().clone()) else {
            return;
        };
        let outputs_for_window = self.space.outputs_for_element(&window);
        let Some(output) = outputs_for_window
            .first()
            .cloned()
            .or_else(|| self.space.outputs().next().cloned())
        else {
            return;
        };
        let Some(geometry) = self.space.output_geometry(&output) else {
            return;
        };

        // Remember pre-maximize geometry so unmaximize_request can put the
        // window back where it was.
        if let Some(old_geo) = self.space.element_bbox(&window) {
            window.user_data().insert_if_missing(OldGeometry::default);
            if let Some(saved) = window.user_data().get::<OldGeometry>() {
                saved.save(old_geo);
            }
        }

        // A server-decorated window's client area sits BELOW Evo's
        // HEADER_BAR_HEIGHT-tall title bar (element.rs's SpaceElement
        // geometry/bbox add it to the element height; the render path pushes
        // the client down by it). So the client must be configured that much
        // SHORTER than the output, or header + client together overflow the
        // screen (the client's bottom rows fall off). Read is_ssd into a local
        // so the RefMut is dropped before map_element re-borrows
        // decoration_state.
        let is_ssd = window.decoration_state().is_ssd;
        let mut client_size = geometry.size;
        if is_ssd {
            client_size.h = (client_size.h - HEADER_BAR_HEIGHT).max(1);
        }
        tracing::info!(
            "maximize_request: output_size={:?} is_ssd={} client_size={:?}",
            geometry.size,
            is_ssd,
            client_size
        );

        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Maximized);
            state.size = Some(client_size);
        });
        self.space.map_element(window, geometry.loc, true);

        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        // Defensive early-return (see maximize_request): never panic here.
        let Some(window) = self.window_for_surface(surface.wl_surface().clone()) else {
            return;
        };
        let restored = window
            .user_data()
            .get::<OldGeometry>()
            .and_then(|data| data.restore());

        // Send size = None, NOT the saved geometry's size. The saved rect is
        // the compositor-space BBOX -- it includes the CSD shadow margin and
        // Evo's SSD header, which is not the xdg_toplevel window-geometry size
        // the client expects. Handing that back configured the client too
        // large and mismatched what it could actually produce; size=None is
        // the standard "you choose" unmaximize configure (what smithay's anvil
        // does), and a client that remembers its own last floating size
        // (Firefox/GTK does) restores exactly that. The saved LOCATION is
        // still used to place it back.
        tracing::info!(
            "unmaximize_request: restored_loc={:?} had_saved_geo={}",
            restored.map(|g| g.loc),
            restored.is_some()
        );

        surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Maximized);
            state.size = None;
        });

        // `restored == None` is not a corner case here, it is the COMMON one,
        // and leaving it unhandled is a real defect (observed on-device:
        // "unmaximize_request: restored_loc=None had_saved_geo=false").
        //
        // `OldGeometry` is only ever populated by our own `maximize_request`.
        // A client that comes up maximized under its own steam -- which
        // GTK/Gecko does, via the `sizemode="maximized"` path in
        // browser-init.js's onBeforeInitialXULLayout, without ever sending
        // xdg_toplevel.set_maximized to us -- was never maximized *by Evo*, so
        // nothing was saved. The window then restores down with no saved rect.
        //
        // Previously that meant skipping `map_element` entirely: the client
        // was told "unmaximized, pick your own size" while the compositor left
        // the element mapped at its maximized origin, so the two disagreed
        // about the window's geometry from that moment on. Place it
        // deliberately instead -- centred on its output at a conventional
        // restore size -- so both sides agree on where the window is.
        let restore_loc = restored.map(|old_geo| old_geo.loc).or_else(|| {
            let output = self
                .space
                .outputs_for_element(&window)
                .first()
                .cloned()
                .or_else(|| self.space.outputs().next().cloned())?;
            let output_geo = self.space.output_geometry(&output)?;
            let size = self
                .space
                .element_bbox(&window)
                .map(|bbox| bbox.size)
                .unwrap_or(output_geo.size);
            // Centre, clamped so the titlebar can never land off-screen.
            Some(
                (
                    output_geo.loc.x + ((output_geo.size.w - size.w) / 2).max(0),
                    output_geo.loc.y + ((output_geo.size.h - size.h) / 2).max(0),
                )
                    .into(),
            )
        });

        tracing::info!(
            "unmaximize_request: mapping to {:?} (synthesized={})",
            restore_loc,
            restored.is_none()
        );

        if let Some(loc) = restore_loc {
            self.space.map_element(window, loc, true);
        }

        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        // Defensive early-return (see maximize_request): never panic here.
        // This is the path a *client-decorated* window's own titlebar
        // minimize button drives (GTK sends xdg_toplevel.set_minimized); the
        // SSD header bar's minimize button goes through
        // `HeaderBar::handle_minimize` instead. Both still end up in
        // `minimize_window`. Without this override smithay's default no-op
        // `minimize_request` swallows the CSD click and the window never
        // hides.
        let Some(window) = self.window_for_surface(surface.wl_surface().clone()) else {
            return;
        };
        self.minimize_window(&window);
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat: Seat<EvoState<BackendData>> = Seat::from_resource(&seat).unwrap();
        let kind = PopupKind::Xdg(surface);
        if let Some(root) = find_popup_root_surface(&kind).ok().and_then(|root| {
            self.space
                .elements()
                .find(|w| w.wl_surface().map(|s| *s == root).unwrap_or(false))
                .cloned()
                .map(KeyboardFocusTarget::from)
                .or_else(|| {
                    self.space.outputs().find_map(|o| {
                        let map = layer_map_for_output(o);
                        map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                            .cloned()
                            .map(KeyboardFocusTarget::LayerSurface)
                    })
                })
        }) {
            let ret = self.popups.grab_popup(root, kind, &seat, serial);

            if let Ok(mut grab) = ret {
                if let Some(keyboard) = seat.get_keyboard() {
                    if keyboard.is_grabbed()
                        && !(keyboard.has_grab(serial)
                            || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
                    {
                        grab.ungrab(PopupUngrabStrategy::All);
                        return;
                    }
                    keyboard.set_focus(self, grab.current_grab(), serial);
                    keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
                }
                if let Some(pointer) = seat.get_pointer() {
                    if pointer.is_grabbed()
                        && !(pointer.has_grab(serial)
                            || pointer
                                .has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
                    {
                        grab.ungrab(PopupUngrabStrategy::All);
                        return;
                    }
                    pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
                }
            }
        }
    }
}

impl<BackendData: Backend> EvoState<BackendData> {
    /// If `surface` belongs to the desktop shell (Bloom) and Evo hasn't yet
    /// issued its one compositor-driven fullscreen for it, do so now
    /// (deferred to an idle callback). The shell is opened popup-like, so it
    /// won't fullscreen itself; Evo drives it so Firefox drops its GTK
    /// titlebar and fills the output. Called from both `ack_configure` and
    /// the per-commit hook because the client's `set_app_id` often lands
    /// AFTER the initial configures -- checking on every commit guarantees we
    /// see it. The `ShellFullscreenedMarker` in the window's user_data makes
    /// this a one-shot. See the SHELL_APP_ID branch of `fullscreen_request`.
    fn maybe_init_shell_fullscreen(&mut self, surface: &WlSurface) {
        let Some(window) = self
            .space
            .elements()
            .find(|e| e.wl_surface().as_deref() == Some(surface))
            .cloned()
        else {
            return;
        };
        if window.user_data().get::<ShellFullscreenedMarker>().is_some() {
            return;
        }
        let app_id = with_states(surface, |states| {
            use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().unwrap().app_id.clone())
        });
        if app_id.as_deref() != Some(SHELL_APP_ID) {
            return;
        }
        window
            .user_data()
            .insert_if_missing(|| ShellFullscreenedMarker);
        if let Some(toplevel) = window.0.toplevel().cloned() {
            tracing::info!("Initiating compositor-driven fullscreen for shell");
            self.handle.insert_idle(move |data| {
                data.fullscreen_request(toplevel, None);
            });
        }
    }

    pub fn move_request_xdg(
        &mut self,
        surface: &ToplevelSurface,
        seat: &Seat<Self>,
        serial: Serial,
    ) {
        if let Some(touch) = seat.get_touch() {
            if touch.has_grab(serial) {
                let start_data = touch.grab_start_data().unwrap();

                let Some(window) = self.window_for_surface(surface.wl_surface().clone()) else {
                    return;
                };

                if start_data.focus.is_none()
                    || !start_data
                        .focus
                        .as_ref()
                        .unwrap()
                        .0
                        .same_client_as(&surface.wl_surface().id())
                {
                    return;
                }

                let mut initial_window_location = self.space.element_location(&window).unwrap();

                let changed = surface.with_pending_state(|state| {
                    if state.states.unset(xdg_toplevel::State::Maximized) {
                        state.size = None;
                        true
                    } else {
                        false
                    }
                });
                if changed {
                    surface.send_configure();
                    initial_window_location = start_data.location.to_i32_round();
                }

                let grab = TouchMoveSurfaceGrab {
                    start_data,
                    window,
                    initial_window_location,
                };

                touch.set_grab(self, grab, serial);
                return;
            }
        }

        let pointer = seat.get_pointer().unwrap();

        if !pointer.has_grab(serial) {
            return;
        }

        let start_data = pointer.grab_start_data().unwrap();

        let Some(window) = self.window_for_surface(surface.wl_surface().clone()) else {
            return;
        };

        if start_data.focus.is_none()
            || !start_data
                .focus
                .as_ref()
                .unwrap()
                .0
                .same_client_as(&surface.wl_surface().id())
        {
            return;
        }

        let mut initial_window_location = self.space.element_location(&window).unwrap();

        let changed = surface.with_pending_state(|state| {
            if state.states.unset(xdg_toplevel::State::Maximized) {
                state.size = None;
                true
            } else {
                false
            }
        });
        if changed {
            surface.send_configure();
            let pos = pointer.current_location();
            initial_window_location = (pos.x as i32, pos.y as i32).into();
        }

        let grab = PointerMoveSurfaceGrab {
            start_data,
            window,
            initial_window_location,
        };

        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self.window_for_surface(root.clone()) else {
            return;
        };

        let mut outputs_for_window = self.space.outputs_for_element(&window);
        if outputs_for_window.is_empty() {
            return;
        }

        let mut outputs_geo = self
            .space
            .output_geometry(&outputs_for_window.pop().unwrap())
            .unwrap();
        for output in outputs_for_window {
            outputs_geo = outputs_geo.merge(self.space.output_geometry(&output).unwrap());
        }

        let window_geo = self.space.element_geometry(&window).unwrap();

        let mut target = outputs_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

fn handle_toplevel_commit(space: &mut Space<WindowElement>, surface: &WlSurface) -> Option<()> {
    let window = space
        .elements()
        .find(|w| w.wl_surface().as_deref() == Some(surface))
        .cloned()?;

    let mut window_loc = space.element_location(&window)?;
    let geometry = window.geometry();

    let new_loc: Point<Option<i32>, Logical> =
        with_states(window.wl_surface().as_deref()?, |states| {
            let data = states
                .data_map
                .get::<RefCell<SurfaceData>>()?
                .borrow_mut();

            if let ResizeState::Resizing(resize_data) = data.resize_state {
                let edges = resize_data.edges;
                let loc = resize_data.initial_window_location;
                let size = resize_data.initial_window_size;

                edges.intersects(ResizeEdge::TOP_LEFT).then(|| {
                    let new_x = edges
                        .intersects(ResizeEdge::LEFT)
                        .then_some(loc.x + (size.w - geometry.size.w));

                    let new_y = edges
                        .intersects(ResizeEdge::TOP)
                        .then_some(loc.y + (size.h - geometry.size.h));

                    (new_x, new_y).into()
                })
            } else {
                None
            }
        })?;

    if let Some(new_x) = new_loc.x {
        window_loc.x = new_x;
    }
    if let Some(new_y) = new_loc.y {
        window_loc.y = new_y;
    }

    if new_loc.x.is_some() || new_loc.y.is_some() {
        space.map_element(window, window_loc, false);
    }

    Some(())
}
