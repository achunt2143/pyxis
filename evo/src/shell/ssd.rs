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
use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};
use std::cell::{RefCell, RefMut};
use std::sync::OnceLock;
use tracing::{debug, info, warn};

use crate::{state::Backend, EvoState};
use super::{grabs::ResizeEdge, WindowElement};

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

// === WINDOW BORDER (item #8: see overlapping windows) ===
// Evo draws no drop shadow, so a thin outer border is the only edge cue when
// windows overlap. Pure black keeps it premultiplied-safe (premult == straight
// for black), so it blends correctly whatever the renderer's alpha convention.
const BORDER_THICKNESS: i32 = 1;
const BORDER_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.55];
/// Straight-alpha byte form of BORDER_COLOR for the rasterized corner arcs
/// (0.55 * 255 ≈ 140). Black stays premultiply-safe.
const BORDER_ALPHA_U8: u8 = 140;
/// Corner-arc buffer size — matches the glass bar's top-corner radius so the
/// border follows the same rounding.
const BORDER_CORNER_SIZE: u32 = GLASS_CORNER_RADIUS as u32;

// === TITLE TEXT (item #9: show the app name in the title bar) ===
const TITLE_RGB: [u8; 3] = [0x1a, 0x1a, 0x1a];
const TITLE_PX: f32 = 14.0;
const TITLE_PAD_LEFT: u32 = 10;
const TITLE_GAP_BEFORE_BUTTONS: u32 = 10;
/// Font shipped by create-rootfs.sh's `fonts-dejavu-core` package.
const DEJAVU_SANS_PATH: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";

/// The title-bar font, loaded once from the rootfs. `None` (missing/unparseable
/// font) degrades to a title bar without text rather than failing — Evo must
/// never panic over a cosmetic decoration.
fn title_font() -> Option<&'static FontVec> {
    static FONT: OnceLock<Option<FontVec>> = OnceLock::new();
    FONT.get_or_init(|| match std::fs::read(DEJAVU_SANS_PATH) {
        Ok(bytes) => match FontVec::try_from_vec(bytes) {
            Ok(f) => Some(f),
            Err(e) => {
                warn!("SSD: failed to parse title font {}: {}", DEJAVU_SANS_PATH, e);
                None
            }
        },
        Err(e) => {
            warn!("SSD: title font {} unavailable: {}", DEJAVU_SANS_PATH, e);
            None
        }
    })
    .as_ref()
}

/// Rasterize `text` into a straight-alpha ARGB8888 buffer (matching the glass
/// bar's convention — see `put_pixel`), left-to-right, clipped to `max_width`
/// pixels (trailing glyphs are dropped rather than squeezed). Returns
/// `(width, height, pixels)` or `None` when there's no font or nothing to draw.
fn rasterize_text(text: &str, max_width: u32) -> Option<(u32, u32, Vec<u8>)> {
    let text = text.trim();
    if text.is_empty() || max_width == 0 {
        return None;
    }
    let font = title_font()?;
    let scale = PxScale::from(TITLE_PX);
    let scaled = font.as_scaled(scale);
    let ascent = scaled.ascent();
    let line_h = (scaled.ascent() - scaled.descent()).ceil().max(1.0) as u32;

    let mut positioned = Vec::new();
    let mut caret = 0.0f32;
    let mut prev = None;
    for c in text.chars() {
        let gid = font.glyph_id(c);
        if let Some(p) = prev {
            caret += scaled.kern(p, gid);
        }
        let advance = scaled.h_advance(gid);
        if !positioned.is_empty() && (caret + advance).ceil() as u32 > max_width {
            break;
        }
        positioned.push(gid.with_scale_and_position(scale, point(caret, ascent)));
        caret += advance;
        prev = Some(gid);
    }

    let width = (caret.ceil() as u32).clamp(1, max_width);
    let mut buf = vec![0u8; (width * line_h * 4) as usize];
    for glyph in positioned {
        if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px >= 0 && py >= 0 && (px as u32) < width && (py as u32) < line_h {
                    let a = (coverage * 255.0).round() as u8;
                    if a > 0 {
                        put_pixel(
                            &mut buf,
                            width,
                            px as u32,
                            py as u32,
                            [TITLE_RGB[0], TITLE_RGB[1], TITLE_RGB[2], a],
                        );
                    }
                }
            });
        }
    }
    Some((width, line_h, buf))
}

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

