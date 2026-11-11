//! Where a window is, which window has the keyboard, and who it is on the bus.
//!
//! Three things used to be scattered across the shell handlers, each with the
//! same failure: every handler wrote what it needed straight into the
//! `Space`, and the workspace record (`WindowState`) held only a window's
//! birth location. So a window that was moved, maximized or fullscreened and
//! then minimized came back where it was *born*, still sized for somewhere
//! else.
//!
//! The rule now: a window's location changes only through
//! [`EvoState::place_window`], and its layout only through
//! [`EvoState::set_window_layout`]. Both write the workspace record, and map
//! the element only if the window is meant to be on screen. Minimizing also
//! snapshots the space's location into the record, so a path that ever does
//! bypass `place_window` (an orphan relocated by `fixup_positions` is the one
//! known) still can't lose a window's place.

use std::cell::{Cell, RefCell};

use smithay::{
    backend::renderer::utils::with_renderer_surface_state,
    desktop::space::SpaceElement,
    output::Output,
    reexports::wayland_server::{
        protocol::{wl_output::WlOutput, wl_surface::WlSurface},
        Resource,
    },
    utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER},
    wayland::{seat::WaylandFocus, shell::xdg::ToplevelSurface},
};
use tracing::{debug, info};

use super::{
    overlay as overlay_mod,
    work_area,
    workspace::WindowLayout,
    FullscreenSurface, ResizeEdge, WindowElement,
};
use crate::{
    focus::KeyboardFocusTarget,
    state::{Backend, EvoState},
};

// ---------------------------------------------------------------------------
// Focus choice, as pure logic so it can be tested without a compositor.
// ---------------------------------------------------------------------------

/// What a window is to focus handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StackRole {
    App,
    /// The desktop. Gets focus only when no app can.
    Shell,
    /// Never takes focus: the overlay, and X11 override-redirect surfaces
    /// (menus, tooltips).
    Ignored,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StackEntry<T> {
    pub id: T,
    pub role: StackRole,
    pub minimized: bool,
}

/// Who gets the keyboard when `leaving` loses it by closing or minimizing:
/// the topmost app window below it, else the shell. `stack` runs back to
/// front, like `Space::elements`.
pub(crate) fn focus_fallback<T: Clone + PartialEq>(stack: &[StackEntry<T>], leaving: &T) -> Option<T> {
    stack
        .iter()
        .rev()
        .find(|e| e.role == StackRole::App && !e.minimized && &e.id != leaving)
        .or_else(|| {
            stack
                .iter()
                .find(|e| e.role == StackRole::Shell && &e.id != leaving)
        })
        .map(|e| e.id.clone())
}

/// Move `id` to the front of a most-recently-used list.
pub(crate) fn mru_touch<T: PartialEq>(history: &mut Vec<T>, id: T) {
    history.retain(|e| e != &id);
    history.insert(0, id);
}

/// The Alt+Tab order: `live` windows by recency, then any never focused.
pub(crate) fn cycle_order<T: Clone + PartialEq>(history: &[T], live: &[T]) -> Vec<T> {
    let mut order: Vec<T> = history.iter().filter(|h| live.contains(h)).cloned().collect();
    let unseen: Vec<T> = live.iter().filter(|l| !order.contains(l)).cloned().collect();
    order.extend(unseen);
    order
}

/// Where one more Alt+Tab (or Shift+Alt+Tab) goes from `index`, wrapping.
pub(crate) fn cycle_step(len: usize, index: usize, reverse: bool) -> usize {
    if reverse {
        (index + len - 1) % len
    } else {
        (index + 1) % len
    }
}

/// Whether a window claiming this app id is an app to appHost. Anything else
/// bound there would be rendered as a dock entry for something the user
/// never launched: `unknown` is a toplevel with no app id, and `vela-default`
/// is what a vela window that never ran browser-init's PYXIS block reports.
/// (The shell is the other case, but that is a matter of who the client is,
/// not of what it claims: see `WindowElement::is_shell_window`.)
pub(crate) fn is_bindable_app_id(app_id: &str) -> bool {
    !app_id.is_empty() && app_id != "unknown" && app_id != "vela-default"
}

/// The `BindSurface` calls to repeat after an Echo reconnect, from
/// `(window id, id it is bound under, client pid)`: one per window that is
/// bound at all, in window order.
pub(crate) fn bindings_to_replay(
    windows: impl IntoIterator<Item = (u32, Option<String>, Option<u32>)>,
) -> Vec<(String, u32, Option<u32>)> {
    let mut binds: Vec<_> = windows
        .into_iter()
        .filter_map(|(id, bound, pid)| bound.filter(|a| is_bindable_app_id(a)).map(|a| (a, id, pid)))
        .collect();
    binds.sort_by_key(|(_, id, _)| *id);
    binds
}

