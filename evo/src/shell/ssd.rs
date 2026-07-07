//! Server-Side Decoration (SSD) rendering for GeckoOS
//! 
//! This module handles the actual rendering of window decorations using Smithay's
//! solid color buffers, integrated with the GeckoOS decoration system for theming
//! and sophisticated hit testing.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                solid::{SolidColorBuffer, SolidColorRenderElement},
                AsRenderElements, Kind,
            },
            ImportAll, ImportMem, Renderer, Texture,
        },
    },
    desktop::WindowSurface,
    input::Seat,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    render_elements,
    utils::{Logical, Point, Serial, Transform},
    wayland::shell::xdg::XdgShellHandler,
};
use std::cell::{RefCell, RefMut};
use tracing::{debug, warn};

use crate::{state::Backend, EvoState};
use super::WindowElement;

// === PYXIS DECORATION SHAPE COLORS ===
// Matches Bloom's window-chrome design language (src/components/DesktopWindow.tsx):
// red circle (close), blue square (maximize), yellow downward triangle (minimize),
// on a translucent "glass" title bar with 16px rounded top corners.
const CLOSE_RGB: [u8; 3] = [0xFF, 0x52, 0x52];
const CLOSE_HOVER_RGB: [u8; 3] = [0xD3, 0x2F, 0x2F];
const MAXIMIZE_RGB: [u8; 3] = [0x21, 0x96, 0xF3];
const MAXIMIZE_HOVER_RGB: [u8; 3] = [0x19, 0x76, 0xD2];
const MINIMIZE_RGB: [u8; 3] = [0xFF, 0xC1, 0x07];
const MINIMIZE_HOVER_RGB: [u8; 3] = [0xFF, 0xB3, 0x00];
const GLASS_RGBA: [u8; 4] = [0xFF, 0xFF, 0xFF, 0x33];
const GLASS_CORNER_RADIUS: f32 = 16.0;

/// Writes one BGRA8888-in-memory pixel (matching `Fourcc::Argb8888`'s byte
/// order) into `buf` at `(x, y)` for a buffer `stride` pixels wide.
fn put_pixel(buf: &mut [u8], stride: u32, x: u32, y: u32, rgba: [u8; 4]) {
    let idx = ((y * stride + x) * 4) as usize;
    buf[idx] = rgba[2]; // B
    buf[idx + 1] = rgba[1]; // G
    buf[idx + 2] = rgba[0]; // R
    buf[idx + 3] = rgba[3]; // A
}

/// Rasterizes a filled circle (transparent outside) into an RGBA buffer.
fn rasterize_circle(size: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let center = size as f32 / 2.0;
    let radius = size as f32 / 2.0 - 2.0;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            if dx * dx + dy * dy <= radius * radius {
                put_pixel(&mut buf, size, x, y, [rgb[0], rgb[1], rgb[2], 0xFF]);
            }
        }
    }
    buf
}

/// Rasterizes a downward-pointing equilateral-ish triangle (transparent
/// outside), matching Bloom's minimize-button glyph orientation.
fn rasterize_triangle_down(size: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let margin = size as f32 * 0.22;
    let top_left = (margin, margin);
    let top_right = (size as f32 - margin, margin);
    let apex = (size as f32 / 2.0, size as f32 - margin);

    fn edge(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
        (p.0 - a.0) * (b.1 - a.1) - (p.1 - a.1) * (b.0 - a.0)
    }

    for y in 0..size {
        for x in 0..size {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let d1 = edge(top_left, top_right, p);
            let d2 = edge(top_right, apex, p);
            let d3 = edge(apex, top_left, p);
            let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
            let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
            if !(has_neg && has_pos) {
                put_pixel(&mut buf, size, x, y, [rgb[0], rgb[1], rgb[2], 0xFF]);
            }
        }
    }
    buf
}

