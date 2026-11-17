use std::cell::RefCell;

use smithay::{
    desktop::{space::SpaceElement, WindowSurface},
    input::{
        pointer::{
            AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
            GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
            GestureSwipeUpdateEvent, GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab,
            PointerInnerHandle, RelativeMotionEvent,
        },
        touch::{GrabStartData as TouchGrabStartData, TouchGrab},
    },
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{IsAlive, Logical, Point, Serial, Size},
    wayland::{compositor::with_states, shell::xdg::SurfaceCachedState},
};
#[cfg(feature = "xwayland")]
use smithay::{utils::Rectangle, xwayland::xwm::ResizeEdge as X11ResizeEdge};

use super::{ssd::MIN_FRAME_SIZE, SurfaceData, WindowElement};
use crate::{
    focus::PointerFocusTarget,
    state::{EvoState, Backend},
};

pub struct PointerMoveSurfaceGrab<BackendData: Backend + 'static> {
    pub start_data: PointerGrabStartData<EvoState<BackendData>>,
    pub window: WindowElement,
    pub initial_window_location: Point<i32, Logical>,
    pub pending_unmaximize: bool,
}

impl<BackendData: Backend> PointerGrab<EvoState<BackendData>> for PointerMoveSurfaceGrab<BackendData> {
    fn motion(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        _focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // While the grab is active, no client has pointer focus
        handle.motion(data, None, event);

        move_grab_motion(
            data,
            &self.window,
            &mut self.initial_window_location,
            self.start_data.location,
            event.location,
            &mut self.pending_unmaximize,
        );
    }

    fn relative_motion(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            // No more buttons are pressed, release the grab.
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        details: AxisFrame,
    ) {
        handle.axis(data, details)
    }

    fn frame(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
    ) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &PointerGrabStartData<EvoState<BackendData>> {
        &self.start_data
    }

    // The move is over: say where the window ended up. During the drag
    // nothing is announced, because motion arrives faster than anyone can use.
    fn unset(&mut self, data: &mut EvoState<BackendData>) {
        self.window.set_lifted(false);
        data.window_moved(&self.window);
    }
}

pub struct TouchMoveSurfaceGrab<BackendData: Backend + 'static> {
    pub start_data: TouchGrabStartData<EvoState<BackendData>>,
    pub window: WindowElement,
    pub initial_window_location: Point<i32, Logical>,
    pub pending_unmaximize: bool,
}

impl<BackendData: Backend> TouchGrab<EvoState<BackendData>> for TouchMoveSurfaceGrab<BackendData> {
    fn down(
        &mut self,
        _data: &mut EvoState<BackendData>,
        _handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        _focus: Option<(
            <EvoState<BackendData> as smithay::input::SeatHandler>::TouchFocus,
            Point<f64, Logical>,
        )>,
        _event: &smithay::input::touch::DownEvent,
        _seq: Serial,
    ) {
    }

    fn up(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        event: &smithay::input::touch::UpEvent,
        seq: Serial,
    ) {
        if event.slot != self.start_data.slot {
            return;
        }

        handle.up(data, event, seq);
        handle.unset_grab(self, data);
    }

    fn motion(
        &mut self,
        data: &mut EvoState<BackendData>,
        _handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        _focus: Option<(
            <EvoState<BackendData> as smithay::input::SeatHandler>::TouchFocus,
            Point<f64, Logical>,
        )>,
        event: &smithay::input::touch::MotionEvent,
        _seq: Serial,
    ) {
        if event.slot != self.start_data.slot {
            return;
        }

        move_grab_motion(
            data,
            &self.window,
            &mut self.initial_window_location,
            self.start_data.location,
            event.location,
            &mut self.pending_unmaximize,
        );
    }

    fn frame(
        &mut self,
        _data: &mut EvoState<BackendData>,
        _handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        _seq: Serial,
    ) {
    }

    fn cancel(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        seq: Serial,
    ) {
        handle.cancel(data, seq);
        handle.unset_grab(self, data);
    }

    fn shape(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        event: &smithay::input::touch::ShapeEvent,
        seq: Serial,
    ) {
        handle.shape(data, event, seq);
    }

    fn orientation(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        event: &smithay::input::touch::OrientationEvent,
        seq: Serial,
    ) {
        handle.orientation(data, event, seq);
    }