// ---------------------------------------------------------------------------
// Per-window markers, kept in the window's user_data.
// ---------------------------------------------------------------------------

/// Set at creation; cleared by the first commit that maps the window, which
/// is the first point its app id is reliably known, and so the first point
/// Evo can tell an app (which takes focus) from the shell (which doesn't).
#[derive(Default)]
struct FocusPending(Cell<bool>);

/// The app id this window is currently bound under in appHost, so the bind is
/// sent once, when the id first becomes known or changes.
#[derive(Default)]
struct BoundAppId(RefCell<Option<String>>);

/// What a window was before it went fullscreen, so leaving fullscreen puts it
/// back: its place and frame size, or "maximized" if that is what it was.
#[derive(Default)]
pub(crate) struct PreFullscreen(RefCell<Option<PreFullscreenState>>);

#[derive(Debug, Clone, Copy)]
pub(crate) struct PreFullscreenState {
    pub location: Point<i32, Logical>,
    /// Frame size, header included.
    pub size: smithay::utils::Size<i32, Logical>,
    pub layout: WindowLayout,
}

impl PreFullscreen {
    pub fn save(&self, state: PreFullscreenState) {
        *self.0.borrow_mut() = Some(state);
    }
    pub fn take(&self) -> Option<PreFullscreenState> {
        self.0.borrow_mut().take()
    }
}

impl WindowElement {
    pub fn set_focus_pending(&self, pending: bool) {
        self.user_data().insert_if_missing(FocusPending::default);
        if let Some(mark) = self.user_data().get::<FocusPending>() {
            mark.0.set(pending);
        }
    }

    pub fn focus_pending(&self) -> bool {
        self.user_data()
            .get::<FocusPending>()
            .is_some_and(|mark| mark.0.get())
    }

    pub(super) fn stack_role(&self) -> StackRole {
        #[cfg(feature = "xwayland")]
        if self.0.x11_surface().is_some_and(|s| s.is_override_redirect()) {
            return StackRole::Ignored;
        }
        if self.is_overlay() {
            StackRole::Ignored
        } else if self.is_shell() {
            StackRole::Shell
        } else {
            StackRole::App
        }
    }

    /// Whether this window is the desktop or its overlay. Only a trusted
    /// client's window can be (see `crate::trust`).
    pub fn is_shell_window(&self) -> bool {
        self.is_shell() || self.is_overlay()
    }
}

// ---------------------------------------------------------------------------
// The EvoState side.
// ---------------------------------------------------------------------------