/// Rasterizes a translucent "glass" bar with rounded top corners only
/// (the bottom edge meets the client area, so it stays square).
fn rasterize_glass_bar(width: u32, height: u32, radius: f32, rgba: [u8; 4]) -> Vec<u8> {
    let mut buf = vec![0u8; (width * height * 4) as usize];
    let r = radius.min(width as f32 / 2.0).min(height as f32);
    for y in 0..height {
        for x in 0..width {
            let xf = x as f32 + 0.5;
            let yf = y as f32 + 0.5;
            let mut inside = true;
            if yf < r {
                if xf < r {
                    let (dx, dy) = (r - xf, r - yf);
                    inside = dx * dx + dy * dy <= r * r;
                } else if xf > width as f32 - r {
                    let (dx, dy) = (xf - (width as f32 - r), r - yf);
                    inside = dx * dx + dy * dy <= r * r;
                }
            }
            if inside {
                put_pixel(&mut buf, width, x, y, rgba);
            }
        }
    }
    buf
}

fn memory_buffer(width: u32, height: u32, pixels: Vec<u8>) -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Argb8888,
        (width as i32, height as i32),
        1,
        Transform::Normal,
        None,
    )
}

render_elements! {
    pub DecorationRenderElement<R> where R: ImportAll + ImportMem;
    Solid=SolidColorRenderElement,
    Memory=MemoryRenderBufferRenderElement<R>,
}

impl<R: Renderer> std::fmt::Debug for DecorationRenderElement<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Solid(arg0) => f.debug_tuple("Solid").field(arg0).finish(),
            Self::Memory(arg0) => f.debug_tuple("Memory").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

// Import GeckoOS decorations when feature is enabled
#[cfg(feature = "decorations")]
use crate::enhancements::decorations::{
    ControlButton, Theme, WindowDecoration, ButtonState, WindowGeometry,
};

/// Window decoration state stored in window user data
pub struct SSDState {
    pub is_ssd: bool,
    pub header_bar: HeaderBar,
    
    // GeckoOS decoration metadata (themes, hit testing, etc.)
    #[cfg(feature = "decorations")]
    pub decoration: Option<WindowDecoration>,
}

/// Header bar rendering state
#[derive(Debug, Clone)]
pub struct HeaderBar {
    pub pointer_loc: Option<Point<f64, Logical>>,
    pub width: u32,
    pub close_button_hover: bool,
    pub maximize_button_hover: bool,
    pub minimize_button_hover: bool,  // NEW: Added minimize button support
    
    // Rendering buffers -- close/minimize are shaped (circle/triangle) so
    // they're rasterized textures; maximize is a square, i.e. just a
    // plain-colored rect, so it stays a SolidColorBuffer.
    pub background: MemoryRenderBuffer,
    pub close_button: MemoryRenderBuffer,
    pub maximize_button: SolidColorBuffer,
    pub minimize_button: MemoryRenderBuffer,
    
    // Theme support
    #[cfg(feature = "decorations")]
    pub theme: Theme,
}

// Layout constants
pub const HEADER_BAR_HEIGHT: i32 = 32;
const BUTTON_HEIGHT: u32 = HEADER_BAR_HEIGHT as u32;
const BUTTON_WIDTH: u32 = 32;

impl HeaderBar {
    /// Create a new header bar with default or themed colors
    pub fn new() -> Self {
        Self {
            pointer_loc: None,
            width: 0,
            close_button_hover: false,
            maximize_button_hover: false,
            minimize_button_hover: false,
            // Fixed 32x32 button size -- rasterize the real (non-hover)
            // shapes up front; redraw() swaps to hover variants as needed.
            background: memory_buffer(1, 1, vec![0; 4]),
            close_button: memory_buffer(
                BUTTON_WIDTH,
                BUTTON_HEIGHT,
                rasterize_circle(BUTTON_WIDTH, CLOSE_RGB),
            ),
            maximize_button: SolidColorBuffer::default(),
            minimize_button: memory_buffer(
                BUTTON_WIDTH,
                BUTTON_HEIGHT,
                rasterize_triangle_down(BUTTON_WIDTH, MINIMIZE_RGB),
            ),
            #[cfg(feature = "decorations")]
            theme: Theme::default(),
        }
    }

    /// Set theme and trigger redraw
    #[cfg(feature = "decorations")]
    pub fn set_theme(&mut self, theme: Theme) {
        debug!("Setting header bar theme");
        self.theme = theme;
        self.redraw(self.width);
    }
    