    fn start_data(&self) -> &smithay::input::touch::GrabStartData<EvoState<BackendData>> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut EvoState<BackendData>) {
        self.window.set_lifted(false);
        data.window_moved(&self.window);
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct ResizeEdge: u32 {
        const NONE = 0;
        const TOP = 1;
        const BOTTOM = 2;
        const LEFT = 4;
        const TOP_LEFT = 5;
        const BOTTOM_LEFT = 6;
        const RIGHT = 8;
        const TOP_RIGHT = 9;
        const BOTTOM_RIGHT = 10;
    }
}

impl From<xdg_toplevel::ResizeEdge> for ResizeEdge {
    #[inline]
    fn from(x: xdg_toplevel::ResizeEdge) -> Self {
        Self::from_bits(x as u32).unwrap_or(Self::NONE)
    }
}

impl From<ResizeEdge> for xdg_toplevel::ResizeEdge {
    #[inline]
    fn from(x: ResizeEdge) -> Self {
        Self::try_from(x.bits()).unwrap_or(Self::None)
    }
}

#[cfg(feature = "xwayland")]
impl From<X11ResizeEdge> for ResizeEdge {
    #[inline]
    fn from(edge: X11ResizeEdge) -> Self {
        match edge {
            X11ResizeEdge::Bottom => ResizeEdge::BOTTOM,
            X11ResizeEdge::BottomLeft => ResizeEdge::BOTTOM_LEFT,
            X11ResizeEdge::BottomRight => ResizeEdge::BOTTOM_RIGHT,
            X11ResizeEdge::Left => ResizeEdge::LEFT,
            X11ResizeEdge::Right => ResizeEdge::RIGHT,
            X11ResizeEdge::Top => ResizeEdge::TOP,
            X11ResizeEdge::TopLeft => ResizeEdge::TOP_LEFT,
            X11ResizeEdge::TopRight => ResizeEdge::TOP_RIGHT,
        }
    }
}

/// The size a configure carries for a frame of size `frame`: the client's
/// own, which is shorter by the SSD header bar when Evo decorates it.
///
/// Resize grabs track the frame, because `WindowElement::geometry` (and so
/// `initial_window_size`) includes the header. Sending that frame size
/// straight to the client grew every SSD window by the header's height the
/// moment a resize began. No client-initiated resize ever had a header to
/// get wrong, so nothing had hit this until the SSD band started resizes of
/// its own.
pub(crate) fn client_size(window: &WindowElement, frame: Size<i32, Logical>) -> Size<i32, Logical> {
    (frame.w.max(1), (frame.h - window.decoration_height()).max(1)).into()
}

fn move_grab_motion<BackendData: Backend>(
    data: &mut EvoState<BackendData>,
    window: &WindowElement,
    initial_window_location: &mut Point<i32, Logical>,
    start_location: Point<f64, Logical>,
    current_location: Point<f64, Logical>,
    pending_unmaximize: &mut bool,
) {
    let delta = current_location - start_location;
    let moved_past_threshold = (delta.x * delta.x + delta.y * delta.y) > 4.0;
    if *pending_unmaximize {
        if !moved_past_threshold {
            return;
        }
        *pending_unmaximize = false;
        if let Some(new_initial) = data.unmaximize_on_drag(window, start_location) {
            *initial_window_location = new_initial;
        }
    } else if moved_past_threshold
        && data
            .workspace_manager
            .state(window)
            .is_some_and(|s| s.layout == super::WindowLayout::Tiled)
    {
        if let Some(tile) = window.user_data().get::<super::element::TileState>() {
            tile.take();
        }
        data.set_window_layout(window, super::WindowLayout::Floating);
    }

    // The window is in the hand once it moves (a press that goes nowhere
    // doesn't lift it): its shadow grows, over the `lift` token.
    window.set_lifted(true);

    let new_location = initial_window_location.to_f64() + delta;
    let new_location = data.clamp_to_work_area(window, new_location.to_i32_round());
    data.place_window(window, new_location, true);
}