/// Rasterize a ~1px rounded-corner arc (transparent everywhere else) at the
/// glass bar's corner radius, so the window border follows the rounding rather
/// than squaring off the top corners. `center` is the arc's center of curvature
/// within the `size`×`size` buffer (top-left corner: `(radius, radius)`;
/// top-right: `(0, radius)`).
fn rasterize_corner(size: u32, radius: f32, center: (f32, f32), rgba: [u8; 4]) -> Vec<u8> {
    let mut buf = vec![0u8; (size * size * 4) as usize];
    // Slightly thicker than the 1px straight edges so the diagonal arc has no
    // gaps; still reads as a hairline.
    let thickness = (BORDER_THICKNESS as f32).max(1.0) + 0.75;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - center.0;
            let dy = y as f32 + 0.5 - center.1;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= radius && dist >= radius - thickness {
                put_pixel(&mut buf, size, x, y, rgba);
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

    // Full visible window height (header + client), needed to size the side
    // borders (which extend the whole window, unlike the header-only widgets).
    pub window_height: u32,
    // Rounded top corners (constant arcs); straight edges run between/below them.
    pub border_corner_tl: MemoryRenderBuffer,
    pub border_corner_tr: MemoryRenderBuffer,
    pub border_top: SolidColorBuffer,
    pub border_bottom: SolidColorBuffer,
    pub border_left: SolidColorBuffer,
    pub border_right: SolidColorBuffer,

    // Title text (app name). Re-rasterized only when the title or the space
    // available for it changes.
    pub title: String,
    pub title_buffer: Option<MemoryRenderBuffer>,
    pub title_px_w: u32,
    pub title_px_h: u32,

    // Theme support
    #[cfg(feature = "decorations")]
    pub theme: Theme,
}

// Layout constants
pub const HEADER_BAR_HEIGHT: i32 = 32;
const BUTTON_HEIGHT: u32 = HEADER_BAR_HEIGHT as u32;
const BUTTON_WIDTH: u32 = 32;

/// Depth of the invisible band around an SSD frame that starts a resize.
/// It lies outside the visible window (see `WindowElement::ssd_resize_band`).
pub const RESIZE_BAND: i32 = 8;
/// How far a corner's diagonal grab reaches along each edge. The top corners
/// are rounded at GLASS_CORNER_RADIUS, so this is the band plus that curve.
const RESIZE_CORNER: i32 = RESIZE_BAND + GLASS_CORNER_RADIUS as i32;
/// The smallest frame a resize will make: the three buttons plus a little
/// title, over a strip of client. Below about three buttons wide, the
/// button hit-tests' `width - BUTTON_WIDTH * n` had nothing left to mean.
pub const MIN_FRAME_SIZE: (i32, i32) = (BUTTON_WIDTH as i32 * 3 + 64, HEADER_BAR_HEIGHT + 48);

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
            window_height: 0,
            border_corner_tl: memory_buffer(
                BORDER_CORNER_SIZE,
                BORDER_CORNER_SIZE,
                rasterize_corner(
                    BORDER_CORNER_SIZE,
                    GLASS_CORNER_RADIUS,
                    (GLASS_CORNER_RADIUS, GLASS_CORNER_RADIUS),
                    [0, 0, 0, BORDER_ALPHA_U8],
                ),
            ),
            border_corner_tr: memory_buffer(
                BORDER_CORNER_SIZE,
                BORDER_CORNER_SIZE,
                rasterize_corner(
                    BORDER_CORNER_SIZE,
                    GLASS_CORNER_RADIUS,
                    (0.0, GLASS_CORNER_RADIUS),
                    [0, 0, 0, BORDER_ALPHA_U8],
                ),
            ),
            border_top: SolidColorBuffer::default(),
            border_bottom: SolidColorBuffer::default(),
            border_left: SolidColorBuffer::default(),
            border_right: SolidColorBuffer::default(),
            title: String::new(),
            title_buffer: None,
            title_px_w: 0,
            title_px_h: 0,
            #[cfg(feature = "decorations")]
            theme: Theme::default(),
        }
    }

    /// Set theme and trigger redraw
    #[cfg(feature = "decorations")]
    pub fn set_theme(&mut self, theme: Theme) {
        debug!("Setting header bar theme");
        self.theme = theme;
        self.redraw(self.width, self.window_height, &self.title.clone());
    }
    
    /// Update pointer location
    pub fn pointer_enter(&mut self, loc: Point<f64, Logical>) {
        self.pointer_loc = Some(loc);
    }
    
    /// Clear pointer location
    pub fn pointer_leave(&mut self) {
        self.pointer_loc = None;
    }

    /// The pointer, when it is over the header bar itself. `pointer_loc`
    /// also covers the resize band around the frame (shell/element.rs), and
    /// the button and hover tests below only mean anything inside the bar.
    /// Without this check, pressing the band just right of a window would
    /// hit-test as its close button.
    fn header_point(&self) -> Option<Point<f64, Logical>> {
        self.pointer_loc.filter(|loc| {
            loc.x >= 0.0 && loc.x < self.width as f64 && loc.y >= 0.0 && loc.y < HEADER_BAR_HEIGHT as f64
        })
    }

    /// The resize edge under the pointer, when it is in the band around the
    /// frame. `pointer_loc` is frame-relative, so the band is whatever lies
    /// outside `0..width` × `0..window_height`.
    #[cfg(feature = "decorations")]
    fn resize_edge(&self) -> Option<ResizeEdge> {
        use crate::enhancements::decorations::{resize_handle_at, ResizeHandle};

        let loc = self.pointer_loc?;
        let (x, y) = (loc.x.floor() as i32, loc.y.floor() as i32);
        let (w, h) = (self.width as i32, self.window_height as i32);
        if (0..w).contains(&x) && (0..h).contains(&y) {
            return None;
        }
        let outer = WindowGeometry {
            x: -RESIZE_BAND,
            y: -RESIZE_BAND,
            width: (w + 2 * RESIZE_BAND) as u32,
            height: (h + 2 * RESIZE_BAND) as u32,
        };
        Some(match resize_handle_at(x, y, &outer, RESIZE_BAND, RESIZE_CORNER)? {
            ResizeHandle::Top => ResizeEdge::TOP,
            ResizeHandle::Bottom => ResizeEdge::BOTTOM,
            ResizeHandle::Left => ResizeEdge::LEFT,
            ResizeHandle::Right => ResizeEdge::RIGHT,
            ResizeHandle::TopLeft => ResizeEdge::TOP_LEFT,
            ResizeHandle::TopRight => ResizeEdge::TOP_RIGHT,
            ResizeHandle::BottomLeft => ResizeEdge::BOTTOM_LEFT,
            ResizeHandle::BottomRight => ResizeEdge::BOTTOM_RIGHT,
        })
    }

    #[cfg(not(feature = "decorations"))]
    fn resize_edge(&self) -> Option<ResizeEdge> {
        None
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
        // The band around the frame first. It is outside the header, so it
        // never competes with the buttons, but it must win over the header
        // fallbacks below, which read only `loc.x`.
        if let Some(edges) = self.resize_edge() {
            info!("SSD resize band press: edges={:?}", edges);
            self.handle_resize(seat, state, window, serial, edges);
            return;
        }
        // Diagnostic: which button a titlebar click resolves to, with the
        // coordinates that decided it. If "maximize closes the app" is really
        // a hit-test overshoot (the maximize/close buttons sit adjacent at the
        // screen corner once maximized), this log shows "close" firing when
        // the user aimed for maximize.
        info!(
            "SSD titlebar click: loc_x={:?} width={} (close>={}, max>={}, min>={})",
            self.pointer_loc.as_ref().map(|l| l.x),
            self.width,
            self.width.saturating_sub(BUTTON_WIDTH),
            self.width.saturating_sub(BUTTON_WIDTH * 2),
            self.width.saturating_sub(BUTTON_WIDTH * 3),
        );
        match self.header_point().as_ref() {
            // Close button (rightmost)
            Some(loc) if loc.x >= self.width.saturating_sub(BUTTON_WIDTH) as f64 => {
                info!("Close button clicked");
                self.handle_close(window);
            }

            // Maximize button (second from right)
            Some(loc) if loc.x >= self.width.saturating_sub(BUTTON_WIDTH * 2) as f64 => {
                info!("Maximize button clicked");
                self.handle_maximize(state, window);
            }

            // Minimize button (third from right)
            Some(loc) if loc.x >= self.width.saturating_sub(BUTTON_WIDTH * 3) as f64 => {
                info!("Minimize button clicked");
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
                // Defer to an idle callback (matching the X11 branch below and
                // `handle_move`) instead of calling (un)maximize_request
                // inline. This handler runs while the SSD button click is
                // STILL holding a RefMut borrow of this window's
                // decoration_state -- see element.rs's
                // `PointerTarget::button` -> `HeaderBar::clicked` ->
                // here -- and `maximize_request`/`unmaximize_request` call
                // `Space::map_element`, which re-reads the SAME
                // decoration_state through `SpaceElement::geometry`/`bbox`
                // (both do `self.decoration_state()`, i.e. `borrow_mut()`).
                // Two live borrows of one RefCell => `already borrowed:
                // BorrowMutError` panic that took down the whole compositor
                // on every maximize click (a Rust panic, which is why no
                // segfault ever showed in dmesg). `insert_idle` runs the
                // request after input dispatch returns and the borrow is
                // released. Minimize/close don't hit this: neither re-borrows
                // decoration_state.
                let is_maximized =
                    w.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Maximized));
                info!(
                    "SSD maximize button: pending_maximized={} -> deferring {}",
                    is_maximized,
                    if is_maximized { "unmaximize_request" } else { "maximize_request" }
                );
                let surface = w.clone();
                state.handle.insert_idle(move |data| {
                    if is_maximized {
                        data.unmaximize_request(surface);
                    } else {
                        data.maximize_request(surface);
                    }
                });
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
    
    /// Start an interactive resize from a press in the band around the
    /// frame. Deferred to an idle callback for the same reason as
    /// `handle_move` and `handle_maximize`: this runs inside the SSD click's
    /// borrow of `decoration_state`, and setting up the grab reads the
    /// window's geometry, which borrows it again.
    ///
    /// This is what #11 was missing. vela negotiates server-side decoration,
    /// so the frame being dragged is Evo's own, and no client ever sends the
    /// `xdg_toplevel.resize` that the xdg path waits for.
    fn handle_resize<B: Backend>(
        &self,
        seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        serial: Serial,
        edges: ResizeEdge,
    ) {
        if let WindowSurface::Wayland(w) = window.0.underlying_surface() {
            let seat = seat.clone();
            let toplevel = w.clone();
            state
                .handle
                .insert_idle(move |data| data.resize_request_xdg(&toplevel, &seat, serial, edges));
        }
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
        // For touch, we only handle move (not button clicks on touch down).
        // The resize band is pointer-only: a touch there does nothing.
        match self.header_point().as_ref() {
            Some(loc) if loc.x >= self.width.saturating_sub(BUTTON_WIDTH) as f64 => {}
            Some(loc) if loc.x >= self.width.saturating_sub(BUTTON_WIDTH * 2) as f64 => {}
            Some(loc) if loc.x >= self.width.saturating_sub(BUTTON_WIDTH * 3) as f64 => {}
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
        match self.header_point().as_ref() {
            Some(loc) if loc.x >= self.width.saturating_sub(BUTTON_WIDTH) as f64 => {
                self.handle_close(window);
            }
            Some(loc) if loc.x >= self.width.saturating_sub(BUTTON_WIDTH * 2) as f64 => {
                self.handle_maximize(state, window);
            }
            Some(loc) if loc.x >= self.width.saturating_sub(BUTTON_WIDTH * 3) as f64 => {
                self.handle_minimize(state, window);
            }
            _ => {}
        }
    }
    
    /// Redraw header bar with theme support
    pub fn redraw(&mut self, width: u32, window_height: u32, title: &str) {
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

        // === BORDERS === sized to the full window (width x window_height, and
        // the thin side strips down the whole height). Re-created only on a
        // real size change so damage tracking sees stable buffers otherwise.
        if needs_redraw_buttons || window_height != self.window_height {
            self.window_height = window_height;
            let r = GLASS_CORNER_RADIUS as i32;
            let w = width as i32;
            let h = window_height.max(1) as i32;
            // Top edge runs BETWEEN the two rounded top corners; the side edges
            // start BELOW them. The bottom corners are square (the client
            // content isn't rounded there), so the bottom edge spans the full
            // width and the sides run all the way down.
            self.border_top
                .update(((w - 2 * r).max(0), BORDER_THICKNESS), BORDER_COLOR);
            self.border_bottom.update((w, BORDER_THICKNESS), BORDER_COLOR);
            self.border_left
                .update((BORDER_THICKNESS, (h - r).max(0)), BORDER_COLOR);
            self.border_right
                .update((BORDER_THICKNESS, (h - r).max(0)), BORDER_COLOR);
        }

        // === TITLE === re-rasterize only when the text or the space available
        // for it (window width minus the three right-hand buttons) changes.
        if needs_redraw_buttons || title != self.title {
            self.title = title.to_string();
            let reserved = BUTTON_WIDTH * 3 + TITLE_PAD_LEFT + TITLE_GAP_BEFORE_BUTTONS;
            let avail = width.saturating_sub(reserved);
            match rasterize_text(&self.title, avail) {
                Some((tw, th, px)) => {
                    self.title_px_w = tw;
                    self.title_px_h = th;
                    self.title_buffer = Some(memory_buffer(tw, th, px));
                }
                None => {
                    self.title_buffer = None;
                    self.title_px_w = 0;
                    self.title_px_h = 0;
                }
            }
        }

        // === CLOSE BUTTON (rightmost) ===
        let close_hovered = self
            .header_point()
            .map(|l| l.x >= width.saturating_sub(BUTTON_WIDTH) as f64)
            .unwrap_or(false);
        if needs_redraw_buttons || close_hovered != self.close_button_hover {
            let rgb = if close_hovered { CLOSE_HOVER_RGB } else { CLOSE_RGB };
            self.close_button = memory_buffer(BUTTON_WIDTH, BUTTON_HEIGHT, rasterize_circle(BUTTON_WIDTH, rgb));
            self.close_button_hover = close_hovered;
        }

        // === MAXIMIZE BUTTON (second from right, plain colored square) ===
        let maximize_hovered = self
            .header_point()
            .map(|l| {
                l.x >= width.saturating_sub(BUTTON_WIDTH * 2) as f64
                    && l.x <= width.saturating_sub(BUTTON_WIDTH) as f64
            })
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
            .header_point()
            .map(|l| {
                l.x >= width.saturating_sub(BUTTON_WIDTH * 3) as f64
                    && l.x <= width.saturating_sub(BUTTON_WIDTH * 2) as f64
            })
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

        let mut elements: Vec<C> = Vec::with_capacity(12);

        // --- Window border (item #8). Pushed first == frontmost (see the
        // reverse-draw note below), so it frames over the client's own edge
        // pixels. The side/bottom borders run the FULL window height, past the
        // header, which is why the HeaderBar tracks window_height. Rounded top
        // corners follow the glass bar's radius; straight edges run
        // between/below them.
        let full_h = self.window_height.max(HEADER_BAR_HEIGHT as u32) as i32;
        let w_i = self.width as i32;
        let r_i = GLASS_CORNER_RADIUS as i32;
        for (buf, off) in [
            (&self.border_top, Point::<i32, Logical>::from((r_i, 0))),
            (&self.border_bottom, Point::from((0, full_h - BORDER_THICKNESS))),
            (&self.border_left, Point::from((0, r_i))),
            (&self.border_right, Point::from((w_i - BORDER_THICKNESS, r_i))),
        ] {
            elements.push(
                DecorationRenderElement::from(SolidColorRenderElement::from_buffer(
                    buf,
                    location + off.to_physical_precise_round(scale),
                    scale,
                    alpha,
                    Kind::Unspecified,
                ))
                .into(),
            );
        }
        // Rounded top corners (arcs matching the glass radius).
        for (corner, off) in [
            (&self.border_corner_tl, Point::<i32, Logical>::from((0, 0))),
            (&self.border_corner_tr, Point::from((w_i - r_i, 0))),
        ] {
            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (location + off.to_physical_precise_round(scale)).to_f64(),
                corner,
                Some(alpha),
                None,
                None,
                Kind::Unspecified,
            ) {
                elements.push(DecorationRenderElement::from(el).into());
            } else {
                warn!("Failed to import border corner texture");
            }
        }

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

        // Title text (item #9) -- over the glass, left-aligned, vertically
        // centered. Pushed before the background so the (semi-opaque) glass
        // doesn't clip it, same as the buttons.
        if let Some(title_buf) = self.title_buffer.as_ref() {
            let ty = (HEADER_BAR_HEIGHT as u32).saturating_sub(self.title_px_h) as i32 / 2;
            let title_off: Point<i32, Logical> = Point::from((TITLE_PAD_LEFT as i32, ty));
            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (location + title_off.to_physical_precise_round(scale)).to_f64(),
                title_buf,
                Some(alpha),
                None,
                None,
                Kind::Unspecified,
            ) {
                elements.push(DecorationRenderElement::from(el).into());
            } else {
                warn!("Failed to import title text texture");
            }
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
