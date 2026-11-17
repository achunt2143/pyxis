use std::{cell::RefCell, os::unix::io::OwnedFd};

use smithay::{
    desktop::{space::{RenderZindex, SpaceElement}, Window},
    input::{pointer::Focus, Seat},
    utils::{Logical, Rectangle, Serial, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        selection::{
            data_device::{
                clear_data_device_selection, current_data_device_selection_userdata,
                request_data_device_client_selection, set_data_device_selection,
            },
            primary_selection::{
                clear_primary_selection, current_primary_selection_userdata,
                request_primary_client_selection, set_primary_selection,
            },
            SelectionTarget,
        },
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        xwm::{Reorder, ResizeEdge as X11ResizeEdge, XwmId},
        X11Surface, X11Wm, XwmHandler,
    },
};
use tracing::{error, trace, warn};

use crate::{focus::KeyboardFocusTarget, state::Backend, EvoState};

use super::{
    element::{FullscreenOutputName, TileState},
    grabs::{client_size, ResizeEdge},
    placement::{default_restore_frame_rect, PreFullscreen, PreFullscreenState},
    OldGeometry, PointerMoveSurfaceGrab, PointerResizeSurfaceGrab, ResizeData, ResizeState,
    SurfaceData, TouchMoveSurfaceGrab, TouchResizeSurfaceGrab, WindowElement, WindowLayout,
};

impl<BackendData: Backend> XWaylandShellHandler for EvoState<BackendData> {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}

impl<BackendData: Backend> XwmHandler for EvoState<BackendData> {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().unwrap()
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Err(err) = window.set_mapped(true) {
            warn!("X11 map_window_request: set_mapped failed: {:?}", err);
            return;
        }
        let window = WindowElement(Window::new_x11_window(window));
        let location = self.compute_new_window_location(self.pointer.current_location(), &window);
        // The same registration as an xdg toplevel: workspace record, space,
        // `windows_by_id`, `WindowCreated`, and (once its class is known)
        // the appHost binding. These windows used to bypass all of it, so
        // their SSD minimize button hid a window nothing could ever find
        // again.
        self.add_window_to_workspace(window.clone(), location);
        let Some(xsurface) = window.0.x11_surface().cloned() else {
            warn!("X11 map_window_request: created Window has no x11_surface");
            return;
        };
        window.set_ssd(!xsurface.is_decorated());
        if xsurface.is_fullscreen() {
            self.fullscreen_request(_xwm, xsurface);
        } else if xsurface.is_maximized() {
            self.maximize_request_x11(&xsurface);
        } else {
            let _ = xsurface.configure(Some(Rectangle::new(location, xsurface.geometry().size)));
        }
        self.focus_window(&window);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let location = window.geometry().loc;
        let window = Window::new_x11_window(window);
        // Real root cause of the "override-redirect ordering" gap (found via
        // Smithay source, not guessed): `Window::new_x11_window` always
        // initializes its z-index to `RenderZindex::Shell` regardless of
        // override-redirect status — the underlying `X11Surface`'s own
        // `SpaceElement::z_index()` impl DOES correctly return `Overlay` for
        // override-redirect surfaces, but that impl is never actually
        // consulted once wrapped in the generic `Window`, which stores its
        // own independent z-index field instead of delegating. Override it
        // explicitly so menus/tooltips/drag icons (what override-redirect
        // actually means in X11 — never regular app windows) render above
        // the normal shell stack instead of competing with it on raw
        // insertion order, which is what caused "always mapped top, no
        // regard for order relative to sibling shell windows."
        window.override_z_index(RenderZindex::Overlay as u8);
        let window = WindowElement(window);
        self.space.map_element(window, location, true);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        // A client withdrawing its window. A managed one comes back through
        // `map_window_request` as a new element, so this one goes entirely
        // (`destroyed_window` is the same).
        self.forget_x11_window(&window);
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.forget_x11_window(&window);
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _x: Option<i32>,
        _y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        // we just set the new size, but don't let windows move themselves around freely
        let mut geo = window.geometry();
        if let Some(w) = w {
            geo.size.w = w as i32;
        }
        if let Some(h) = h {
            geo.size.h = h as i32;
        }
        let _ = window.configure(geo);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        let Some(elem) = self.tracked_window_for_x11(&window) else {
            return;
        };
        self.place_window(&elem, geometry.loc, false);
        // The class-level ordering bug this used to describe (override-
        // redirect windows sharing the same z-index tier as regular shell
        // windows, so a plain app re-map could bury a menu/tooltip) is fixed
        // by `mapped_override_redirect_window`'s `override_z_index` call —
        // override-redirect windows now always sort into `RenderZindex::
        // Overlay`, above every regular shell window regardless of map
        // order. What's still not implemented is precise *relative* order
        // among MULTIPLE simultaneous override-redirect siblings via the
        // `_above` hint above (Smithay's `Space` only exposes push-to-top
        // insertion within a z-index tier, not "insert directly above a
        // named sibling") — in practice this call already re-inserts `elem`
        // at the top of the Overlay tier on every reconfigure, so the most-
        // recently-configured override-redirect window wins ties, which
        // covers the common single-popup case even without honoring `_above`
        // exactly.
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        self.maximize_request_x11(&window);
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        self.unmaximize_request_x11(&window);
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let Some(elem) = self.tracked_window_for_x11(&window) else {
            return;
        };
        // The window may not be on an output yet (a minimized one is on
        // none): the primary output then.
        let Some(output) = self.output_for_fullscreen(&elem, None) else {
            return;
        };
        let Some(geometry) = self.space.output_geometry(&output) else {
            return;
        };