#[allow(clippy::too_many_arguments)]
fn resize_grab_motion<BackendData: Backend>(
    data: &mut EvoState<BackendData>,
    window: &WindowElement,
    edges: ResizeEdge,
    initial_location: Point<i32, Logical>,
    initial_size: Size<i32, Logical>,
    last_window_size: &mut Size<i32, Logical>,
    start_location: Point<f64, Logical>,
    current_location: Point<f64, Logical>,
) {
    let (mut dx, mut dy) = (current_location - start_location).into();
    let mut new_window_width = initial_size.w;
    let mut new_window_height = initial_size.h;

    let left_right = ResizeEdge::LEFT | ResizeEdge::RIGHT;
    let top_bottom = ResizeEdge::TOP | ResizeEdge::BOTTOM;

    // Round, don't truncate: the pointer delta is fractional (libinput
    // scales the tablet's axis into output pixels), and `as i32` on
    // 752 - 80.007 gave 671, so a -80 drag shrank a window by 81.
    if edges.intersects(left_right) {
        if edges.intersects(ResizeEdge::LEFT) {
            dx = -dx;
        }
        new_window_width = (initial_size.w as f64 + dx).round() as i32;
    }

    if edges.intersects(top_bottom) {
        if edges.intersects(ResizeEdge::TOP) {
            dy = -dy;
        }
        new_window_height = (initial_size.h as f64 + dy).round() as i32;
    }

    let (min_size, max_size) = match window.0.underlying_surface() {
        WindowSurface::Wayland(_) => window
            .wl_surface()
            .map(|surface| {
                with_states(&surface, |states| {
                    let mut guard = states.cached_state.get::<SurfaceCachedState>();
                    let data = guard.current();
                    (data.min_size, data.max_size)
                })
            })
            .unwrap_or_default(),
        #[cfg(feature = "xwayland")]
        WindowSurface::X11(x11) => (
            x11.min_size().unwrap_or_default(),
            x11.max_size().unwrap_or_default(),
        ),
    };

    // These sizes are the frame's, header bar included (see
    // `client_size`). So the client's own min/max are raised by the
    // header to match, and an SSD frame keeps room for its buttons.
    let header = window.decoration_height();
    let frame_min = if header > 0 { MIN_FRAME_SIZE } else { (1, 1) };
    let min_width = min_size.w.max(frame_min.0);
    let min_height = (min_size.h + header).max(frame_min.1);
    let max_width = if max_size.w == 0 { i32::MAX } else { max_size.w };
    let max_height = if max_size.h == 0 {
        i32::MAX
    } else {
        max_size.h + header
    };

    // Nor may a drag push an edge of the frame out of the work area (under
    // the shell's top bar or dock, say).
    let limit = data.max_resize_frame(window, edges, initial_location, initial_size);
    let max_width = max_width.min(limit.w);
    let max_height = max_height.min(limit.h);

    new_window_width = new_window_width.max(min_width).min(max_width.max(min_width));
    new_window_height = new_window_height.max(min_height).min(max_height.max(min_height));

    *last_window_size = (new_window_width, new_window_height).into();

    match window.0.underlying_surface() {
        WindowSurface::Wayland(xdg) => {
            xdg.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Resizing);
                state.size = Some(client_size(window, *last_window_size));
            });
            xdg.send_pending_configure();
        }
        #[cfg(feature = "xwayland")]
        WindowSurface::X11(x11) => {
            let Some(location) = data.space.element_location(window) else {
                tracing::warn!("Resize grab motion: X11 window has no location in space");
                return;
            };
            let target = Rectangle::new(location, client_size(window, *last_window_size));
            if let Err(err) = x11.configure(target) {
                tracing::warn!("Resize grab motion: failed to configure X11 window: {:?}", err);
            }
        }
    }
}

fn adjust_top_left_location<BackendData: Backend>(
    data: &mut EvoState<BackendData>,
    window: &WindowElement,
    edges: ResizeEdge,
    initial_location: Point<i32, Logical>,
    initial_size: Size<i32, Logical>,
) -> Option<Point<i32, Logical>> {
    let mut location = data.space.element_location(window).or_else(|| {
        tracing::warn!("Resize grab finish: window has no location in space");
        None
    })?;
    if edges.intersects(ResizeEdge::TOP_LEFT) {
        let geometry = window.geometry();
        if edges.intersects(ResizeEdge::LEFT) {
            location.x = initial_location.x + (initial_size.w - geometry.size.w);
        }
        if edges.intersects(ResizeEdge::TOP) {
            location.y = initial_location.y + (initial_size.h - geometry.size.h);
        }
        data.place_window(window, location, true);
        data.window_moved(window);
    }
    Some(location)
}