    /// Update pointer location
    pub fn pointer_enter(&mut self, loc: Point<f64, Logical>) {
        self.pointer_loc = Some(loc);
    }
    
    /// Clear pointer location
    pub fn pointer_leave(&mut self) {
        self.pointer_loc = None;
    }
    
    /// Handle click with GeckoOS decoration hit testing
    #[cfg(feature = "decorations")]
    pub fn clicked_with_decoration<B: Backend>(
        &mut self,
        seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        serial: Serial,
        decoration: &WindowDecoration,
    ) {
        if let Some(loc) = self.pointer_loc.as_ref() {
            // Use GeckoOS decoration hit testing (more sophisticated)
            let simple_geom = WindowGeometry {
                x: 0,
                y: 0,
                width: self.width,
                height: HEADER_BAR_HEIGHT as u32,
            };
            
            if let Some(button) = decoration.get_button_at(
                loc.x as i32, 
                loc.y as i32, 
                &simple_geom
            ) {
                match button {
                    ControlButton::Close => {
                        debug!("Close button clicked (GeckoOS decoration)");
                        self.handle_close(window);
                    }
                    ControlButton::Maximize => {
                        debug!("Maximize button clicked (GeckoOS decoration)");
                        self.handle_maximize(state, window);
                    }
                    ControlButton::Minimize => {
                        debug!("Minimize button clicked (GeckoOS decoration)");
                        self.handle_minimize(state, window);
                    }
                }
                return;
            }
            
            // If not on a button, handle window move
            self.handle_move(seat, state, window, serial);
        }
    }
    
    /// Handle click with fallback simple hit testing
    pub fn clicked<B: Backend>(
        &mut self,
        seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        serial: Serial,
    ) {
        match self.pointer_loc.as_ref() {
            // Close button (rightmost)
            Some(loc) if loc.x >= (self.width - BUTTON_WIDTH) as f64 => {
                debug!("Close button clicked");
                self.handle_close(window);
            }
            
            // Maximize button (second from right)
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 2)) as f64 => {
                debug!("Maximize button clicked");
                self.handle_maximize(state, window);
            }
            
