use std::{borrow::Cow, cell::RefCell, time::Duration};

use smithay::{
    backend::renderer::{
        element::{surface::WaylandSurfaceRenderElement, AsRenderElements},
        ImportAll, ImportMem, Renderer, Texture,
    },
    desktop::{
        space::SpaceElement, utils::OutputPresentationFeedback, Window, WindowSurface, WindowSurfaceType,
    },
    backend::input::ButtonState,
    input::{
        pointer::{
            AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
            GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
            GestureSwipeUpdateEvent, MotionEvent, PointerTarget, RelativeMotionEvent,
        },
        touch::TouchTarget,
        Seat,
    },
    output::Output,
    reexports::{
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    render_elements,
    utils::{
        user_data::UserDataMap, IsAlive, Logical, Physical, Point, Rectangle, Scale, Serial, Size,
    },
    wayland::{
        compositor::{with_states, SurfaceData as WlSurfaceData},
        dmabuf::DmabufFeedback,
        seat::WaylandFocus,
        shell::xdg::XdgToplevelSurfaceData,
    },
};

use super::ssd::{DecorationRenderElement, HEADER_BAR_HEIGHT};
use super::xdg::SHELL_APP_ID;
use crate::{focus::PointerFocusTarget, state::Backend, EvoState};

/// Geometry a window had before it was maximized (or minimized), stored in
/// its user_data so the un-maximize/restore path can put it back. Shared
/// between the XDG and X11 shell handlers and the SSD title-bar buttons so
/// both window kinds and both trigger paths (client-requested vs.
/// title-bar-clicked) agree on the same saved geometry.
#[derive(Debug, Default)]
pub(crate) struct OldGeometry(RefCell<Option<Rectangle<i32, Logical>>>);
impl OldGeometry {
    pub fn save(&self, geo: Rectangle<i32, Logical>) {
        *self.0.borrow_mut() = Some(geo);
    }

    pub fn restore(&self) -> Option<Rectangle<i32, Logical>> {
        self.0.borrow_mut().take()
    }
}

#[derive(Debug, Clone, PartialEq)]
#[derive(Eq, Hash)]
pub struct WindowElement(pub Window);

impl WindowElement {
    /// The SSD header bar's rect, in the coordinate space that smithay hands
    /// us for both rendering and hit-testing.
    ///
    /// That space is anchored at the element's *render origin*, which
    /// `Space` defines as `location - geometry().loc` — the top-left of the
    /// client's own buffer, i.e. the outer corner of its CSD **shadow
    /// margin**, not the top-left of the window you can actually see. GTK
    /// clients routinely set a non-zero window geometry for exactly that
    /// reason (a plain GTK dialog reports `set_window_geometry(22, 22, …)`).
    ///
    /// Everything SSD-related therefore has to be offset by `geometry().loc`,
    /// or the header bar renders up-and-left of the window it belongs to and
    /// its hit-testing lands on the shadow instead of the buttons. Returns
    /// `None` when this window isn't server-decorated.
    fn ssd_header_rect(&self) -> Option<Rectangle<i32, Logical>> {
        if !self.decoration_state().is_ssd {
            return None;
        }
        let geo = SpaceElement::geometry(&self.0);
        Some(Rectangle::new(
            geo.loc,
            Size::from((geo.size.w, HEADER_BAR_HEIGHT)),
        ))
    }

    /// The window's human-readable title for the SSD title bar: the
    /// xdg_toplevel title (X11 WM title), falling back to the app_id/class when
    /// the client hasn't set a title (so e.g. a web app that never set
    /// document.title still shows its id rather than a blank bar). Empty only
    /// when neither is set.
    fn window_title(&self) -> String {
        match self.0.underlying_surface() {
            WindowSurface::Wayland(toplevel) => with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| {
                        d.lock().ok().and_then(|g| {
                            g.title
                                .clone()
                                .filter(|t| !t.trim().is_empty())
                                .or_else(|| g.app_id.clone())
                        })
                    })
                    .unwrap_or_default()
            }),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(s) => {
                let title = s.title();
                if title.trim().is_empty() {
                    s.class()
                } else {
                    title
                }
            }
        }
    }

    /// Whether this window is the desktop shell (Bloom), identified by
    /// app_id / X11 class. Used to pin it beneath app windows in the z-order.
    pub(crate) fn is_shell(&self) -> bool {
        match self.0.underlying_surface() {
            WindowSurface::Wayland(toplevel) => with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok().and_then(|g| g.app_id.clone()))
                    .as_deref()
                    == Some(SHELL_APP_ID)
            }),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(s) => s.class() == SHELL_APP_ID,
        }
    }

    pub fn surface_under(
        &self,
        location: Point<f64, Logical>,
        window_type: WindowSurfaceType,
    ) -> Option<(PointerFocusTarget, Point<i32, Logical>)> {
        // Read `is_ssd` out and let the borrow end with this statement --
        // holding a `RefMut` across the `ssd_header_rect()` call below (which
        // re-borrows the same RefCell) is an instant `BorrowMutError` panic.
        let is_ssd = self.decoration_state().is_ssd;
        if let Some(header) = self.ssd_header_rect() {
            if header.to_f64().contains(location) {
                // Report the header's own top-left as the focus location, not
                // `Point::default()`. Smithay makes pointer events
                // focus-relative via `event.location - loc`, and
                // `HeaderBar::clicked`/`redraw` interpret `pointer_loc` as
                // header-relative (they compare `loc.x` against
                // `self.width`). Handing back the render origin here shifted
                // every button hit-test by the shadow margin.
                return Some((PointerFocusTarget::SSD(SSD(self.clone())), header.loc));
            }
        }
        let offset = if is_ssd {
            Point::from((0, HEADER_BAR_HEIGHT))
        } else {
            Point::default()
        };

        let surface_under = self.0.surface_under(location - offset.to_f64(), window_type);
        let (under, loc) = match self.0.underlying_surface() {
            WindowSurface::Wayland(_) => {
                surface_under.map(|(surface, loc)| (PointerFocusTarget::WlSurface(surface), loc))
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(s) => {
                surface_under.map(|(_, loc)| (PointerFocusTarget::X11Surface(s.clone()), loc))
            }
        }?;
        Some((under, loc + offset))
    }

    pub fn with_surfaces<F>(&self, processor: F)
    where
        F: FnMut(&WlSurface, &WlSurfaceData),
    {
        self.0.with_surfaces(processor);
    }

    pub fn send_frame<T, F>(
        &self,
        output: &Output,
        time: T,
        throttle: Option<Duration>,
        primary_scan_out_output: F,
    ) where
        T: Into<Duration>,
        F: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
    {
        self.0.send_frame(output, time, throttle, primary_scan_out_output)
    }

    pub fn send_dmabuf_feedback<'a, P, F>(
        &self,
        output: &Output,
        primary_scan_out_output: P,
        select_dmabuf_feedback: F,
    ) where
        P: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
        F: Fn(&WlSurface, &WlSurfaceData) -> &'a DmabufFeedback + Copy,
    {
        self.0
            .send_dmabuf_feedback(output, primary_scan_out_output, select_dmabuf_feedback)
    }

    pub fn take_presentation_feedback<F1, F2>(
        &self,
        output_feedback: &mut OutputPresentationFeedback,
        primary_scan_out_output: F1,
        presentation_feedback_flags: F2,
    ) where
        F1: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
        F2: FnMut(&WlSurface, &WlSurfaceData) -> wp_presentation_feedback::Kind + Copy,
    {
        self.0.take_presentation_feedback(
            output_feedback,
            primary_scan_out_output,
            presentation_feedback_flags,
        )
    }

    #[cfg(feature = "xwayland")]
    #[inline]
    pub fn is_x11(&self) -> bool {
        self.0.is_x11()
    }

    #[inline]
    pub fn is_wayland(&self) -> bool {
        self.0.is_wayland()
    }

    #[inline]
    pub fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.0.wl_surface()
    }

    #[inline]
    pub fn user_data(&self) -> &UserDataMap {
        self.0.user_data()
    }
}