fn resize_grab_finish<BackendData: Backend>(
    data: &mut EvoState<BackendData>,
    window: &WindowElement,
    edges: ResizeEdge,
    initial_location: Point<i32, Logical>,
    initial_size: Size<i32, Logical>,
    last_window_size: Size<i32, Logical>,
    serial: Serial,
) {
    if !window.alive() {
        return;
    }

    match window.0.underlying_surface() {
        WindowSurface::Wayland(xdg) => {
            xdg.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Resizing);
                state.size = Some(client_size(window, last_window_size));
            });
            xdg.send_pending_configure();
            let _ = adjust_top_left_location(data, window, edges, initial_location, initial_size);

            let Some(surface) = window.wl_surface() else {
                tracing::warn!("Wayland resize grab finish: window has no wl_surface");
                return;
            };
            with_states(&surface, |states| {
                let Some(surface_data_cell) = states.data_map.get::<RefCell<SurfaceData>>() else {
                    tracing::warn!("Wayland resize grab finish: SurfaceData missing on surface");
                    return;
                };
                let mut surface_data = surface_data_cell.borrow_mut();
                if let ResizeState::Resizing(resize_data) = surface_data.resize_state {
                    surface_data.resize_state = ResizeState::WaitingForFinalAck(resize_data, serial);
                } else {
                    tracing::warn!(
                        "Wayland resize grab finish: unexpected resize state {:?}",
                        surface_data.resize_state
                    );
                    surface_data.resize_state = ResizeState::NotResizing;
                }
            });
        }
        #[cfg(feature = "xwayland")]
        WindowSurface::X11(x11) => {
            let Some(location) =
                adjust_top_left_location(data, window, edges, initial_location, initial_size)
            else {
                return;
            };
            let target = Rectangle::new(location, client_size(window, last_window_size));
            if let Err(err) = x11.configure(target) {
                tracing::warn!("X11 resize grab finish: failed to configure window: {:?}", err);
            }

            let Some(surface) = window.wl_surface() else {
                tracing::warn!("X11 resize grab finish: window has no wl_surface");
                return;
            };
            with_states(&surface, |states| {
                let Some(surface_data_cell) = states.data_map.get::<RefCell<SurfaceData>>() else {
                    tracing::warn!("X11 resize grab finish: SurfaceData missing on surface");
                    return;
                };
                let mut surface_data = surface_data_cell.borrow_mut();
                if let ResizeState::Resizing(resize_data) = surface_data.resize_state {
                    surface_data.resize_state = ResizeState::WaitingForCommit(resize_data);
                } else {
                    tracing::warn!(
                        "X11 resize grab finish: unexpected resize state {:?}",
                        surface_data.resize_state
                    );
                    surface_data.resize_state = ResizeState::NotResizing;
                }
            });
        }
    }
}

/// Reset resize state when a resize grab is cancelled externally (lock screen
/// `release_grabs`, pointer focus loss, touch cancel, dead window) rather than
/// finished normally via `button`/`up`.
pub(crate) fn cancel_resize_on_unset<BackendData: Backend>(
    data: &mut EvoState<BackendData>,
    window: &WindowElement,
) {
    let mut cancelled = false;
    if let Some(surface) = window.wl_surface() {
        with_states(&surface, |states| {
            if let Some(surface_data_cell) = states.data_map.get::<RefCell<SurfaceData>>() {
                let mut surface_data = surface_data_cell.borrow_mut();
                if matches!(surface_data.resize_state, ResizeState::Resizing(_)) {
                    surface_data.resize_state = ResizeState::NotResizing;
                    cancelled = true;
                }
            }
        });
    }
    if cancelled && window.alive() {
        if let WindowSurface::Wayland(xdg) = window.0.underlying_surface() {
            xdg.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Resizing);
            });
            xdg.send_pending_configure();
        }
        data.window_moved(window);
    }
}

/// Information about the resize operation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ResizeData {
    /// The edges the surface is being resized with.
    pub edges: ResizeEdge,
    /// The initial window location.
    pub initial_window_location: Point<i32, Logical>,
    /// The initial window size (geometry width and height).
    pub initial_window_size: Size<i32, Logical>,
}

/// State of the resize operation.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum ResizeState {
    /// The surface is not being resized.
    #[default]
    NotResizing,
    /// The surface is currently being resized.
    Resizing(ResizeData),
    /// The resize has finished, and the surface needs to ack the final configure.
    WaitingForFinalAck(ResizeData, Serial),
    /// The resize has finished, and the surface needs to commit its final state.
    WaitingForCommit(ResizeData),
}

pub struct PointerResizeSurfaceGrab<BackendData: Backend + 'static> {
    pub start_data: PointerGrabStartData<EvoState<BackendData>>,
    pub window: WindowElement,
    pub edges: ResizeEdge,
    pub initial_window_location: Point<i32, Logical>,
    pub initial_window_size: Size<i32, Logical>,
    pub last_window_size: Size<i32, Logical>,
}

