use std::{cell::RefCell, os::unix::io::OwnedFd};

use smithay::{
    desktop::{space::{RenderZindex, SpaceElement}, Window},
    input::pointer::Focus,
    utils::{Logical, Rectangle, SERIAL_COUNTER},
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
use tracing::{error, trace};

use crate::{focus::KeyboardFocusTarget, state::Backend, EvoState};

use super::{
    placement::{PreFullscreen, PreFullscreenState},
    OldGeometry, PointerMoveSurfaceGrab, PointerResizeSurfaceGrab, ResizeData, ResizeState,
    SurfaceData, TouchMoveSurfaceGrab, WindowElement, WindowLayout,
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
        window.set_mapped(true).unwrap();
        let window = WindowElement(Window::new_x11_window(window));
        let location = self.compute_new_window_location(self.pointer.current_location(), &window);
        // The same registration as an xdg toplevel: workspace record, space,
        // `windows_by_id`, `WindowCreated`, and (once its class is known)
        // the appHost binding. These windows used to bypass all of it, so
        // their SSD minimize button hid a window nothing could ever find
        // again.
        self.add_window_to_workspace(window.clone(), location);
        let Some(xsurface) = window.0.x11_surface() else {
            unreachable!()
        };
        let _ = xsurface.configure(Some(Rectangle::new(location, xsurface.geometry().size)));
        window.set_ssd(!xsurface.is_decorated());
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
        // luckily Evo only supports one seat anyway...
        let start_data = self.pointer.grab_start_data().unwrap();

        let Some(element) = self.tracked_window_for_x11(&window) else {
            return;
        };
        let Some(loc) = self.space.element_location(&element) else {
            return;
        };
        let geometry = element.geometry();
        let (initial_window_location, initial_window_size) = (loc, geometry.size);

        let Some(wl_surface) = element.wl_surface().map(|s| s.into_owned()) else {
            return;
        };
        with_states(&wl_surface, move |states| {
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
            window: element,
            edges: edges.into(),
            initial_window_location,
            initial_window_size,
            last_window_size: initial_window_size,
        };

        let pointer = self.pointer.clone();
        pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        self.move_request_x11(&window)
    }

    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        if let Some(keyboard) = self.seat.get_keyboard() {
            // check that an X11 window is focused
            if let Some(KeyboardFocusTarget::Window(w)) = keyboard.current_focus() {
                if let Some(surface) = w.x11_surface() {
                    if surface.xwm_id().unwrap() == xwm {
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

    pub fn maximize_request_x11(&mut self, window: &X11Surface) {
        let Some(elem) = self.tracked_window_for_x11(window) else {
            return;
        };
        let Some(output) = self.output_for_fullscreen(&elem, None) else {
            return;
        };
        let Some(geometry) = self.space.output_geometry(&output).map(super::work_area::of_output) else {
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
        // place worth restoring to.
        if layout != WindowLayout::Maximized {
            self.begin_geometry_transition(&elem, Some(geometry.size));
            if let Some(old_geo) = self.window_frame(&elem) {
                window.user_data().insert_if_missing(OldGeometry::default);
                if let Some(saved) = window.user_data().get::<OldGeometry>() {
                    saved.save(old_geo);
                }
            }
        }
        let _ = window.set_maximized(true);
        let _ = window.configure(geometry);
        self.place_window(&elem, geometry.loc, false);
        self.set_window_layout(&elem, WindowLayout::Maximized);
    }

    /// Restores a window maximized via `maximize_request_x11` back to its
    /// pre-maximize geometry. Used both by the client-requested XWM path
    /// (`XwmHandler::unmaximize_request`) and by the SSD maximize button
    /// toggling itself off.
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
        let _ = window.set_maximized(false);
        if let Some(old_geo) = window
            .user_data()
            .get::<OldGeometry>()
            .and_then(|data| data.restore())
        {
            let _ = window.configure(old_geo);
            self.place_window(&elem, old_geo.loc, false);
        }
        self.set_window_layout(&elem, WindowLayout::Floating);
    }

    pub fn move_request_x11(&mut self, window: &X11Surface) {
        if let Some(touch) = self.seat.get_touch() {
            if let Some(start_data) = touch.grab_start_data() {
                let element = self.tracked_window_for_x11(window);

                if let Some(element) = element {
                    let Some(mut initial_window_location) = self.space.element_location(&element) else {
                        return;
                    };

                    // If surface is maximized then unmaximize it
                    if window.is_maximized() {
                        window.set_maximized(false).unwrap();
                        self.set_window_layout(&element, WindowLayout::Floating);
                        let pos = start_data.location;
                        initial_window_location = (pos.x as i32, pos.y as i32).into();
                        if let Some(old_geo) = window
                            .user_data()
                            .get::<OldGeometry>()
                            .and_then(|data| data.restore())
                        {
                            window
                                .configure(Rectangle::new(initial_window_location, old_geo.size))
                                .unwrap();
                        }
                    }

                    let grab = TouchMoveSurfaceGrab {
                        start_data,
                        window: element,
                        initial_window_location,
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

        let Some(mut initial_window_location) = self.space.element_location(&element) else {
            return;
        };

        // If surface is maximized then unmaximize it
        if window.is_maximized() {
            window.set_maximized(false).unwrap();
            self.set_window_layout(&element, WindowLayout::Floating);
            let pos = self.pointer.current_location();
            initial_window_location = (pos.x as i32, pos.y as i32).into();
            if let Some(old_geo) = window
                .user_data()
                .get::<OldGeometry>()
                .and_then(|data| data.restore())
            {
                window
                    .configure(Rectangle::new(initial_window_location, old_geo.size))
                    .unwrap();
            }
        }

        let grab = PointerMoveSurfaceGrab {
            start_data,
            window: element,
            initial_window_location,
        };

        let pointer = self.pointer.clone();
        pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    }
}