        let layout = self
            .workspace_manager
            .state(&elem)
            .map_or(WindowLayout::Floating, |s| s.layout);
        if layout != WindowLayout::Fullscreen {
            self.begin_geometry_transition(&elem, Some(geometry.size));
            if let Some(frame) = self.window_frame(&elem) {
                elem.user_data().insert_if_missing(PreFullscreen::default);
                if let Some(saved) = elem.user_data().get::<PreFullscreen>() {
                    saved.save(PreFullscreenState {
                        location: self.space.element_location(&elem).unwrap_or(frame.loc),
                        size: elem.geometry().size,
                        layout,
                    });
                }
            }
        }

        let _ = window.set_fullscreen(true);
        elem.set_ssd_suppressed(true);
        elem.user_data().insert_if_missing(FullscreenOutputName::default);
        if let Some(slot) = elem.user_data().get::<FullscreenOutputName>() {
            slot.set(Some(output.name()));
        }
        let _ = window.configure(geometry);
        self.place_window(&elem, geometry.loc, true);
        self.set_window_layout(&elem, WindowLayout::Fullscreen);
        if self.is_window_mapped(&elem) {
            self.set_exclusive_fullscreen(&elem, &output);
            self.focus_window(&elem);
        }
        trace!("Fullscreening: {:?}", elem);
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let Some(elem) = self.tracked_window_for_x11(&window) else {
            return;
        };
        if self
            .workspace_manager
            .state(&elem)
            .is_some_and(|s| s.layout == WindowLayout::Fullscreen)
        {
            self.begin_geometry_transition(&elem, None);
        }
        let _ = window.set_fullscreen(false);
        elem.set_ssd(!window.is_decorated());
        elem.set_ssd_suppressed(false);
        if let Some(slot) = elem.user_data().get::<FullscreenOutputName>() {
            slot.set(None);
        }
        self.clear_exclusive_fullscreen(&elem);