impl<BackendData: Backend + 'static> EvoState<BackendData> {
    /// The window owning `surface`, whether or not it is mapped. A minimized
    /// window is out of the space, so a space-only lookup never finds it:
    /// that is how one that was destroyed while minimized used to leak.
    pub fn tracked_window_for_surface(&self, surface: &WlSurface) -> Option<WindowElement> {
        self.window_for_surface(surface.clone()).or_else(|| {
            self.workspace_manager
                .all_windows()
                .find(|w| w.wl_surface().as_deref() == Some(surface))
                .cloned()
        })
    }

    /// As `tracked_window_for_surface`, by the xdg toplevel itself.
    pub fn tracked_window_for_toplevel(&self, toplevel: &ToplevelSurface) -> Option<WindowElement> {
        let is_it = |w: &&WindowElement| w.0.toplevel().is_some_and(|t| t == toplevel);
        self.space
            .elements()
            .find(is_it)
            .or_else(|| self.workspace_manager.all_windows().find(is_it))
            .cloned()
    }

    #[cfg(feature = "xwayland")]
    pub fn tracked_window_for_x11(&self, surface: &smithay::xwayland::X11Surface) -> Option<WindowElement> {
        let is_it = |w: &&WindowElement| matches!(w.0.x11_surface(), Some(s) if s == surface);
        self.space
            .elements()
            .find(is_it)
            .or_else(|| self.workspace_manager.all_windows().find(is_it))
            .cloned()
    }

    /// Whether the window is in the space, i.e. on screen.
    pub fn is_window_mapped(&self, window: &WindowElement) -> bool {
        self.space.elements().any(|w| w == window)
    }

    /// Whether `window` is meant to be on screen: tracked, on the active
    /// workspace (or sticky), and not minimized. Untracked windows (X11
    /// override-redirect menus) are always shown.
    fn should_be_mapped(&self, window: &WindowElement) -> bool {
        let Some(state) = self.workspace_manager.state(window) else {
            return true;
        };
        if state.minimized {
            return false;
        }
        state.sticky || self.workspace_manager.workspace_for_window(window) == Some(self.workspace_manager.active_workspace_id())
    }

    /// **The** way a window changes place. Writes the workspace record, and
    /// maps the element there if it is meant to be on screen. A minimized
    /// window gets its new place recorded and stays hidden, so restoring it
    /// lands where it was last put.
    pub fn place_window(&mut self, window: &WindowElement, location: Point<i32, Logical>, activate: bool) {
        self.workspace_manager.record_location(window, location);
        if self.should_be_mapped(window) {
            self.space.map_element(window.clone(), location, activate);
        }
    }

    /// Place a window for the first time, or settle a guessed place: not a
    /// move anyone chose, so it is also what `WindowMoved` is measured from.
    pub fn place_window_initially(&mut self, window: &WindowElement, location: Point<i32, Logical>) {
        self.place_window(window, location, false);
        self.workspace_manager.set_reported_location(window, location);
    }

    /// Announce a window's place as `WindowMoved`, if it is not the one last
    /// announced. Called when a move or resize ends, not on every motion.
    pub fn window_moved(&mut self, window: &WindowElement) {
        let Some(location) = self.workspace_manager.state(window).map(|s| s.location) else {
            return;
        };
        if self.workspace_manager.set_reported_location(window, location) == Some(location) {
            return;
        }
        #[cfg(feature = "echo-ipc")]
        if let Some(id) = self.window_id_for(window) {
            let _ = self.echo_service.read().emit_window_moved(id, location.x, location.y);
        }
    }

    /// **The** way a window's layout changes. Records it and, for a real
    /// app window, announces `WindowMaximized` / `WindowFullscreen` when
    /// either flips.
    pub fn set_window_layout(&mut self, window: &WindowElement, layout: WindowLayout) {
        let Some(old) = self.workspace_manager.set_layout(window, layout) else {
            return;
        };
        if old == layout || window.is_shell_window() {
            return;
        }
        #[cfg(feature = "echo-ipc")]
        if let Some(id) = self.window_id_for(window) {
            let echo = self.echo_service.read();
            if (old == WindowLayout::Maximized) != (layout == WindowLayout::Maximized) {
                let _ = echo.emit_window_maximized(id, layout == WindowLayout::Maximized);
            }
            if (old == WindowLayout::Fullscreen) != (layout == WindowLayout::Fullscreen) {
                let _ = echo.emit_window_fullscreen(id, layout == WindowLayout::Fullscreen);
            }
        }
    }

    /// Give `window` the keyboard, raised and activated; restore it first if
    /// it was minimized. The one place an app window takes focus. The overlay
    /// never does.
    pub fn focus_window(&mut self, window: &WindowElement) {
        if window.is_overlay() {
            return;
        }
        self.raise_window(window);
        #[cfg(feature = "xwayland")]
        if let Some(surface) = window.0.x11_surface() {
            if let Some(xwm) = self.xwm.as_mut() {
                let _ = xwm.raise_window(surface);
            }
        }
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(window.clone().into()), SERIAL_COUNTER.next_serial());
        }
    }

    /// Record that `window` has the keyboard, for Alt+Tab's recency order.
    pub fn note_focused(&mut self, window: &WindowElement) {
        if window.stack_role() == StackRole::App {
            mru_touch(&mut self.focus_history, window.clone());
        }
    }

    /// Ask an app window to close, as the titlebar's X does: the client
    /// runs its own teardown. The desktop and its overlay are never closed.
    pub fn close_window(&mut self, window: &WindowElement) {
        if window.is_shell_window() {
            return;
        }
        match window.0.underlying_surface() {
            smithay::desktop::WindowSurface::Wayland(w) => w.send_close(),
            #[cfg(feature = "xwayland")]
            smithay::desktop::WindowSurface::X11(w) => {
                let _ = w.close();
            }
        }
    }

    /// Super+Q: close the app window that has the keyboard, if one does.
    pub fn close_focused_window(&mut self) {
        let focused = self
            .seat
            .get_keyboard()
            .and_then(|k| k.current_focus())
            .and_then(|f| f.wl_surface().map(|s| s.into_owned()))
            .and_then(|s| self.tracked_window_for_surface(&s));
        if let Some(window) = focused {
            self.close_window(&window);
        }
    }

    /// `leaving` is closing or being minimized. If it has the keyboard (or
    /// the keyboard has nowhere live to be), hand it to the next app window
    /// down the stack, else to the shell, else to nobody.
    pub fn refocus_after_loss(&mut self, leaving: &WindowElement) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let focus_is_fine = match keyboard.current_focus() {
            Some(KeyboardFocusTarget::Window(w)) => w != leaving.0 && w.alive(),
            Some(other) => other.alive(),
            None => false,
        };
        if focus_is_fine {
            return;
        }
        let stack: Vec<StackEntry<WindowElement>> = self
            .space
            .elements()
            .filter(|w| w.alive())
            .map(|w| StackEntry {
                id: w.clone(),
                role: w.stack_role(),
                minimized: self.workspace_manager.is_minimized(w),
            })
            .collect();
        let next = focus_fallback(&stack, leaving);
        debug!("focus handoff from a departing window: {:?}", next.as_ref().map(|w| w.app_id()));
        match next {
            Some(window) => self.focus_window(&window),
            None => keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial()),
        }
    }

    /// First commit that maps a new window: give it the keyboard, unless it
    /// is the shell or the overlay. See `FocusPending`.
    pub fn focus_new_window_if_mapped(&mut self, window: &WindowElement, root: &WlSurface) {
        let mapped = with_renderer_surface_state(root, |state| state.buffer().is_some()).unwrap_or(false);
        if !mapped {
            return;
        }
        window.set_focus_pending(false);
        if window.is_shell_window() || !self.is_window_mapped(window) {
            return;
        }
        info!("focusing new window {:?}", window.app_id());
        self.focus_window(window);
    }

    /// Bind the window's surface to its appHost session, once, when its app
    /// id first becomes known or changes. Cheap to call on every commit and
    /// ack: it sends nothing unless the id differs from the one last bound.
    ///
    /// This is the first point the id is known (see the long comment in
    /// `xdg.rs::ack_configure`, which used to re-send on every ack, waiting
    /// on a reply each time).
    #[cfg(feature = "echo-ipc")]
    pub fn sync_surface_binding(&mut self, window: &WindowElement) {
        let Some(window_id) = self.window_id_for(window) else {
            return;
        };
        // Whether there is anything to bind waits on what the client has said
        // (a vela window that has not run its PYXIS block yet claims
        // nothing), but what is bound is who the window is: for an app that
        // is the id its launcher gave the connection, whatever it claims.
        let bindable = !window.is_shell_window()
            && window
                .claimed_app_id()
                .is_some_and(|claimed| is_bindable_app_id(&claimed));
        let wanted = bindable.then(|| window.app_id()).flatten();
        window.user_data().insert_if_missing(BoundAppId::default);
        let Some(bound) = window.user_data().get::<BoundAppId>() else {
            return;
        };
        if *bound.0.borrow() == wanted {
            return;
        }
        let echo = self.echo_service.read();
        if bound.0.borrow().is_some() {
            if let Err(e) = echo.unbind_surface_remote(window_id) {
                tracing::warn!("unbind_surface({}) failed: {}", window_id, e);
            }
        }
        if let Some(app_id) = &wanted {
            if let Err(e) = echo.bind_surface_remote(app_id, window_id, self.client_pid(window)) {
                tracing::warn!("bind_surface({}, {}) failed: {}", app_id, window_id, e);
            }
        }
        *bound.0.borrow_mut() = wanted;
    }

    /// Send appHost every binding it should hold, again. Run after the Echo
    /// broker reconnects: a restarted echo-daemon's appHost starts with no
    /// bindings, and `sync_surface_binding` will not resend what it believes
    /// is already bound. The list is read from the windows themselves (the
    /// id each was last bound under, `BoundAppId`, and the client's current
    /// pid), so the broker keeps no registry. The bind is idempotent in
    /// appHost, so a replay racing a bind still queued from before the drop
    /// does no harm.
    #[cfg(feature = "echo-ipc")]
    pub fn replay_surface_bindings(&mut self) {
        let entries: Vec<(u32, Option<String>, Option<u32>)> = self
            .windows_by_id
            .iter()
            .map(|(id, window)| {
                let bound = window
                    .user_data()
                    .get::<BoundAppId>()
                    .and_then(|b| b.0.borrow().clone());
                (*id, bound, self.client_pid(window))
            })
            .collect();
        let binds = bindings_to_replay(entries);
        info!("echo reconnected: replaying {} surface binding(s)", binds.len());
        let echo = self.echo_service.read();
        for (app_id, window_id, pid) in binds {
            if let Err(e) = echo.bind_surface_remote(&app_id, window_id, pid) {
                tracing::warn!("replayed bind_surface({}, {}) failed: {}", app_id, window_id, e);
            }
        }
    }

    /// The process behind a window: the Wayland client's, from its socket
    /// credentials, or the X11 window's own `_NET_WM_PID` (the socket there
    /// belongs to Xwayland, which would match no session).
    #[cfg(feature = "echo-ipc")]
    fn client_pid(&self, window: &WindowElement) -> Option<u32> {
        #[cfg(feature = "xwayland")]
        if let Some(surface) = window.0.x11_surface() {
            return surface.pid();
        }
        let surface = window.wl_surface()?;
        let client = self.display_handle.get_client(surface.id()).ok()?;
        let credentials = client.get_credentials(&self.display_handle).ok()?;
        u32::try_from(credentials.pid).ok()
    }

    /// The output a window fills when fullscreened: the one asked for, else
    /// the one it is on, else the first. A minimized window is on none, so
    /// the last step is what a window asking from the dock gets.
    pub fn output_for_fullscreen(&self, window: &WindowElement, requested: Option<&WlOutput>) -> Option<Output> {
        requested
            .and_then(Output::from_resource)
            .or_else(|| self.space.outputs_for_element(window).first().cloned())
            .or_else(|| self.space.outputs().next().cloned())
    }

    /// Put `window` in the output's exclusive fullscreen slot (the render
    /// path draws only it, plus the shell overlay).
    pub fn set_exclusive_fullscreen(&mut self, window: &WindowElement, output: &Output) {
        output.user_data().insert_if_missing(FullscreenSurface::default);
        if let Some(slot) = output.user_data().get::<FullscreenSurface>() {
            slot.set(window.clone());
        }
        self.backend_data.reset_buffers(output);
    }

    /// Take `window` out of any output's fullscreen slot.
    pub fn clear_exclusive_fullscreen(&mut self, window: &WindowElement) {
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        for output in outputs {
            let holds_it = output
                .user_data()
                .get::<FullscreenSurface>()
                .and_then(|slot| slot.get())
                .is_some_and(|held| &held == window);
            if holds_it {
                if let Some(slot) = output.user_data().get::<FullscreenSurface>() {
                    slot.clear();
                }
                self.backend_data.reset_buffers(&output);
            }
        }
    }

    /// The frame rectangle `window` occupies, mapped or not: its visible
    /// geometry in the space, else its recorded place and current size. Not
    /// the bbox, which also holds the invisible resize band around an SSD
    /// frame (and a CSD shadow): maximize saved that as the place to restore
    /// to, so every maximize/unmaximize round trip moved the window by the
    /// band's 8 px.
    pub fn window_frame(&self, window: &WindowElement) -> Option<Rectangle<i32, Logical>> {
        self.space.element_geometry(window).or_else(|| {
            let location = self.workspace_manager.state(window)?.location;
            Some(Rectangle::new(location, window.geometry().size))
        })
    }
}