impl IsAlive for WindowElement {
    #[inline]
    fn alive(&self) -> bool {
        self.0.alive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SSD(WindowElement);

impl IsAlive for SSD {
    #[inline]
    fn alive(&self) -> bool {
        self.0.alive()
    }
}

impl WaylandFocus for SSD {
    #[inline]
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.0.wl_surface()
    }
}

impl<BackendData: Backend> PointerTarget<EvoState<BackendData>> for SSD {
    fn enter(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        event: &MotionEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
        }
    }
    fn motion(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        event: &MotionEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
        }
    }
    fn relative_motion(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _event: &RelativeMotionEvent,
    ) {
    }
    fn button(
        &self,
        seat: &Seat<EvoState<BackendData>>,
        data: &mut EvoState<BackendData>,
        event: &ButtonEvent,
    ) {
        // Act on PRESS only. Without this check a single physical click
        // dispatched `clicked` TWICE -- once on press, once on release -- and
        // the two dispatches did not necessarily resolve to the same button,
        // because `HeaderBar::clicked` hit-tests relative to the *current*
        // `self.width` and the window can change width in between.
        //
        // That is alpha2 #7, and it is why "maximize works but restore-down
        // quits the app":
        //   press   -> click lands in the maximize band of a MAXIMIZED window
        //              (width 1280) -> defers unmaximize_request
        //   [idle]  -> window unmaximizes, header bar shrinks to its restored
        //              width, so every button band moves left
        //   release -> the SAME pointer x is now past `width - BUTTON_WIDTH`,
        //              i.e. inside the CLOSE band -> send_close()
        // The client closes its only window, and Gecko quits the whole app
        // with `quit-application-requested reason="lastwindow"` (confirmed
        // on-device). Maximizing looked fine only because both dispatches
        // landed on the same button when the width didn't change first.
        //
        // Matches input_handler.rs's existing
        // `wl_pointer::ButtonState::Pressed == state` idiom.
        if event.state != ButtonState::Pressed {
            return;
        }
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.clicked(seat, data, &self.0, event.serial);
        }
    }
    fn axis(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _frame: AxisFrame,
    ) {
    }
    fn frame(&self, _seat: &Seat<EvoState<BackendData>>, _data: &mut EvoState<BackendData>) {}
    fn leave(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _serial: Serial,
        _time: u32,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_leave();
        }
    }
    fn gesture_swipe_begin(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _event: &GestureSwipeBeginEvent,
    ) {
    }
    fn gesture_swipe_update(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _event: &GestureSwipeUpdateEvent,
    ) {
    }
    fn gesture_swipe_end(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _event: &GestureSwipeEndEvent,
    ) {
    }
    fn gesture_pinch_begin(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _event: &GesturePinchBeginEvent,
    ) {
    }
    fn gesture_pinch_update(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _event: &GesturePinchUpdateEvent,
    ) {
    }
    fn gesture_pinch_end(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _event: &GesturePinchEndEvent,
    ) {
    }
    fn gesture_hold_begin(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _event: &GestureHoldBeginEvent,
    ) {
    }
    fn gesture_hold_end(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _event: &GestureHoldEndEvent,
    ) {
    }
}

