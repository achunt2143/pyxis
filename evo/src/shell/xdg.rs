use std::cell::RefCell;

use smithay::{
    desktop::{
        find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output,
        space::SpaceElement, PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy,
        Space, Window, WindowSurfaceType,
    },
    input::{pointer::Focus, Seat},
    reexports::{
        wayland_protocols::xdg::{decoration as xdg_decoration, shell::server::xdg_toplevel},
        wayland_server::{
            protocol::{wl_output, wl_seat, wl_surface::WlSurface},
            Resource,
        },
    },
    utils::{Logical, Point, Rectangle, Serial, Size},
    wayland::{
        compositor::{self, with_states, BufferAssignment, SurfaceAttributes},
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
    placement::{PreFullscreen, PreFullscreenState},
    OldGeometry, PointerMoveSurfaceGrab, PointerResizeSurfaceGrab, ResizeData, ResizeEdge,
    ResizeState, SurfaceData, WindowElement, WindowLayout,
};

/// Wayland `xdg_toplevel` app_id of the desktop shell (Bloom), stamped by
/// `browser-init.js`'s PYXIS block via `gdk_wayland_window_set_application_id`
/// to exactly equal `PYXIS_APP_ID` (see that file's comment) -- so this is a
/// real, already-plumbed identity channel, not a guess. Used in
/// `fullscreen_request` to give the shell a NON-exclusive fullscreen (full
/// output size + chromeless, but composited under app windows) instead of the
/// exclusive single-surface scanout an ordinary fullscreen app gets, and in
/// `ack_configure` to *initiate* that fullscreen from the compositor side.
pub(crate) const SHELL_APP_ID: &str = "bloom";

/// app_id of Bloom's overlay window, the transparent second shell window
/// that holds what must be seen over apps (see `super::overlay`). vela stamps
/// it `<shell app id>-overlay`.
pub(crate) const OVERLAY_APP_ID: &str = "bloom-overlay";

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
        // Who is on the other end is fixed by how it connected, and is known
        // before it has said anything. Everything about the window's role
        // and identity hangs on this, so it is recorded before the window is
        // registered anywhere.
        let (trusted, context_app_id) = self
            .display_handle
            .get_client(surface.wl_surface().id())
            .ok()
            .map(|client| {
                (
                    crate::trust::client_is_trusted(&client),
                    client
                        .get_data::<crate::state::ClientState>()
                        .and_then(|state| state.security_context.as_ref())
                        .and_then(|context| context.app_id.clone()),
                )
            })
            .unwrap_or((false, None));
        window.set_origin(trusted, context_app_id);

        // Compute placement location
        let pointer_location = self.pointer.current_location();
        let location = self.compute_new_window_location(pointer_location, &window);

        // Add to workspace manager which will also register with Echo and map to space
        self.add_window_to_workspace(window.clone(), location);
        // That location was picked before the window had a size; the first
        // commit that gives it one places it for real.
        window.set_placement_pending(true);
        // Nor is it known yet whether this is an app or the shell: the first
        // commit that maps it decides whether it takes the keyboard.
        window.set_focus_pending(true);

        debug!("Created new XDG toplevel window at {:?}", location);

        // A client can unmap its window by committing a null buffer without
        // destroying it. The snapshot a close animation draws has to be taken
        // before that commit drops the buffer, so it is taken here, in the
        // pre-commit hook, which runs before the pending state is applied.
        compositor::add_pre_commit_hook::<EvoState<BackendData>, _>(
            surface.wl_surface(),
            |state, _, surface| {
                let unmapping = with_states(surface, |states| {
                    matches!(
                        states.cached_state.get::<SurfaceAttributes>().pending().buffer,
                        Some(BufferAssignment::Removed)
                    )
                });
                if unmapping {
                    if let Some(window) = state.tracked_window_for_surface(surface) {
                        state.begin_close_animation(&window);
                    }
                }
            },
        );

        // Set up commit handler for window resize/move updates
        compositor::add_post_commit_hook::<EvoState<BackendData>, _>(
            surface.wl_surface(),
            |state, _, surface| {
                state.handle_toplevel_commit(surface);
                // Also (re)check whether this is the shell that needs its
                // compositor-driven fullscreen -- app_id is often set AFTER the
                // initial configures, so ack_configure alone can miss it; commits
                // keep coming, so this catches it whenever set_app_id lands.
                state.maybe_init_shell_fullscreen(surface);
                // Likewise the appHost binding: it follows the app id, which
                // can arrive or change on any commit.
                #[cfg(feature = "echo-ipc")]
                if let Some(window) = state.tracked_window_for_surface(surface) {
                    state.sync_surface_binding(&window);
                }
            },
        );
    }

    /// A client destroyed its toplevel. Take the window out of the
    /// workspace, the Space, and the Echo registries.
    ///
    /// **This was missing entirely**, and `XdgShellHandler` provides a no-op
    /// default, so it compiled and silently did nothing — Evo never learned
    /// that a window had gone away. Observed 2026-09-21: the workspace
    /// window count only ever rose (1,2,3,4,5,6 across three launches of one
    /// app) and `remove_window_from_workspace`'s own debug line never
    /// appeared once in a whole session.
    ///
    /// The only call to that function was in the commit handler, guarded by
    /// `if !root.alive()` — which cannot fire for a normally-closed window,
    /// because destroying a toplevel is precisely when commits STOP. It
    /// could only ever catch a surface that died between a commit being
    /// queued and handled.
    ///
    /// What the user saw: close an app, launch it again, and get a dead
    /// browser-chrome window sitting over the real one. Those are stale
    /// elements Evo kept compositing — including the short-lived unstamped
    /// window Gecko opens and closes during startup, which reaches Evo as
    /// `vela-default` (no `pyxisApp` stamp) and then never leaves. It read
    /// as "kiosk mode was ignored on relaunch"; the window was real, just
    /// nobody's.
    ///
    /// The lookup used to be the Space's alone, and a minimized window is
    /// out of the Space: closing one left a ghost in `ListWindows`, no
    /// `WindowClosed`, and a surface appHost never unbound. It now looks
    /// wherever a minimized window still is.
    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let Some(window) = self.tracked_window_for_toplevel(&surface) else {
            // Already gone, or never tracked.
            debug!("toplevel_destroyed: no tracked window for this surface");
            return;
        };
        if window.is_overlay() {
            // The shell's layer is gone: what it reserved and grabbed goes
            // with it, or apps stay squeezed under a bar nobody draws.
            super::overlay::reset();
            super::minimize::reset_targets();
            self.note_overlay_keyboard(false);
            self.set_work_area(super::work_area::Insets::default());
        }
        self.remove_window_from_workspace(&window);
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
        self.resize_request_xdg(&surface, &seat, serial, edges.into())
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

            if let Some(window) = self.tracked_window_for_surface(&surface) {
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
                let is_shell = window.is_shell_window();
                let is_ssd = !is_shell && (xdg_ssd || kde_mode == Some(KdeMode::Server));

                tracing::debug!(
                    "xdg ack_configure: app_id={:?} xdg_decoration={:?} kde_decoration={:?} -> is_ssd={}",
                    app_id,
                    configure.state.decoration_mode,
                    kde_mode,
                    is_ssd
                );
                let was_ssd = window.ssd_negotiated();
                window.set_ssd(is_ssd);

                // Re-fit a maximized window when its decoration changes.
                // vela asks to be maximized the moment its toplevel exists
                // (Firefox maximizes a first-run window on any screen smaller
                // than 1280x1040), and it does so BEFORE it negotiates
                // server-side decoration. So maximize_request saw
                // is_ssd=false and sized the client to the whole output. When
                // SSD then turned on, the 32 px header pushed the client's
                // bottom rows off the screen: every app on the 1280x800 VM
                // was a 1280x832 frame. Found 2026-09-25, in the first
                // harness screenshot of an app window.
                let acked_maximized = configure.state.states.contains(xdg_toplevel::State::Maximized);
                if was_ssd != is_ssd && acked_maximized {
                    if let (Some(toplevel), Some(geometry)) =
                        (window.0.toplevel().cloned(), self.maximize_target(&window))
                    {
                        let client_size = maximized_client_size(&window, geometry.size);
                        tracing::info!(
                            "decoration changed on a maximized window: is_ssd={} -> re-fit client to {:?}",
                            is_ssd,
                            client_size
                        );
                        toplevel.with_pending_state(|state| state.size = Some(client_size));
                        toplevel.send_configure();
                    }
                }

                // Bind the surface to its AppHost session once its app id is
                // known -- which this ack is among the first points of, and
                // never at map time (see `add_window_to_workspace`).
                // `sync_surface_binding` sends nothing when the id is the one
                // already bound, so this is cheap on every ack. It used to
                // call `bind_surface_remote` here unconditionally, which
                // waited on the daemon's reply, on this thread.
                #[cfg(feature = "echo-ipc")]
                self.sync_surface_binding(&window);
            }
            self.maybe_init_shell_fullscreen(&surface);
        }
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        wl_output: Option<wl_output::WlOutput>,
    ) {
        // Looked up where minimized windows still are, and every step is an
        // early return rather than an unwrap: a minimized window can ask for
        // fullscreen, and the lookup used to panic the compositor on it.
        let Some(window) = self.tracked_window_for_toplevel(&surface) else {
            return;
        };
        let is_shell = window.is_shell_window();

        // A window that is not on any output yet is one still being set up,
        // and its app id is not known either, so it can't be told from the
        // shell. The shell asks for fullscreen at birth, and must not take
        // the output exclusively: guessing "an app" here put the whole screen
        // under a blank window. So the request waits (the shell is driven
        // from the compositor side once its id lands, see
        // `maybe_init_shell_fullscreen`). Not so for a minimized window, which
        // is on no output either, but is known: it takes the primary one.
        let known = is_shell
            || wl_output.is_some()
            || !self.space.outputs_for_element(&window).is_empty()
            || self.workspace_manager.is_minimized(&window);
        let output = known
            .then(|| self.output_for_fullscreen(&window, wl_output.as_ref()))
            .flatten();
        let Some(geometry) = output.as_ref().and_then(|o| self.space.output_geometry(o)) else {
            if surface.is_initial_configure_sent() {
                surface.send_configure();
            }
            return;
        };
        let Some(output) = output else {
            return;
        };
        // The client's own handle on that output, if it has bound one.
        let client_output = self
            .display_handle
            .get_client(surface.wl_surface().id())
            .ok()
            .and_then(|client| output.client_outputs(&client).next());

        // The desktop shell (Bloom) is launched PYXIS_DISPLAY_MODE=
        // fullscreen purely so GTK drops its titlebar (a Wayland toplevel
        // is only chromeless when fullscreen/kiosk/undecorated -- see
        // vela-gtk-titlebar-decoration). But unlike a real fullscreen app
        // the shell must sit BEHIND ordinary app windows, not take over
        // the output. Evo's exclusive-fullscreen render path (render.rs's
        // FullscreenSurface) draws ONLY that one surface, which is exactly
        // what made every app launched from the shell invisible. So for
        // the shell we still send the fullscreen state+size below (that's
        // what makes Firefox undecorate), but instead of registering it as
        // the output's exclusive FullscreenSurface we map it at the output
        // origin as an ordinary bottom-of-stack space window
        // (activate=false, so app windows raised later stay on top).
        // Real fullscreen apps (anything that isn't the shell) keep the
        // exclusive scanout path. See evo-firefox-decoration-arch.
        if !is_shell {
            // The snapshot of the frame it is leaving, header included (which
            // is suppressed below).
            if self
                .workspace_manager
                .state(&window)
                .map_or(WindowLayout::Floating, |s| s.layout)
                != WindowLayout::Fullscreen
            {
                self.begin_geometry_transition(&window, Some(geometry.size));
            }
            // Remember where it was and what it was, so leaving fullscreen
            // puts it back. Before the header is suppressed below: the frame
            // size includes it. Not for a window that has never had a real
            // place (born fullscreen): there is nothing to go back to.
            let layout = self
                .workspace_manager
                .state(&window)
                .map_or(WindowLayout::Floating, |s| s.layout);
            if layout != WindowLayout::Fullscreen && !window.placement_pending() {
                if let Some(frame) = self.window_frame(&window) {
                    window.user_data().insert_if_missing(PreFullscreen::default);
                    if let Some(saved) = window.user_data().get::<PreFullscreen>() {
                        saved.save(PreFullscreenState {
                            location: self
                                .space
                                .element_location(&window)
                                .unwrap_or(frame.loc),
                            size: window.geometry().size,
                            layout,
                        });
                    }
                }
            }
            // A fullscreen window is bare: no header, and the client gets the
            // whole output.
            window.set_ssd_suppressed(true);
            self.set_window_layout(&window, WindowLayout::Fullscreen);
        }

        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Fullscreen);
            state.size = Some(geometry.size);
            state.fullscreen_output = client_output;
        });

        if is_shell {
            self.place_window(&window, geometry.loc, false);
            trace!("Shell fullscreen (non-exclusive): {:?}", window);
        } else {
            self.place_window(&window, geometry.loc, true);
            // A minimized window is only marked: it takes the output when it
            // is restored (`unminimize_window`).
            if self.is_window_mapped(&window) {
                self.set_exclusive_fullscreen(&window, &output);
                self.focus_window(&window);
            }
            trace!("Fullscreening: {:?}", window);
        }

        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let window = self.tracked_window_for_toplevel(&surface);
        let saved = window
            .as_ref()
            .and_then(|w| w.user_data().get::<PreFullscreen>().and_then(PreFullscreen::take));
        if let Some(window) = &window {
            if self
                .workspace_manager
                .state(window)
                .is_some_and(|s| s.layout == WindowLayout::Fullscreen)
            {
                // Before the header comes back and the output is released.
                self.begin_geometry_transition(window, None);
            }
            self.clear_exclusive_fullscreen(window);
            window.set_ssd_suppressed(false);
        }

        surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Fullscreen);
            state.fullscreen_output.take();
            // "You choose": a client that was never placed (born
            // fullscreen) or has nothing saved picks its own size.
            state.size = None;
        });

        if let Some(window) = window {
            trace!("Unfullscreening: {:?}", window);
            window.begin_settling();
            match saved {
                Some(saved) if saved.layout == WindowLayout::Maximized => {
                    // It was maximized before it went fullscreen, so it
                    // goes back to maximized, not to a floating place.
                    if let Some(target) = self.maximize_target(&window) {
                        let client_size = maximized_client_size(&window, target.size);
                        surface.with_pending_state(|state| state.size = Some(client_size));
                        self.place_window(&window, target.loc, true);
                    }
                    self.set_window_layout(&window, WindowLayout::Maximized);
                }
                Some(saved) => {
                    let header = window.decoration_height();
                    let client_size = (saved.size.w, (saved.size.h - header).max(1)).into();
                    surface.with_pending_state(|state| state.size = Some(client_size));
                    let location = self.pull_into_work_area(&window, saved.location);
                    self.place_window(&window, location, true);
                    self.set_window_layout(&window, WindowLayout::Floating);
                }
                None => self.set_window_layout(&window, WindowLayout::Floating),
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
        let Some(window) = self.tracked_window_for_toplevel(&surface) else {
            return;
        };
        let Some(geometry) = self.maximize_target(&window) else {
            return;
        };
        let layout = self
            .workspace_manager
            .state(&window)
            .map_or(WindowLayout::Floating, |s| s.layout);
        if layout == WindowLayout::Fullscreen {
            // Already filling the output, and leaving fullscreen is the
            // client's to ask for.
            return;
        }

        // Draw the frame growing out of the old one: the snapshot is taken
        // here, before anything below changes the window.
        if layout != WindowLayout::Maximized {
            self.begin_geometry_transition(&window, Some(geometry.size));
        }

        // Remember pre-maximize geometry so unmaximize_request can put the
        // window back where it was. Not for a window that has never had a
        // real place (PlacementPending): one maximized at birth would
        // otherwise "restore" to the blind guess it was created at. And not
        // when it is already maximized, where the frame on hand is the
        // maximized one and would overwrite the place worth going back to.
        if window.placement_pending() || layout == WindowLayout::Maximized {
            // Nothing worth remembering.
        } else if let Some(old_geo) = self.window_frame(&window) {
            window.user_data().insert_if_missing(OldGeometry::default);
            if let Some(saved) = window.user_data().get::<OldGeometry>() {
                saved.save(old_geo);
            }
        }

        let client_size = maximized_client_size(&window, geometry.size);
        tracing::info!(
            "maximize_request: output_size={:?} is_ssd={} client_size={:?}",
            geometry.size,
            window.is_ssd(),
            client_size
        );

        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Maximized);
            state.size = Some(client_size);
        });
        self.place_window(&window, geometry.loc, true);
        self.set_window_layout(&window, WindowLayout::Maximized);

        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        // Defensive early-return (see maximize_request): never panic here.
        let Some(window) = self.tracked_window_for_toplevel(&surface) else {
            return;
        };
        if self
            .workspace_manager
            .state(&window)
            .is_some_and(|s| s.layout == WindowLayout::Fullscreen)
        {
            // Fullscreen outranks it; the client only lets go of the flag.
            surface.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Maximized);
            });
            if surface.is_initial_configure_sent() {
                surface.send_configure();
            }
            return;
        }
        // Only a window that is maximized has a frame to leave: a client that
        // lets go of a flag it never had must not freeze its picture waiting
        // for a size that will not change.
        if self
            .workspace_manager
            .state(&window)
            .is_some_and(|s| s.layout == WindowLayout::Maximized)
        {
            self.begin_geometry_transition(&window, None);
        }
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

        window.begin_settling();
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
            let output_geo = super::work_area::of_output(self.space.output_geometry(&output)?);
            let size = self
                .window_frame(&window)
                .map(|frame| frame.size)
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
            // Where it was before may be under the shell's top bar now (it
            // was placed before Bloom reported a work area). Only its
            // top-left is moved here: its size is the client's to choose, and
            // `fit_to_work_area` fits the rest when it commits.
            let loc = self.pull_into_work_area(&window, loc);
            self.place_window(&window, loc, true);
        }
        self.set_window_layout(&window, WindowLayout::Floating);

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
        let Some(window) = self.tracked_window_for_toplevel(&surface) else {
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
        // Trusted clients only: a window that merely claims to be the shell
        // gets no fullscreen of its own choosing.
        if !window.is_shell_window() {
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

    /// The area a maximized `window` fills: its own output's work area (the
    /// output minus the edges the shell reserves, see `work_area`), or the
    /// first output's if it has none yet (a window maximized before it was
    /// ever mapped onto an output).
    pub(super) fn maximize_target(&self, window: &WindowElement) -> Option<Rectangle<i32, Logical>> {
        let output = self
            .space
            .outputs_for_element(window)
            .first()
            .cloned()
            .or_else(|| self.space.outputs().next().cloned())?;
        self.space
            .output_geometry(&output)
            .map(super::work_area::of_output)
    }

    /// Start an interactive resize of `surface` from the press that `serial`
    /// belongs to. There are two callers: a client's own
    /// `xdg_toplevel.resize` (`resize_request`), and a press on the resize
    /// band of Evo's SSD frame (`HeaderBar::handle_resize`).
    ///
    /// `initial_window_size` is the frame size, header bar included.
    /// Conversion to the client's own size happens in the grab, where it
    /// sends configures.
    pub fn resize_request_xdg(
        &mut self,
        surface: &ToplevelSurface,
        seat: &Seat<Self>,
        serial: Serial,
        edges: ResizeEdge,
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
                        edges,
                        initial_window_location,
                        initial_window_size,
                    });
                });

                let grab = TouchResizeSurfaceGrab {
                    start_data,
                    window,
                    edges,
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

        // Not unwrap(): the SSD path gets here from an idle callback, and the
        // window can close between the press and the callback.
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
        let Some(loc) = self.space.element_location(&window) else {
            return;
        };
        let (initial_window_location, initial_window_size) = (loc, geometry.size);

        with_states(surface.wl_surface(), move |states| {
            states
                .data_map
                .get::<RefCell<SurfaceData>>()
                .unwrap()
                .borrow_mut()
                .resize_state = ResizeState::Resizing(ResizeData {
                edges,
                initial_window_location,
                initial_window_size,
            });
        });

        let grab = PointerResizeSurfaceGrab {
            start_data,
            window,
            edges,
            initial_window_location,
            initial_window_size,
            last_window_size: initial_window_size,
        };

        pointer.set_grab(self, grab, serial, Focus::Clear);
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
                    self.set_window_layout(&window, WindowLayout::Floating);
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
            self.set_window_layout(&window, WindowLayout::Floating);
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

impl<BackendData: Backend> EvoState<BackendData> {
    /// Give a window with a guessed location (PlacementPending) its real
    /// place, once it has a size and floats. The compositor's commit handler
    /// calls this right after `Window::on_commit`. That is the one point
    /// where the size is current: in a post-commit hook, which runs first,
    /// the first buffer's size is not in yet. vela paints into a subsurface
    /// and rarely commits its toplevel again, so the hook version never
    /// placed a vela window at all (found on device, 2026-09-25).
    pub(crate) fn settle_placement(&mut self, window: &WindowElement) {
        if window.is_shell() || window.is_overlay() {
            // The shell places itself (fullscreen, see
            // maybe_init_shell_fullscreen). Never centre it.
            window.set_placement_pending(false);
        } else if let Some(location) = first_real_placement(&self.space, window) {
            tracing::info!(
                "placing {:?} ({:?}) at {:?}, now that it has a size",
                window.app_id(),
                window.geometry().size,
                location
            );
            window.set_placement_pending(false);
            self.place_window_initially(window, location);
        }
    }

    /// Follow a top-left resize: when the window's left or top edge is the
    /// one being dragged, its location moves as its size changes.
    pub(crate) fn handle_toplevel_commit(&mut self, surface: &WlSurface) -> Option<()> {
        let window = self.tracked_window_for_surface(surface)?;

        let mut window_loc = self.space.element_location(&window)?;
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
            self.place_window(&window, window_loc, false);
        }

        Some(())
    }
}

