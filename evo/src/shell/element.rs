use std::{borrow::Cow, cell::RefCell, time::Duration};

use smithay::{
    backend::renderer::{
        element::{
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            AsRenderElements, Kind,
        },
        utils::with_renderer_surface_state,
        ImportAll, ImportMem, Renderer, Texture,
    },
    desktop::{
        space::SpaceElement, utils::OutputPresentationFeedback, PopupManager, Window, WindowSurface,
        WindowSurfaceType,
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
        wayland_protocols::{
            wp::presentation_time::server::wp_presentation_feedback,
            xdg::shell::server::xdg_toplevel,
        },
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

use super::ssd::{DecorationRenderElement, HEADER_BAR_HEIGHT, RESIZE_BAND};
use crate::anim::shader::{self, ClippedSurface, GlesAccess};
use super::xdg::{OVERLAY_APP_ID, SHELL_APP_ID};
use crate::{focus::PointerFocusTarget, state::Backend, EvoState};

/// Geometry a window had before it was maximized (or minimized), stored in
/// its user_data so the un-maximize/restore path can put it back. Shared
/// between the XDG and X11 shell handlers and the SSD title-bar buttons so
/// both window kinds and both trigger paths (client-requested vs.
/// title-bar-clicked) agree on the same saved geometry.
#[derive(Debug, Default)]
pub(crate) struct OldGeometry(RefCell<Option<Rectangle<i32, Logical>>>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TileSide {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SavedTile {
    pub side: TileSide,
    pub restore_geo: Rectangle<i32, Logical>,
}

/// Pre-tile floating geometry and current tile side for Super+Left/Right snap/restore.
#[derive(Debug, Default)]
pub(crate) struct TileState(RefCell<Option<SavedTile>>);

impl TileState {
    pub fn get(&self) -> Option<SavedTile> {
        *self.0.borrow()
    }

    pub fn save(&self, tile: SavedTile) {
        *self.0.borrow_mut() = Some(tile);
    }

    pub fn take(&self) -> Option<SavedTile> {
        self.0.borrow_mut().take()
    }
}

/// Name of the active `Output` a fullscreen or shell window is assigned to,
/// so layout fixups keep windows on secondary outputs anchored cleanly even if
/// outputs move or resize.
#[derive(Debug, Default)]
pub(crate) struct FullscreenOutputName(RefCell<Option<String>>);

impl FullscreenOutputName {
    pub fn get(&self) -> Option<String> {
        self.0.borrow().clone()
    }

    pub fn set(&self, name: Option<String>) {
        *self.0.borrow_mut() = name;
    }
}

/// Marks a window whose location is a guess, made before its size was known.
///
/// A new toplevel gets a location when it is created, but its size only
/// arrives with its first buffer. So the location is picked blind, and a
/// window as large as vela's (1152x720, plus the header) landed partly off
/// the output about half the time, band and buttons included. The first
/// commit that gives the window a real size while it floats places it for
/// real (`first_real_placement` in shell/xdg.rs). A window born maximized
/// keeps the mark until it is first restored, which is also why its guessed
/// location is never saved as the place to restore to.
#[derive(Default)]
pub(crate) struct PlacementPending(std::cell::Cell<bool>);
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

/// How a window's client connected, recorded at creation (see
/// `crate::trust`). Kept in the window's `user_data`; a window without one
/// is an app.
struct WindowOrigin {
    trusted: bool,
    /// The app id the launcher gave the client's connection, if it came
    /// through a security context that carried one.
    context_app_id: Option<String>,
}

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
        if !self.is_ssd() {
            return None;
        }
        let geo = SpaceElement::geometry(&self.0);
        Some(Rectangle::new(
            geo.loc,
            Size::from((geo.size.w, HEADER_BAR_HEIGHT)),
        ))
    }

    /// The SSD frame (header bar and client) in the same render-origin space
    /// as `ssd_header_rect`, when it can be resized from its edges. Returns
    /// `None` when the window isn't server-decorated, or is maximized or
    /// fullscreen. Such a window fills its space, so it has no edge to drag,
    /// and its band would sit off the output anyway.
    fn ssd_resizable_frame(&self) -> Option<Rectangle<i32, Logical>> {
        if !self.is_ssd() {
            return None;
        }
        // The committed state, i.e. what is on screen now. A maximize that
        // is only pending has not changed anything the pointer can see yet.
        let fills_its_space = match self.0.underlying_surface() {
            WindowSurface::Wayland(toplevel) => toplevel.with_committed_state(|state| {
                state.is_some_and(|state| {
                    state.states.contains(xdg_toplevel::State::Maximized)
                        || state.states.contains(xdg_toplevel::State::Fullscreen)
                })
            }),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(x11) => x11.is_maximized() || x11.is_fullscreen(),
        };
        if fills_its_space {
            return None;
        }
        Some(SpaceElement::geometry(self))
    }

    /// The resizable SSD frame and the outer edge of the invisible band
    /// around it, where a press starts a resize (`HeaderBar::clicked`).
    ///
    /// The band lies wholly OUTSIDE the visible frame. Taking it from inside
    /// would steal the client's own edge pixels, such as a scrollbar. That is
    /// why it needs the matching `bbox` and `is_in_input_region` below:
    /// without them the space never offers this window a point outside it.
    fn ssd_resize_band(&self) -> Option<(Rectangle<i32, Logical>, Rectangle<i32, Logical>)> {
        let frame = self.ssd_resizable_frame()?;
        let outer = Rectangle::new(
            frame.loc - Point::from((RESIZE_BAND, RESIZE_BAND)),
            frame.size + Size::from((2 * RESIZE_BAND, 2 * RESIZE_BAND)),
        );
        Some((frame, outer))
    }

    /// Whether `point` (render-origin space) is in the resize band.
    fn in_ssd_resize_band(&self, point: Point<f64, Logical>) -> bool {
        self.ssd_resize_band().is_some_and(|(frame, outer)| {
            outer.to_f64().contains(point) && !frame.to_f64().contains(point)
        })
    }

    /// How much taller server-side decoration makes the window than its
    /// client: the header bar, or nothing for a client that decorates itself.
    ///
    /// Resize grabs work in frame size, which is what the user drags and what
    /// `geometry()` reports. A configure carries the client's own size, so
    /// they convert with this at the protocol boundary.
    pub fn decoration_height(&self) -> i32 {
        if self.is_ssd() {
            HEADER_BAR_HEIGHT
        } else {
            0
        }
    }

    /// Mark this window's location as a guess, or as settled (see
    /// `PlacementPending`).
    pub fn set_placement_pending(&self, pending: bool) {
        self.user_data().insert_if_missing(PlacementPending::default);
        if let Some(mark) = self.user_data().get::<PlacementPending>() {
            mark.0.set(pending);
        }
    }

    pub fn placement_pending(&self) -> bool {
        self.user_data()
            .get::<PlacementPending>()
            .is_some_and(|mark| mark.0.get())
    }

    /// What the title bar says, and in what colours: the app's icon and name
    /// from its identity (`SetAppIdentity`), looked up by the window's trusted
    /// app id, never its claim, then the client's own title when it adds
    /// anything. An X11 window, or an app with no identity, shows its title as
    /// it always did, on the plain material.
    pub fn title_source(&self) -> super::ssd::TitleSource {
        use super::identity;
        let title = self.window_title();
        #[cfg(feature = "xwayland")]
        let x11 = self.is_x11();
        #[cfg(not(feature = "xwayland"))]
        let x11 = false;
        let found = if x11 { None } else { self.app_id().and_then(|id| identity::lookup(&id)) };
        super::ssd::TitleSource {
            label: identity::title_label(found.as_ref().map(|i| i.name.as_str()), &title),
            icon: found.as_ref().and_then(|i| i.icon.clone()),
            colors: identity::bar_colors(
                found.and_then(|i| i.accent),
                identity::dark(),
                identity::material(identity::dark()),
            ),
        }
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
                    .and_then(|d| d.lock().ok().and_then(|g| g.title.clone()))
                    .filter(|t| !t.trim().is_empty())
            })
            // The fallback is the window's identity, not its claim: an app
            // that sets no title must not be able to caption itself as
            // another app.
            .or_else(|| self.app_id())
            .unwrap_or_default(),
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

    /// The app_id the client claims: the xdg_toplevel app_id, or the X11
    /// class. `None` until a Wayland client sets one. Anyone can claim
    /// anything, so nothing about a window's role or identity is decided
    /// from this alone (see `app_id` and `is_trusted`).
    pub fn claimed_app_id(&self) -> Option<String> {
        match self.0.underlying_surface() {
            WindowSurface::Wayland(toplevel) => with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok().and_then(|g| g.app_id.clone()))
            }),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(s) => Some(s.class()),
        }
    }

    /// Record how this window's client connected (see `crate::trust`). Done
    /// once, when the toplevel is created, from the connection itself.
    pub fn set_origin(&self, trusted: bool, context_app_id: Option<String>) {
        self.user_data().insert_if_missing(|| WindowOrigin {
            trusted,
            context_app_id,
        });
    }

    /// Whether the window's client is part of the system. A window whose
    /// origin was never recorded (an X11 window, which has no connection of
    /// its own) is an app.
    pub fn is_trusted(&self) -> bool {
        self.user_data()
            .get::<WindowOrigin>()
            .is_some_and(|origin| origin.trusted)
    }

    /// Who the window is: the app id the launcher gave its connection when
    /// it has one, else what the client claims. For an app this is what
    /// appHost binds and what the title bar falls back to.
    pub fn app_id(&self) -> Option<String> {
        let context = self.user_data().get::<WindowOrigin>();
        crate::trust::effective_app_id(
            context.and_then(|origin| origin.context_app_id.as_deref()),
            self.claimed_app_id(),
        )
    }

    /// Whether this window is the desktop shell (Bloom). Only a trusted
    /// client can be: an app that calls itself `bloom` is an app. Used to
    /// pin it beneath app windows in the z-order.
    pub(crate) fn is_shell(&self) -> bool {
        self.is_trusted() && self.claimed_app_id().as_deref() == Some(SHELL_APP_ID)
    }

    /// Whether this window is Bloom's overlay (see `super::overlay`). Trusted
    /// clients only, for the same reason.
    pub(crate) fn is_overlay(&self) -> bool {
        self.is_trusted() && self.claimed_app_id().as_deref() == Some(OVERLAY_APP_ID)
    }

    pub fn surface_under(
        &self,
        location: Point<f64, Logical>,
        window_type: WindowSurfaceType,
    ) -> Option<(PointerFocusTarget, Point<i32, Logical>)> {
        let is_ssd = self.is_ssd();
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
            // The resize band belongs to the frame too, and is reported from
            // the same origin, so `HeaderBar` sees frame-relative
            // coordinates: negative above and left of the frame, and past
            // its width or height below and right.
            if self.in_ssd_resize_band(location) {
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
        if self.0.is_ssd() {
        let mut state = self.0.decoration_state();
            state.header_bar.pointer_enter(event.location);
        }
    }
    fn motion(
        &self,
        _seat: &Seat<EvoState<BackendData>>,
        _data: &mut EvoState<BackendData>,
        event: &MotionEvent,
    ) {
        if self.0.is_ssd() {
        let mut state = self.0.decoration_state();
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
        if self.0.is_ssd() {
            let mut state = self.0.decoration_state();
            state
                .header_bar
                .clicked_button(seat, data, &self.0, event.serial, event.time, event.button);
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
        if self.0.is_ssd() {
        let mut state = self.0.decoration_state();
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
        if self.0.is_ssd() {
        let mut state = self.0.decoration_state();
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
        if self.0.is_ssd() {
        let mut state = self.0.decoration_state();
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
        if self.0.is_ssd() {
        let mut state = self.0.decoration_state();
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
        if self.is_ssd() {
            geo.size.h += HEADER_BAR_HEIGHT;
        }
        geo
    }
    fn bbox(&self) -> Rectangle<i32, Logical> {
        let mut bbox = SpaceElement::bbox(&self.0);
        if self.is_ssd() {
            bbox.size.h += HEADER_BAR_HEIGHT;
        }
        // Space::element_under filters on bbox before it ever asks
        // is_in_input_region, so the resize band has to be inside it.
        if let Some((_, outer)) = self.ssd_resize_band() {
            bbox = bbox.merge(outer);
        }
        bbox
    }
    fn is_in_input_region(&self, point: &Point<f64, Logical>) -> bool {
        if self.is_overlay() {
            // Only where Bloom says it draws (SetOverlayRegion). Everywhere
            // else the overlay is glass, and the click goes to what's beneath.
            // While the screen is locked the overlay is the lock page and the
            // whole screen is its to click (shell/lock.rs).
            return (super::lock::input_locked() || super::overlay::region_contains(*point))
                && SpaceElement::is_in_input_region(&self.0, point);
        }
        // Same render-origin caveat as `ssd_header_rect`: `point` is relative
        // to the buffer/shadow origin, so the header occupies
        // `geometry().loc.y ..= +HEADER_BAR_HEIGHT`, not `0 ..= HEADER_BAR_HEIGHT`.
        if let Some(header) = self.ssd_header_rect() {
            header.to_f64().contains(*point)
                || self.in_ssd_resize_band(*point)
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
        } else if self.is_overlay() {
            // The overlay is the shell's layer ABOVE app windows (toasts, the
            // lock screen), so it sorts over them the way the desktop sorts
            // under them: Top, the level of a wlr-layer-shell top panel.
            smithay::desktop::space::RenderZindex::Top as u8
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
    pub WindowRenderElement<R> where R: ImportAll + ImportMem + GlesAccess;
    Window=WaylandSurfaceRenderElement<R>,
    // Client content with rounded bottom corners (see `anim::shader`).
    Clipped=ClippedSurface<R>,
    // An opening window drawn as Lumen's Bloom (see `anim::bloom`).
    Bloom=crate::anim::shader::BloomSurface<R>,
    Decoration=DecorationRenderElement<R>,
    // The title bar's frosted glass (see `shell::frost`).
    Frost=crate::anim::shader::FrostElement,
    // A popup that is fading in (see `popup_pose`).
    Popup=crate::anim::transform::Transformed<WaylandSurfaceRenderElement<R>>,
);

impl<R: Renderer> std::fmt::Debug for WindowRenderElement<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Window(arg0) => f.debug_tuple("Window").field(arg0).finish(),
            Self::Clipped(arg0) => f.debug_tuple("Clipped").field(arg0).finish(),
            Self::Bloom(arg0) => f.debug_tuple("Bloom").field(arg0).finish(),
            Self::Decoration(arg0) => f.debug_tuple("Decoration").field(arg0).finish(),
            Self::Frost(arg0) => f.debug_tuple("Frost").field(arg0).finish(),
            Self::Popup(arg0) => f.debug_tuple("Popup").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

/// A popup's fade-in, kept in its surface's data map. Made the first frame the
/// popup is seen with a buffer.
#[derive(Default)]
struct PopupAnimSlot {
    started: std::cell::Cell<bool>,
    anim: RefCell<Option<crate::anim::TransformAnim>>,
}

/// The pose a popup is drawn in this frame (scale, alpha), or `None` when it
/// is drawn plain. Starts the animation the first time the popup has a buffer.
fn popup_pose(surface: &WlSurface) -> Option<crate::anim::WindowTransform> {
    let mapped = with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
    if !mapped {
        return None;
    }
    let now = crate::anim::now();
    with_states(surface, |states| {
        states.data_map.insert_if_missing(PopupAnimSlot::default);
        let slot = states.data_map.get::<PopupAnimSlot>()?;
        if !slot.started.replace(true) {
            *slot.anim.borrow_mut() = crate::anim::TransformAnim::popup_in(&crate::anim::settings(), now);
        }
        let mut anim = slot.anim.borrow_mut();
        let running = anim.as_ref()?;
        if running.is_finished(now) {
            *anim = None;
            return None;
        }
        // Popups scale about their own top-left, which the caller supplies
        // as the origin; only the scale and alpha come from here.
        Some(running.sample(now, Rectangle::default()))
    })
}

impl WindowElement {
    /// The window's render elements, SSD header included: the header, then
    /// popups (if `popups`; each fading in on its own clock), then the
    /// toplevel's surface tree. `AsRenderElements::render_elements` is this
    /// with popups.
    pub fn frame_render_elements<R>(
        &self,
        renderer: &mut R,
        mut location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
        popups: bool,
    ) -> Vec<WindowRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem + GlesAccess,
        R::TextureId: Clone + Texture + Send + 'static,
    {
        let window_bbox = SpaceElement::bbox(&self.0);
        let mut vec: Vec<WindowRenderElement<R>> = Vec::new();
        // Client content is clipped to the frame's radius at its bottom
        // corners, once the header has moved `location` below itself.
        let mut clip: Option<(gles_clip::Clip, smithay::backend::renderer::gles::GlesTexProgram)> = None;

        if self.is_ssd() && !window_bbox.is_empty() {
            let window_geo = SpaceElement::geometry(&self.0);

            // Draw the header at the window's *visible* top-left. `location`
            // is the render origin (= buffer origin), so for a client with a
            // CSD shadow margin it sits `window_geo.loc` up-and-left of the
            // window -- which is exactly the offset that made the SSD look
            // detached from its window. See `ssd_header_rect`.
            let header_loc = location + window_geo.loc.to_physical_precise_round(scale);

            // Title + full visible height (header + client) — the SSD title
            // text and the side borders both need these. Computed before the
            // decoration_state borrow (title_source reads the surface and the
            // identity table, not the decoration state).
            let title = self.title_source();
            let busy = self.is_busy();
            let full_height =
                (HEADER_BAR_HEIGHT + window_geo.size.h).max(HEADER_BAR_HEIGHT) as u32;

            let (focus, _) = self.depth_levels(crate::anim::now());
            let square = self.is_square();
            let radius = self.content_corner_radius();
            let programs = if radius > 0.0 { shader::programs(renderer) } else { None };
            // The bar's glass: true frost (F2) or frosted wallpaper (F1) when
            // captured, the shader works and the window has been placed on the
            // output; else the flat fill the header draws itself (F0). The
            // inactive variant is made only while something shows it.
            let gear = super::frost::gear_for_window(
                self,
                renderer,
                title.colors.dark,
                super::frost::Gear::wants_inactive(focus as f32),
            )
            .filter(|_| self.frost_anchor().is_some());
            self.set_frost_tier(gear.as_ref().map_or(super::frost::Tier::F0, |g| g.tier));
            let mut state = self.decoration_state();
            let width = window_geo.size.w;
            state.header_bar.focus = focus as f32;
            state.header_bar.square = square;
            state.header_bar.bottom_round = programs.is_some();
            state.header_bar.busy = busy;
            state.header_bar.frosted = gear.is_some();
            state.header_bar.redraw(width as u32, full_height, &title);
            vec = AsRenderElements::<R>::render_elements::<WindowRenderElement<R>>(
                &state.header_bar,
                renderer,
                header_loc,
                scale,
                alpha,
            );

            location.y += (scale.y * HEADER_BAR_HEIGHT as f64) as i32;

            let bar_radii = state.header_bar.bar_radii();
            drop(state);
            if let Some(gear) = gear.as_ref() {
                // Behind the title and the controls (which come first in the
                // list), in front of nothing: the bar's glass.
                let geo = Rectangle::new(
                    header_loc,
                    Size::<i32, Logical>::from((width, HEADER_BAR_HEIGHT)).to_physical_precise_round(scale),
                );
                let bar = super::frost::BarFacts {
                    geo,
                    radii: bar_radii.scale(scale.x),
                    px: scale.x,
                    focus: focus as f32,
                    colors: &title.colors,
                    alpha,
                };
                vec.extend(super::frost::bar_elements(self, gear, &bar).into_iter().map(WindowRenderElement::Frost));
            }
            if let Some(programs) = programs {
                let loc = location + window_geo.loc.to_physical_precise_round(scale);
                let size = window_geo.size.to_physical_precise_round(scale);
                clip = Some((
                    gles_clip::Clip { rect: Rectangle::new(loc, size), radius: (radius * scale.x).round() },
                    programs.clip,
                ));
            }
        }
        let clipped = |element: WaylandSurfaceRenderElement<R>| -> WindowRenderElement<R> {
            let Some((c, program)) = clip.as_ref() else {
                return WindowRenderElement::Window(element);
            };
            if !ClippedSurface::touches_corner(&element, scale, c.rect, c.radius as i32) {
                return WindowRenderElement::Window(element);
            }
            match ClippedSurface::new(element, program.clone(), scale, c.rect, c.radius) {
                Ok(clipped) => WindowRenderElement::Clipped(clipped),
                Err(element) => WindowRenderElement::Window(element),
            }
        };

        match self.0.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                let surface = toplevel.wl_surface();
                if popups {
                    for (popup, popup_offset) in PopupManager::popups_for_surface(surface) {
                        let offset: Point<i32, Physical> = (SpaceElement::geometry(&self.0).loc + popup_offset
                            - popup.geometry().loc)
                            .to_physical_precise_round(scale);
                        let pose = popup_pose(popup.wl_surface());
                        let popup_alpha = alpha * pose.map_or(1.0, |p| p.alpha);
                        let elements = render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<R>>(
                            renderer,
                            popup.wl_surface(),
                            location + offset,
                            scale,
                            popup_alpha,
                            Kind::Unspecified,
                        );
                        match pose {
                            Some(pose) => {
                                let origin = location + offset + popup.geometry().loc.to_physical_precise_round(scale);
                                let physical = crate::anim::transform::PhysicalTransform {
                                    origin,
                                    scale: Scale::from((pose.scale.x, pose.scale.y)),
                                    offset: (0, 0).into(),
                                };
                                vec.extend(
                                    crate::anim::transform::wrap_all(elements, physical)
                                        .into_iter()
                                        .map(WindowRenderElement::Popup),
                                );
                            }
                            None => vec.extend(elements.into_iter().map(WindowRenderElement::Window)),
                        }
                    }
                }
                vec.extend(
                    render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<R>>(
                        renderer,
                        surface,
                        location,
                        scale,
                        alpha,
                        Kind::Unspecified,
                    )
                    .into_iter()
                    .map(&clipped),
                );
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(_) => {
                vec.extend(
                    AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(
                        &self.0, renderer, location, scale, alpha,
                    )
                    .into_iter()
                    .map(&clipped),
                );
            }
        }
        vec
    }
}

impl<R> AsRenderElements<R> for WindowElement
where
    R: Renderer + ImportAll + ImportMem + GlesAccess,
    R::TextureId: Clone + Texture + Send + 'static,
{
    type RenderElement = WindowRenderElement<R>;

    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        renderer: &mut R,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        self.frame_render_elements(renderer, location, scale, alpha, true)
            .into_iter()
            .map(C::from)
            .collect()
    }
}

mod gles_clip {
    use smithay::utils::{Physical, Rectangle};

    /// The rounded rectangle client content is clipped to, output-physical.
    pub struct Clip {
        pub rect: Rectangle<i32, Physical>,
        pub radius: f64,
    }
}