impl<BackendData: Backend> PointerGrab<EvoState<BackendData>> for PointerResizeSurfaceGrab<BackendData> {
    fn motion(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        _focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // While the grab is active, no client has pointer focus
        handle.motion(data, None, event);

        // It is impossible to get `min_size` and `max_size` of dead toplevel, so we return early.
        if !self.window.alive() {
            handle.unset_grab(self, data, event.serial, event.time, true);
            return;
        }

        resize_grab_motion(
            data,
            &self.window,
            self.edges,
            self.initial_window_location,
            self.initial_window_size,
            &mut self.last_window_size,
            self.start_data.location,
            event.location,
        );
    }

    fn relative_motion(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        focus: Option<(PointerFocusTarget, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            resize_grab_finish(
                data,
                &self.window,
                self.edges,
                self.initial_window_location,
                self.initial_window_size,
                self.last_window_size,
                event.serial,
            );
            // Release after transitioning `ResizeState` to WaitingForFinalAck /
            // WaitingForCommit so `unset` only resets aborted `Resizing` grabs.
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        details: AxisFrame,
    ) {
        handle.axis(data, details)
    }

    fn frame(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
    ) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut PointerInnerHandle<'_, EvoState<BackendData>>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &PointerGrabStartData<EvoState<BackendData>> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut EvoState<BackendData>) {
        cancel_resize_on_unset(data, &self.window);
    }
}

pub struct TouchResizeSurfaceGrab<BackendData: Backend + 'static> {
    pub start_data: TouchGrabStartData<EvoState<BackendData>>,
    pub window: WindowElement,
    pub edges: ResizeEdge,
    pub initial_window_location: Point<i32, Logical>,
    pub initial_window_size: Size<i32, Logical>,
    pub last_window_size: Size<i32, Logical>,
}

impl<BackendData: Backend> TouchGrab<EvoState<BackendData>> for TouchResizeSurfaceGrab<BackendData> {
    fn down(
        &mut self,
        _data: &mut EvoState<BackendData>,
        _handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        _focus: Option<(
            <EvoState<BackendData> as smithay::input::SeatHandler>::TouchFocus,
            Point<f64, Logical>,
        )>,
        _event: &smithay::input::touch::DownEvent,
        _seq: Serial,
    ) {
    }

    fn up(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        event: &smithay::input::touch::UpEvent,
        _seq: Serial,
    ) {
        if event.slot != self.start_data.slot {
            return;
        }
        resize_grab_finish(
            data,
            &self.window,
            self.edges,
            self.initial_window_location,
            self.initial_window_size,
            self.last_window_size,
            event.serial,
        );
        handle.unset_grab(self, data);
    }

    fn motion(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        _focus: Option<(
            <EvoState<BackendData> as smithay::input::SeatHandler>::TouchFocus,
            Point<f64, Logical>,
        )>,
        event: &smithay::input::touch::MotionEvent,
        _seq: Serial,
    ) {
        if event.slot != self.start_data.slot {
            return;
        }

        // It is impossible to get `min_size` and `max_size` of dead toplevel, so we return early.
        if !self.window.alive() {
            handle.unset_grab(self, data);
            return;
        }

        resize_grab_motion(
            data,
            &self.window,
            self.edges,
            self.initial_window_location,
            self.initial_window_size,
            &mut self.last_window_size,
            self.start_data.location,
            event.location,
        );
    }

    fn frame(
        &mut self,
        _data: &mut EvoState<BackendData>,
        _handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        _seq: Serial,
    ) {
    }

    fn cancel(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        seq: Serial,
    ) {
        handle.cancel(data, seq);
        handle.unset_grab(self, data);
    }

    fn shape(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        event: &smithay::input::touch::ShapeEvent,
        seq: Serial,
    ) {
        handle.shape(data, event, seq);
    }

    fn orientation(
        &mut self,
        data: &mut EvoState<BackendData>,
        handle: &mut smithay::input::touch::TouchInnerHandle<'_, EvoState<BackendData>>,
        event: &smithay::input::touch::OrientationEvent,
        seq: Serial,
    ) {
        handle.orientation(data, event, seq);
    }

    fn start_data(&self) -> &smithay::input::touch::GrabStartData<EvoState<BackendData>> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut EvoState<BackendData>) {
        cancel_resize_on_unset(data, &self.window);
    }
}