// ---------------------------------------------------------------------------
// Work area and the overlay's keyboard.
// ---------------------------------------------------------------------------

impl<BackendData: Backend + 'static> EvoState<BackendData> {
    /// The work area of the output `window` is on, or the first output's: the
    /// part of it app windows may use (see `super::work_area`).
    pub fn work_area_for(&self, window: &WindowElement) -> Option<Rectangle<i32, Logical>> {
        let output = self
            .space
            .outputs_for_element(window)
            .first()
            .cloned()
            .or_else(|| self.space.outputs().next().cloned())?;
        self.space.output_geometry(&output).map(work_area::of_output)
    }

    /// Where a dragged app window may go: see `work_area::clamp_move`. The
    /// desktop and its overlay are never clamped.
    pub fn clamp_to_work_area(&self, window: &WindowElement, location: Point<i32, Logical>) -> Point<i32, Logical> {
        if window.is_shell_window() {
            return location;
        }
        let Some(area) = self.work_area_for(window) else {
            return location;
        };
        let frame = self
            .window_frame(window)
            .map_or_else(|| window.geometry().size, |f| f.size);
        work_area::clamp_move(location, frame, area)
    }

    /// `location` moved down and right, if need be, to the work area's
    /// top-left: for a window whose size is not known yet (see
    /// `fit_to_work_area`, which does the rest once it is).
    pub fn pull_into_work_area(&self, window: &WindowElement, location: Point<i32, Logical>) -> Point<i32, Logical> {
        match (window.is_shell_window(), self.work_area_for(window)) {
            (false, Some(area)) => (location.x.max(area.loc.x), location.y.max(area.loc.y)).into(),
            _ => location,
        }
    }

    /// The largest frame a resize of `window` from `edges` may reach: the edge
    /// being dragged stays inside the work area.
    pub fn max_resize_frame(
        &self,
        window: &WindowElement,
        edges: ResizeEdge,
        initial_location: Point<i32, Logical>,
        initial_size: smithay::utils::Size<i32, Logical>,
    ) -> smithay::utils::Size<i32, Logical> {
        let unlimited = (i32::MAX, i32::MAX).into();
        if window.is_shell_window() {
            return unlimited;
        }
        self.work_area_for(window).map_or(unlimited, |area| {
            work_area::max_resize_frame(edges, initial_location, initial_size, area)
        })
    }

    /// Make a floating app window's frame fit the work area: shrink it if it
    /// is bigger (a window restored to a size chosen for the whole screen, or
    /// first mapped at one) and move it so all of it, resize band included,
    /// is inside, with the top inset, the dock's inset and the screen edge
    /// respected. Shrinking is a configure the client answers; this runs again
    /// on its commit and does nothing once the frame fits. Maximized and
    /// fullscreen windows have their own sizes, a window mid-resize is the
    /// grab's, and X11 windows are only moved.
    pub fn fit_to_work_area(&mut self, window: &WindowElement) {
        if window.is_shell_window() || window.placement_pending() {
            return;
        }
        let layout = self
            .workspace_manager
            .state(window)
            .map_or(WindowLayout::Floating, |s| s.layout);
        if layout != WindowLayout::Floating || self.workspace_manager.is_minimized(window) {
            return;
        }
        let Some(area) = self.work_area_for(window) else {
            return;
        };
        let Some(frame) = self.window_frame(window) else {
            return;
        };
        if frame.size.w <= 0 || frame.size.h <= window.decoration_height() {
            // Not sized yet.
            return;
        }
        if window.still_settling() {
            return;
        }
        let toplevel = window.0.toplevel().cloned();
        if let Some(toplevel) = &toplevel {
            // Mid-resize is the grab's. A configure still in flight means the
            // frame on hand is about to change (an unmaximize's first commit
            // still has the maximized size), and fitting that one would move
            // the window for a size it is leaving: wait for the commit that
            // acknowledges it.
            if toplevel_is_resizing(toplevel) || configure_in_flight(toplevel) {
                return;
            }
        }
        let min = toplevel
            .as_ref()
            .map(|t| {
                smithay::wayland::compositor::with_states(t.wl_surface(), |states| {
                    let mut cached = states.cached_state.get::<smithay::wayland::shell::xdg::SurfaceCachedState>();
                    let min = cached.current().min_size;
                    let header = window.decoration_height();
                    smithay::utils::Size::<i32, Logical>::from((min.w, if min.h > 0 { min.h + header } else { 0 }))
                })
            })
            .unwrap_or_default();
        let (location, size) = work_area::fit_frame(frame.loc, frame.size, min, area);
        if size != frame.size {
            if let Some(toplevel) = toplevel {
                // Ask once per size: a client that keeps its size (a minimum
                // we could not read, a bug) must not be asked on every commit.
                let asked = window.user_data().get::<FitAsked>().and_then(|a| a.0.get());
                if asked != Some((size.w, size.h)) {
                    window.user_data().insert_if_missing(FitAsked::default);
                    if let Some(mark) = window.user_data().get::<FitAsked>() {
                        mark.0.set(Some((size.w, size.h)));
                    }
                    let client = (size.w, (size.h - window.decoration_height()).max(1)).into();
                    self.begin_geometry_transition(window, Some(size));
                    window.begin_settling();
                    toplevel.with_pending_state(|state| state.size = Some(client));
                    if toplevel.is_initial_configure_sent() {
                        toplevel.send_configure();
                    }
                }
            }
        }
        // Moved to where the frame will be inside, whether or not the client
        // has caught up with its new size: the top-left is what must be
        // below the bar and clear of the screen's edge.
        let target = if size != frame.size { work_area::clamp_move(frame.loc, size, area) } else { location };
        if target != frame.loc {
            if size == frame.size {
                // Only the place changes: the live window slides there.
                self.begin_geometry_transition(window, Some(size));
            }
            self.place_window(window, target, false);
        }
    }

    /// Bloom reported new edges: fit the windows already open to them. A
    /// maximized window is resized to the new area; a floating one is shrunk
    /// and moved until it fits (`fit_to_work_area`).
    pub fn refit_to_work_area(&mut self) {
        let windows: Vec<WindowElement> = self
            .workspace_manager
            .all_windows()
            .filter(|w| w.alive() && !w.is_shell_window())
            .cloned()
            .collect();
        for window in windows {
            let layout = self
                .workspace_manager
                .state(&window)
                .map_or(WindowLayout::Floating, |s| s.layout);
            match layout {
                WindowLayout::Fullscreen => {}
                WindowLayout::Maximized => {
                    let Some(target) = self.maximize_target(&window) else {
                        continue;
                    };
                    self.begin_geometry_transition(&window, Some(target.size));
                    match window.0.underlying_surface() {
                        smithay::desktop::WindowSurface::Wayland(toplevel) => {
                            let size = super::xdg::maximized_client_size(&window, target.size);
                            toplevel.with_pending_state(|state| state.size = Some(size));
                            self.place_window(&window, target.loc, false);
                            if toplevel.is_initial_configure_sent() {
                                toplevel.send_configure();
                            }
                        }
                        #[cfg(feature = "xwayland")]
                        smithay::desktop::WindowSurface::X11(surface) => {
                            let _ = surface.configure(target);
                            self.place_window(&window, target.loc, false);
                        }
                    }
                }
                _ => self.fit_to_work_area(&window),
            }
        }
    }

    /// Bloom's `SetWorkArea`. Returns whether the insets changed.
    pub fn set_work_area(&mut self, insets: work_area::Insets) -> bool {
        let changed = work_area::set_insets(insets);
        if changed {
            info!("work area insets: {:?}", insets);
            self.refit_to_work_area();
        }
        changed
    }

    /// Bloom's `SetOverlayFocus`: the one way the overlay takes the keyboard,
    /// and the one way it gives it back. It never takes it on its own (a
    /// click on it, a new overlay window, activation: `focus_window` refuses
    /// it all), because a toast must not steal what an app is typing.
    ///
    /// Giving it back hands the keyboard to the next app down the stack, else
    /// the shell, like a window that closed (`refocus_after_loss`).
    pub fn set_overlay_keyboard(&mut self, wanted: bool) {
        let Some(overlay) = self.space.elements().find(|w| w.is_overlay()).cloned() else {
            if wanted {
                tracing::warn!("SetOverlayFocus: no overlay window to focus");
            }
            return;
        };
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        if wanted {
            if !overlay_mod::keyboard_held() {
                keyboard.set_focus(self, Some(overlay.into()), SERIAL_COUNTER.next_serial());
            }
        } else if overlay_mod::keyboard_held() {
            self.refocus_after_loss(&overlay);
        }
    }

    /// The seat's keyboard focus moved to or from the overlay. Called from
    /// `focus_changed`, so the flag follows what really happened, whoever
    /// moved the focus (an Alt+Tab, say). Announces a change as
    /// `OverlayFocusChanged`, so Bloom can close the panels that were using
    /// the keyboard.
    pub fn note_overlay_keyboard(&mut self, held: bool) {
        if !overlay_mod::note_keyboard(held) {
            return;
        }
        #[cfg(feature = "echo-ipc")]
        {
            let _ = self.echo_service.read().emit_overlay_focus_changed(held);
        }
    }
}