        let saved = elem.user_data().get::<PreFullscreen>().and_then(PreFullscreen::take);
        match saved {
            Some(saved) => {
                let header = elem.decoration_height();
                let _ = window.configure(Rectangle::new(
                    saved.location,
                    (saved.size.w, (saved.size.h - header).max(1)).into(),
                ));
                self.place_window(&elem, saved.location, true);
                self.set_window_layout(&elem, saved.layout);
            }
            None => self.set_window_layout(&elem, WindowLayout::Floating),
        }
        trace!("Unfullscreening: {:?}", elem);
    }

    fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(elem) = self.tracked_window_for_x11(&window) {
            self.minimize_window(&elem);
        }
    }

    fn unminimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(elem) = self.tracked_window_for_x11(&window) {
            self.focus_window(&elem);
        }
    }

    fn resize_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32, edges: X11ResizeEdge) {
        let seat = self.seat.clone();
        self.start_x11_resize(&window, &seat, None, edges.into());
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        self.move_request_x11(&window)
    }

    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        if let Some(keyboard) = self.seat.get_keyboard() {
            // check that an X11 window is focused
            if let Some(KeyboardFocusTarget::Window(w)) = keyboard.current_focus() {
                if let Some(surface) = w.x11_surface() {
                    if surface.xwm_id() == Some(xwm) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn send_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        match selection {
            SelectionTarget::Clipboard => {
                if let Err(err) = request_data_device_client_selection(&self.seat, mime_type, fd) {
                    error!(?err, "Failed to request current wayland clipboard for Xwayland",);
                }
            }
            SelectionTarget::Primary => {
                if let Err(err) = request_primary_client_selection(&self.seat, mime_type, fd) {
                    error!(
                        ?err,
                        "Failed to request current wayland primary selection for Xwayland",
                    );
                }
            }
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        trace!(?selection, ?mime_types, "Got Selection from X11",);
        // TODO check, that focused windows is X11 window before doing this
        match selection {
            SelectionTarget::Clipboard => {
                set_data_device_selection(&self.display_handle, &self.seat, mime_types, ())
            }
            SelectionTarget::Primary => {
                set_primary_selection(&self.display_handle, &self.seat, mime_types, ())
            }
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => {
                if current_data_device_selection_userdata(&self.seat).is_some() {
                    clear_data_device_selection(&self.display_handle, &self.seat)
                }
            }
            SelectionTarget::Primary => {
                if current_primary_selection_userdata(&self.seat).is_some() {
                    clear_primary_selection(&self.display_handle, &self.seat)
                }
            }
        }
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        self.xwm = None;
    }
}

impl<BackendData: Backend> EvoState<BackendData> {
    /// An X11 window is gone from the client's side (withdrawn or
    /// destroyed): drop it from the workspace, the space, and Echo.
    fn forget_x11_window(&mut self, window: &X11Surface) {
        if let Some(elem) = self.tracked_window_for_x11(window) {
            self.remove_window_from_workspace(&elem);
        }
    }

    pub fn resize_request_x11(
        &mut self,
        window: &X11Surface,
        seat: &Seat<Self>,
        serial: Serial,
        edges: ResizeEdge,
    ) {
        self.start_x11_resize(window, seat, Some(serial), edges);
    }

    fn start_x11_resize(
        &mut self,
        window: &X11Surface,
        seat: &Seat<Self>,
        serial: Option<Serial>,
        edges: ResizeEdge,
    ) {
        let Some(element) = self.tracked_window_for_x11(window) else {
            warn!("X11 resize request: window is not tracked");
            return;
        };
        let Some(loc) = self.space.element_location(&element) else {
            warn!("X11 resize request: window has no location in space");
            return;
        };
        let Some(wl_surface) = element.wl_surface().map(|s| s.into_owned()) else {
            warn!("X11 resize request: window has no wl_surface");
            return;
        };

        if let Some(tile) = element.user_data().get::<TileState>() {
            tile.take();
        }
        if self
            .workspace_manager
            .state(&element)
            .is_some_and(|s| s.layout == WindowLayout::Tiled)
        {
            self.set_window_layout(&element, WindowLayout::Floating);
        }

        let geometry = element.geometry();
        let (initial_window_location, initial_window_size) = (loc, geometry.size);

        if let (Some(serial), Some(touch)) = (serial, seat.get_touch()) {
            if touch.has_grab(serial) {
                let Some(start_data) = touch.grab_start_data() else {
                    warn!("X11 touch resize request: missing grab_start_data");
                    return;
                };
                with_states(&wl_surface, move |states| {
                    states.data_map.insert_if_missing(RefCell::<SurfaceData>::default);
                    if let Some(surface_data) = states.data_map.get::<RefCell<SurfaceData>>() {
                        surface_data.borrow_mut().resize_state = ResizeState::Resizing(ResizeData {
                            edges,
                            initial_window_location,
                            initial_window_size,
                        });
                    }
                });
                let grab = TouchResizeSurfaceGrab {
                    start_data,
                    window: element,
                    edges,
                    initial_window_location,
                    initial_window_size,
                    last_window_size: initial_window_size,
                };
                touch.set_grab(self, grab, serial);
                return;
            }
        }

        let pointer = self.pointer.clone();
        if let Some(serial) = serial {
            if !pointer.has_grab(serial) {
                warn!("X11 pointer resize request: pointer no longer has grab for {:?}", serial);
                return;
            }
        }
        let Some(start_data) = pointer.grab_start_data() else {
            warn!("X11 resize request: pointer has no grab_start_data");
            return;
        };

        with_states(&wl_surface, move |states| {
            states.data_map.insert_if_missing(RefCell::<SurfaceData>::default);
            if let Some(surface_data) = states.data_map.get::<RefCell<SurfaceData>>() {
                surface_data.borrow_mut().resize_state = ResizeState::Resizing(ResizeData {
                    edges,
                    initial_window_location,
                    initial_window_size,
                });
            }
        });

        let grab = PointerResizeSurfaceGrab {
            start_data,
            window: element,
            edges,
            initial_window_location,
            initial_window_size,
            last_window_size: initial_window_size,
        };
        let grab_serial = serial.unwrap_or_else(|| SERIAL_COUNTER.next_serial());
        pointer.set_grab(self, grab, grab_serial, Focus::Clear);
    }

    pub fn maximize_request_x11(&mut self, window: &X11Surface) {
        let Some(elem) = self.tracked_window_for_x11(window) else {
            return;
        };
        let Some(geometry) = self.work_area_for(&elem) else {
            return;
        };
        let layout = self
            .workspace_manager
            .state(&elem)
            .map_or(WindowLayout::Floating, |s| s.layout);
        if layout == WindowLayout::Fullscreen {
            return;
        }

        // Not when already maximized: the frame on hand would overwrite the
        // place worth restoring to. If tiled, preserve the original pre-tile
        // floating geometry instead of the half-screen frame.
        let pre_tile = elem.user_data().get::<TileState>().and_then(TileState::take);
        if layout != WindowLayout::Maximized {
            self.begin_geometry_transition(&elem, Some(geometry.size));
            let restore_geo = pre_tile
                .map(|t| t.restore_geo)
                .or_else(|| self.window_frame(&elem));
            if let Some(old_geo) = restore_geo {
                window.user_data().insert_if_missing(OldGeometry::default);
                if let Some(saved) = window.user_data().get::<OldGeometry>() {
                    saved.save(old_geo);
                }
            }
        }
        if let Err(err) = window.set_maximized(true) {
            warn!("X11 maximize_request_x11: set_maximized failed: {:?}", err);
        }
        let target = Rectangle::new(geometry.loc, client_size(&elem, geometry.size));
        if let Err(err) = window.configure(target) {
            warn!("X11 maximize_request_x11: configure failed: {:?}", err);
        }
        self.place_window(&elem, geometry.loc, false);
        self.set_window_layout(&elem, WindowLayout::Maximized);
    }

    /// Restores a window maximized via `maximize_request_x11` back to its
    /// pre-maximize geometry (or a centered default floating rect if self-maximized).
    pub fn unmaximize_request_x11(&mut self, window: &X11Surface) {
        let Some(elem) = self.tracked_window_for_x11(window) else {
            return;
        };

        if self
            .workspace_manager
            .state(&elem)
            .is_some_and(|s| s.layout == WindowLayout::Maximized)
        {
            // (Not the saved size: reading `OldGeometry` takes it.)
            self.begin_geometry_transition(&elem, None);
        }
        if let Err(err) = window.set_maximized(false) {
            warn!("X11 unmaximize_request_x11: set_maximized(false) failed: {:?}", err);
        }
        let target = window
            .user_data()
            .get::<OldGeometry>()
            .and_then(|data| data.restore())
            .or_else(|| self.work_area_for(&elem).map(default_restore_frame_rect));
        if let Some(rect) = target {
            let loc = self.pull_into_work_area(&elem, rect.loc);
            let configure_rect = Rectangle::new(loc, client_size(&elem, rect.size));
            if let Err(err) = window.configure(configure_rect) {
                warn!("X11 unmaximize_request_x11: configure failed: {:?}", err);
            }
            self.place_window(&elem, loc, false);
        }
        self.set_window_layout(&elem, WindowLayout::Floating);
    }

    pub fn move_request_x11(&mut self, window: &X11Surface) {
        if let Some(touch) = self.seat.get_touch() {
            if let Some(start_data) = touch.grab_start_data() {
                if let Some(element) = self.tracked_window_for_x11(window) {
                    let Some(initial_window_location) = self.space.element_location(&element) else {
                        warn!("X11 touch move request: window has no location in space");
                        return;
                    };
                    let grab = TouchMoveSurfaceGrab {
                        start_data,
                        window: element,
                        initial_window_location,
                        pending_unmaximize: window.is_maximized(),
                    };
                    touch.set_grab(self, grab, SERIAL_COUNTER.next_serial());
                    return;
                }
            }
        }

        // luckily Evo only supports one seat anyway...
        let Some(start_data) = self.pointer.grab_start_data() else {
            return;
        };
        let Some(element) = self.tracked_window_for_x11(window) else {
            return;
        };
        let Some(initial_window_location) = self.space.element_location(&element) else {
            warn!("X11 pointer move request: window has no location in space");
            return;
        };

        let grab = PointerMoveSurfaceGrab {
            start_data,
            window: element,
            initial_window_location,
            pending_unmaximize: window.is_maximized(),
        };

        let pointer = self.pointer.clone();
        pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    }
}