impl<BackendData: Backend> TouchTarget<EvoState<BackendData>> for SSD {
    fn down(
        &self,
        seat: &Seat<EvoState<BackendData>>,
        data: &mut EvoState<BackendData>,
        event: &smithay::input::touch::DownEvent,
        _seq: Serial,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
            state.header_bar.touch_down(seat, data, &self.0, event.serial);
        }
    }

    fn up(
        &self,
        seat: &Seat<EvoState<BackendData>>,
        data: &mut EvoState<BackendData>,
        event: &smithay::input::touch::UpEvent,
        _seq: Serial,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.touch_up(seat, data, &self.0, event.serial);
        }
    }

    fn motion(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        event: &smithay::input::touch::MotionEvent,
        _seq: Serial,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
        }
    }

    fn frame(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _seq: Serial,
    ) {
    }

    fn cancel(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _seq: Serial,
    ) {
    }

    fn shape(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _event: &smithay::input::touch::ShapeEvent,
        _seq: Serial,
    ) {
    }

    fn orientation(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        _event: &smithay::input::touch::OrientationEvent,
        _seq: Serial,
    ) {
    }
}

impl SpaceElement for WindowElement {
    fn geometry(&self) -> Rectangle<i32, Logical> {
        let mut geo = SpaceElement::geometry(&self.0);
        if self.decoration_state().is_ssd {
            geo.size.h += HEADER_BAR_HEIGHT;
        }
        geo
    }
    fn bbox(&self) -> Rectangle<i32, Logical> {
        let mut bbox = SpaceElement::bbox(&self.0);
        if self.decoration_state().is_ssd {
            bbox.size.h += HEADER_BAR_HEIGHT;
        }
        bbox
    }
    fn is_in_input_region(&self, point: &Point<f64, Logical>) -> bool {
        // Same render-origin caveat as `ssd_header_rect`: `point` is relative
        // to the buffer/shadow origin, so the header occupies
        // `geometry().loc.y ..= +HEADER_BAR_HEIGHT`, not `0 ..= HEADER_BAR_HEIGHT`.
        if let Some(header) = self.ssd_header_rect() {
            header.to_f64().contains(*point)
                || SpaceElement::is_in_input_region(
                    &self.0,
                    &(*point - Point::from((0.0, HEADER_BAR_HEIGHT as f64))),
                )
        } else {
            SpaceElement::is_in_input_region(&self.0, point)
        }
    }
    fn z_index(&self) -> u8 {
        if self.is_shell() {
            // Bloom is the desktop backdrop (wallpaper + shell): pin it beneath
            // every app window so click-to-focus (or any raise) can never bring
            // it over them. Apps keep smithay's default (RenderZindex::Shell =
            // 30); popups and layer-shell overlays sort above via their own
            // higher z-index. This is what makes the "wallpaper < shell <
            // windows < popups" layering hold regardless of stacking order.
            smithay::desktop::space::RenderZindex::Background as u8
        } else {
            SpaceElement::z_index(&self.0)
        }
    }