            // Minimize button (third from right)
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 3)) as f64 => {
                debug!("Minimize button clicked");
                self.handle_minimize(state, window);
            }
            
            // Title bar drag area
            Some(_) => {
                self.handle_move(seat, state, window, serial);
            }
            
            _ => {}
        }
    }
    
    /// Handle close button action
    fn handle_close(&self, window: &WindowElement) {
        match window.0.underlying_surface() {
            WindowSurface::Wayland(w) => w.send_close(),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(w) => {
                let _ = w.close();
            }
        };
    }
    
    /// Handle maximize button action -- toggles: grows the window if it
    /// isn't currently maximized, restores its pre-maximize geometry
    /// (see `OldGeometry`) if it is.
    fn handle_maximize<B: Backend>(&self, state: &mut EvoState<B>, window: &WindowElement) {
        match window.0.underlying_surface() {
            WindowSurface::Wayland(w) => {
                let is_maximized =
                    w.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Maximized));
                if is_maximized {
                    state.unmaximize_request(w.clone());
                } else {
                    state.maximize_request(w.clone());
                }
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(w) => {
                let is_maximized = w.is_maximized();
                let surface = w.clone();
                state.handle.insert_idle(move |data| {
                    if is_maximized {
                        data.unmaximize_request_x11(&surface);
                    } else {
                        data.maximize_request_x11(&surface);
                    }
                });
            }
        };
    }

    /// Handle minimize button action. Minimizing is purely a compositor-side
    /// visibility choice (no client protocol round-trip needed, unlike
    /// close/maximize) -- `EvoState::minimize_window` already implements
    /// this correctly via the workspace manager, it just wasn't wired up to
    /// the SSD button before now.
    fn handle_minimize<B: Backend>(&self, state: &mut EvoState<B>, window: &WindowElement) {
        state.minimize_window(window);
    }
    
    /// Handle window move action
    fn handle_move<B: Backend>(
        &self,
        seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        serial: Serial,
    ) {
        match window.0.underlying_surface() {
            WindowSurface::Wayland(w) => {
                let seat = seat.clone();
                let toplevel = w.clone();
                state
                    .handle
                    .insert_idle(move |data| data.move_request_xdg(&toplevel, &seat, serial));
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(w) => {
                let window = w.clone();
                state
                    .handle
                    .insert_idle(move |data| data.move_request_x11(&window));
            }
        };
    }
    
    /// Handle touch down event
    pub fn touch_down<B: Backend>(
        &mut self,
        seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        serial: Serial,
    ) {
        // For touch, we only handle move (not button clicks on touch down)
        match self.pointer_loc.as_ref() {
            Some(loc) if loc.x >= (self.width - BUTTON_WIDTH) as f64 => {}
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 2)) as f64 => {}
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 3)) as f64 => {}
            Some(_) => {
                self.handle_move(seat, state, window, serial);
            }
            _ => {}
        }
    }
    
    /// Handle touch up event (actual button activation)
    pub fn touch_up<B: Backend>(
        &mut self,
        _seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        _serial: Serial,
    ) {
        match self.pointer_loc.as_ref() {
            Some(loc) if loc.x >= (self.width - BUTTON_WIDTH) as f64 => {
                self.handle_close(window);
            }
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 2)) as f64 => {
                self.handle_maximize(state, window);
            }
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 3)) as f64 => {
                self.handle_minimize(state, window);
            }
            _ => {}
        }
    }
    
    /// Redraw header bar with theme support
    pub fn redraw(&mut self, width: u32) {
        if width == 0 {
            self.width = 0;
            return;
        }

        let needs_redraw_buttons = width != self.width;
        if needs_redraw_buttons {
            self.width = width;
            // Only the glass bar depends on window width -- (re)rasterize it
            // now rather than on every hover-triggered redraw.
            self.background = memory_buffer(
                width,
                HEADER_BAR_HEIGHT as u32,
                rasterize_glass_bar(width, HEADER_BAR_HEIGHT as u32, GLASS_CORNER_RADIUS, GLASS_RGBA),
            );
        }

        // === CLOSE BUTTON (rightmost) ===
        let close_hovered = self
            .pointer_loc
            .as_ref()
            .map(|l| l.x >= (width - BUTTON_WIDTH) as f64)
            .unwrap_or(false);
        if needs_redraw_buttons || close_hovered != self.close_button_hover {
            let rgb = if close_hovered { CLOSE_HOVER_RGB } else { CLOSE_RGB };
            self.close_button = memory_buffer(BUTTON_WIDTH, BUTTON_HEIGHT, rasterize_circle(BUTTON_WIDTH, rgb));
            self.close_button_hover = close_hovered;
        }

        // === MAXIMIZE BUTTON (second from right, plain colored square) ===
        let maximize_hovered = self
            .pointer_loc
            .as_ref()
            .map(|l| l.x >= (width - BUTTON_WIDTH * 2) as f64 && l.x <= (width - BUTTON_WIDTH) as f64)
            .unwrap_or(false);
        if needs_redraw_buttons || maximize_hovered != self.maximize_button_hover {
            let rgb = if maximize_hovered { MAXIMIZE_HOVER_RGB } else { MAXIMIZE_RGB };
            self.maximize_button.update(
                (BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32),
                [rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0, 1.0],
            );
            self.maximize_button_hover = maximize_hovered;
        }

        // === MINIMIZE BUTTON (third from right) ===
        let minimize_hovered = self
            .pointer_loc
            .as_ref()
            .map(|l| l.x >= (width - BUTTON_WIDTH * 3) as f64 && l.x <= (width - BUTTON_WIDTH * 2) as f64)
            .unwrap_or(false);
        if needs_redraw_buttons || minimize_hovered != self.minimize_button_hover {
            let rgb = if minimize_hovered { MINIMIZE_HOVER_RGB } else { MINIMIZE_RGB };
            self.minimize_button = memory_buffer(
                BUTTON_WIDTH,
                BUTTON_HEIGHT,
                rasterize_triangle_down(BUTTON_WIDTH, rgb),
            );
            self.minimize_button_hover = minimize_hovered;
        }
    }
}