/// A window whose size has just been asked to change, with the frame size it
/// had then. Gecko answers a configure by acknowledging it at once, but its
/// toplevel's geometry only follows when it next paints, so for a moment a
/// commit shows the OLD size under the new state. Fitting that would move the
/// window for a size it is leaving (an unmaximize's restored place lost to the
/// maximized frame), so fitting waits until the size changes, or this long.
#[derive(Default)]
struct Settling(RefCell<Option<(smithay::utils::Size<i32, Logical>, std::time::Instant)>>);

const SETTLE_LIMIT: std::time::Duration = std::time::Duration::from_secs(2);

impl WindowElement {
    /// Note that this window's size was just asked to change.
    pub fn begin_settling(&self) {
        self.user_data().insert_if_missing(Settling::default);
        if let Some(mark) = self.user_data().get::<Settling>() {
            *mark.0.borrow_mut() = Some((self.geometry().size, std::time::Instant::now()));
        }
    }

    /// Whether the size asked for has not shown up yet. Clears itself once
    /// the size changes or `SETTLE_LIMIT` passes.
    fn still_settling(&self) -> bool {
        let Some(mark) = self.user_data().get::<Settling>() else {
            return false;
        };
        let mut slot = mark.0.borrow_mut();
        match *slot {
            Some((stale, since)) if self.geometry().size == stale && since.elapsed() < SETTLE_LIMIT => true,
            Some(_) => {
                *slot = None;
                false
            }
            None => false,
        }
    }
}