/// Where a floating window goes once its size is known: centred on its
/// output. `None` while there is nothing to place yet, because it has no size
/// (no buffer) or fills its output (maximized or fullscreen). The mark stays
/// set until the window is both sized and floating.
fn first_real_placement(space: &Space<WindowElement>, window: &WindowElement) -> Option<Point<i32, Logical>> {
    // geometry() includes an SSD header even before the client has a
    // buffer, so "no size yet" is a height of just the decoration.
    let size = window.geometry().size;
    if size.w <= 0 || size.h <= window.decoration_height() {
        return None;
    }
    let fills_output = window.0.toplevel()?.with_committed_state(|state| {
        state.is_some_and(|state| {
            state.states.contains(xdg_toplevel::State::Maximized)
                || state.states.contains(xdg_toplevel::State::Fullscreen)
        })
    });
    if fills_output {
        return None;
    }
    let output = space
        .outputs_for_element(window)
        .first()
        .cloned()
        .or_else(|| space.outputs().next().cloned())?;
    let area = super::work_area::of_output(space.output_geometry(&output)?);
    // Clamped at the top-left, so a window bigger than its output keeps its
    // title bar and buttons on screen.
    Some(Point::from((
        area.loc.x + ((area.size.w - size.w) / 2).max(0),
        area.loc.y + ((area.size.h - size.h) / 2).max(0),
    )))
}

/// The client size that makes a maximized `window` fill an output of
/// `output_size`. A server-decorated window's client sits BELOW Evo's
/// HEADER_BAR_HEIGHT-tall title bar (element.rs's geometry and bbox add the
/// header, and the render path pushes the client down by it). So the client
/// must be that much shorter than the output, or header and client together
/// overflow the screen and the client's bottom rows fall off.
pub(super) fn maximized_client_size(window: &WindowElement, output_size: Size<i32, Logical>) -> Size<i32, Logical> {
    let mut client_size = output_size;
    client_size.h = (client_size.h - window.decoration_height()).max(1);
    client_size
}