    fn set_activate(&self, activated: bool) {
        SpaceElement::set_activate(&self.0, activated);
    }
    fn output_enter(&self, output: &Output, overlap: Rectangle<i32, Logical>) {
        SpaceElement::output_enter(&self.0, output, overlap);
    }
    fn output_leave(&self, output: &Output) {
        SpaceElement::output_leave(&self.0, output);
    }
    #[profiling::function]
    fn refresh(&self) {
        SpaceElement::refresh(&self.0);
    }
}

render_elements!(
    pub WindowRenderElement<R> where R: ImportAll + ImportMem;
    Window=WaylandSurfaceRenderElement<R>,
    Decoration=DecorationRenderElement<R>,
);

impl<R: Renderer> std::fmt::Debug for WindowRenderElement<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Window(arg0) => f.debug_tuple("Window").field(arg0).finish(),
            Self::Decoration(arg0) => f.debug_tuple("Decoration").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

impl<R> AsRenderElements<R> for WindowElement
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + Send + 'static,
{
    type RenderElement = WindowRenderElement<R>;

    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        renderer: &mut R,
        mut location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        let window_bbox = SpaceElement::bbox(&self.0);

        if self.decoration_state().is_ssd && !window_bbox.is_empty() {
            let window_geo = SpaceElement::geometry(&self.0);

            // Draw the header at the window's *visible* top-left. `location`
            // is the render origin (= buffer origin), so for a client with a
            // CSD shadow margin it sits `window_geo.loc` up-and-left of the
            // window -- which is exactly the offset that made the SSD look
            // detached from its window. See `ssd_header_rect`.
            let header_loc = location + window_geo.loc.to_physical_precise_round(scale);

            // Title + full visible height (header + client) — the SSD title
            // text and the side borders both need these. Computed before the
            // decoration_state borrow (window_title reads the surface, not the
            // decoration state).
            let title = self.window_title();
            let full_height =
                (HEADER_BAR_HEIGHT + window_geo.size.h).max(HEADER_BAR_HEIGHT) as u32;

            let mut state = self.decoration_state();
            let width = window_geo.size.w;
            state.header_bar.redraw(width as u32, full_height, &title);
            let mut vec = AsRenderElements::<R>::render_elements::<WindowRenderElement<R>>(
                &state.header_bar,
                renderer,
                header_loc,
                scale,
                alpha,
            );

            location.y += (scale.y * HEADER_BAR_HEIGHT as f64) as i32;

            let window_elements =
                AsRenderElements::render_elements(&self.0, renderer, location, scale, alpha);
            vec.extend(window_elements);
            vec.into_iter().map(C::from).collect()
        } else {
            AsRenderElements::render_elements(&self.0, renderer, location, scale, alpha)
                .into_iter()
                .map(C::from)
                .collect()
        }
    }
}