impl Default for HeaderBar {
    fn default() -> Self {
        Self::new()
    }
}

// === RENDERING ===
impl<R> AsRenderElements<R> for HeaderBar
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
{
    type RenderElement = DecorationRenderElement<R>;

    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        renderer: &mut R,
        location: Point<i32, smithay::utils::Physical>,
        scale: smithay::utils::Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        let header_end_offset: Point<i32, Logical> = Point::from((self.width as i32, 0));
        let button_offset: Point<i32, Logical> = Point::from((BUTTON_WIDTH as i32, 0));

        let mut elements: Vec<C> = Vec::with_capacity(4);

        // Smithay's damage renderer draws render-element lists in *reverse*
        // (backend/renderer/damage/mod.rs: `render_elements.iter().rev()`),
        // and culls each element's damage against the opaque regions of
        // elements *earlier* in the list. So index 0 ends up drawn last (=
        // topmost/frontmost on screen), and anything after it in the list is
        // treated as sitting underneath it. Buttons must therefore come
        // before the background here, or the (semi-opaque) background --
        // being "in front" -- clips the buttons out entirely.

        // Close button (rightmost) -- red circle.
        let close_loc = location + (header_end_offset - button_offset).to_physical_precise_round(scale);
        if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            close_loc.to_f64(),
            &self.close_button,
            Some(alpha),
            None,
            None,
            Kind::Unspecified,
        ) {
            elements.push(DecorationRenderElement::from(el).into());
        } else {
            warn!("Failed to import close button texture");
        }

        // Maximize button (second from right) -- blue square, plain solid color.
        elements.push(
            DecorationRenderElement::from(SolidColorRenderElement::from_buffer(
                &self.maximize_button,
                location + (header_end_offset - button_offset.upscale(2)).to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::Unspecified,
            ))
            .into(),
        );

        // Minimize button (third from right) -- yellow triangle.
        let minimize_loc = location + (header_end_offset - button_offset.upscale(3)).to_physical_precise_round(scale);
        if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            minimize_loc.to_f64(),
            &self.minimize_button,
            Some(alpha),
            None,
            None,
            Kind::Unspecified,
        ) {
            elements.push(DecorationRenderElement::from(el).into());
        } else {
            warn!("Failed to import minimize button texture");
        }

        // Background (drawn last so it sits behind the buttons) -- glassy rounded bar.
        if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            location.to_f64(),
            &self.background,
            Some(alpha),
            None,
            None,
            Kind::Unspecified,
        ) {
            elements.push(DecorationRenderElement::from(el).into());
        } else {
            warn!("Failed to import header bar background texture");
        }

        elements
    }
}

// === WINDOW ELEMENT DECORATION API ===
impl WindowElement {
    /// Get or create decoration state for this window
    pub fn decoration_state(&self) -> RefMut<'_, SSDState> {
        self.user_data().insert_if_missing(|| {
            RefCell::new(SSDState {
                is_ssd: false,
                header_bar: HeaderBar::new(),
                #[cfg(feature = "decorations")]
                decoration: None,
            })
        });
        
        self.user_data()
            .get::<RefCell<SSDState>>()
            .unwrap()
            .borrow_mut()
    }
    
    /// Enable or disable server-side decorations
    pub fn set_ssd(&self, ssd: bool) {
        self.decoration_state().is_ssd = ssd;
    }
    
    /// Set decoration theme (GeckoOS enhancement)
    #[cfg(feature = "decorations")]
    pub fn set_decoration_theme(&self, theme: Theme) {
        let mut state = self.decoration_state();
        state.header_bar.set_theme(theme.clone());
        
        if let Some(decoration) = state.decoration.as_mut() {
            decoration.set_theme(theme);
        }
    }
    
    /// Initialize GeckoOS decoration metadata
    #[cfg(feature = "decorations")]
    pub fn init_decoration(&self, window_id: u32, title: String, app_id: String) {
        let mut state = self.decoration_state();
        state.decoration = Some(WindowDecoration::new(window_id, title, app_id));
        debug!("Initialized GeckoOS decoration for window {}", window_id);
    }
}