/// The last size `fit_to_work_area` asked a window for, so it asks once.
#[derive(Default)]
struct FitAsked(Cell<Option<(i32, i32)>>);

/// Whether a configure has been sent that the client has not acknowledged, or
/// one is queued: the client's size is about to change.
fn configure_in_flight(toplevel: &ToplevelSurface) -> bool {
    smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
            .is_some_and(|data| {
                let data = data.lock().unwrap();
                !data.pending_configures().is_empty() || data.has_pending_changes()
            })
    })
}

/// Whether an interactive resize of this toplevel is in progress.
fn toplevel_is_resizing(toplevel: &ToplevelSurface) -> bool {
    smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<RefCell<super::SurfaceData>>()
            .is_some_and(|data| !matches!(data.borrow().resize_state, super::ResizeState::NotResizing))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u32, role: StackRole) -> StackEntry<u32> {
        StackEntry { id, role, minimized: false }
    }

    #[test]
    fn focus_falls_to_the_next_app_down_the_stack() {
        // Back to front: shell, app 1, app 2, app 3 (closing).
        let stack = [
            entry(0, StackRole::Shell),
            entry(1, StackRole::App),
            entry(2, StackRole::App),
            entry(3, StackRole::App),
        ];
        assert_eq!(focus_fallback(&stack, &3), Some(2));
        assert_eq!(focus_fallback(&stack, &2), Some(3), "topmost other app, not the one beneath");
    }

    #[test]
    fn focus_skips_minimized_apps_and_goes_to_the_shell_when_none_is_left() {
        let mut stack = vec![
            entry(0, StackRole::Shell),
            entry(1, StackRole::App),
            entry(2, StackRole::App),
        ];
        stack[1].minimized = true;
        assert_eq!(focus_fallback(&stack, &2), Some(0), "app 1 is minimized, so the shell");
        assert_eq!(focus_fallback(&stack[..1], &9), Some(0));
    }

    #[test]
    fn focus_never_falls_to_the_overlay_or_to_nothing_useful() {
        let stack = [entry(0, StackRole::Shell), entry(5, StackRole::Ignored)];
        assert_eq!(focus_fallback(&stack, &1), Some(0));
        assert_eq!(focus_fallback(&[entry(5, StackRole::Ignored)], &1), None);
        assert_eq!(focus_fallback::<u32>(&[], &1), None);
    }

    #[test]
    fn most_recent_first_and_minimized_windows_stay_in_it() {
        let mut history = vec![];
        for id in [1, 2, 3, 2] {
            mru_touch(&mut history, id);
        }
        assert_eq!(history, [2, 3, 1]);
        // 9 was never focused; 3 is gone.
        assert_eq!(cycle_order(&history, &[1, 2, 9]), [2, 1, 9]);
    }

    #[test]
    fn cycling_wraps_both_ways() {
        assert_eq!(cycle_step(3, 0, false), 1);
        assert_eq!(cycle_step(3, 2, false), 0);
        assert_eq!(cycle_step(3, 0, true), 2);
        assert_eq!(cycle_step(3, 1, true), 0);
    }

    #[test]
    fn only_real_apps_are_bound() {
        assert!(is_bindable_app_id("com.pyxis.notes"));
        assert!(!is_bindable_app_id(""));
        assert!(!is_bindable_app_id("unknown"));
        assert!(!is_bindable_app_id("vela-default"));
    }

    #[test]
    fn replay_covers_bound_windows_only() {
        let binds = bindings_to_replay([
            (9, Some("com.pyxis.notes".to_string()), Some(300)),
            (2, None, Some(10)),                                  // shell or not yet known
            (5, Some("vela-default".to_string()), Some(11)),      // never bindable
            (3, Some("com.pyxis.files".to_string()), None),
        ]);
        assert_eq!(
            binds,
            vec![
                ("com.pyxis.files".to_string(), 3, None),
                ("com.pyxis.notes".to_string(), 9, Some(300)),
            ]
        );
    }
}
