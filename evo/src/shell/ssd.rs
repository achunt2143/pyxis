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

// === PYXIS WINDOW CONTROLS ===
// The remochi spinner's shapes, colours and outlines (shell/identity.rs):
// circle (close), square (maximize), downward triangle (minimize), right to
// left, each 14 px centred in its 32 px target. At rest, controls sit as
// etched monochrome outlines (dark ink when focused, quiet ink when inactive);
// hover reveals the Mochi spinner colour; a busy window runs the spinner in
// the three slots (shell/busy.rs).
use super::identity::{
    BarColors, Icon, Radii, TitleLabel, SPINNER_BLUE, SPINNER_BLUE_OUTLINE, SPINNER_RED, SPINNER_RED_OUTLINE,
    SPINNER_YELLOW, SPINNER_YELLOW_OUTLINE,
};
use super::busy::{self, Shape};

/// The spinner's outline, and the inactive outline (a little heavier, as it
/// carries the shape alone).
const OUTLINE_PX: f32 = 1.2;
const INACTIVE_OUTLINE_PX: f32 = 1.5;
/// The frame's corners (the waterdrop), logical px.
const FRAME_RADII: Radii = crate::anim::depth::FRAME_RADII;

// === WINDOW BORDER (item #8: see overlapping windows) ===
// A thin outer border, over the drop shadow (`anim::depth`) that gives windows
// their depth, so the edge stays crisp where a light window meets a light one.
// Pure black keeps it premultiplied-safe (premult == straight
// for black), so it blends correctly whatever the renderer's alpha convention.
const BORDER_THICKNESS: i32 = 1;
const BORDER_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.55];
/// Straight-alpha byte form of BORDER_COLOR for the rasterized corner arcs
/// (0.55 * 255 ≈ 140). Black stays premultiply-safe.
const BORDER_ALPHA_U8: u8 = 140;

// === TITLE (item #9: show the app name in the title bar) ===
const TITLE_PX: f32 = 13.0;
const TITLE_PAD_LEFT: u32 = 10;
const TITLE_GAP_BEFORE_BUTTONS: u32 = 10;
/// Between the icon and the name, and around the dot before the document.
const ICON_GAP: u32 = 8;
/// Fonts shipped by create-rootfs.sh's `fonts-dejavu-core` package.
const DEJAVU_SANS_PATH: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";
const DEJAVU_SANS_BOLD_PATH: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf";

fn load_font(path: &str) -> Option<FontVec> {
    match std::fs::read(path) {
        Ok(bytes) => match FontVec::try_from_vec(bytes) {
            Ok(f) => Some(f),
            Err(e) => {
                warn!("SSD: failed to parse title font {}: {}", path, e);
                None
            }
        },
        Err(e) => {
            warn!("SSD: title font {} unavailable: {}", path, e);
            None
        }
    }
}

/// The title-bar font, loaded once from the rootfs. `None` (missing/unparseable
/// font) degrades to a title bar without text rather than failing — Evo must
/// never panic over a cosmetic decoration.
fn title_font() -> Option<&'static FontVec> {
    static FONT: OnceLock<Option<FontVec>> = OnceLock::new();
    FONT.get_or_init(|| load_font(DEJAVU_SANS_PATH)).as_ref()
}

/// The app name's font: bold, or the regular one when bold is missing.
fn name_font() -> Option<&'static FontVec> {
    static FONT: OnceLock<Option<FontVec>> = OnceLock::new();
    FONT.get_or_init(|| load_font(DEJAVU_SANS_BOLD_PATH)).as_ref().or_else(title_font)
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

/// Lays `rgba` (straight alpha, its alpha times `cover`) over the pixel at
/// `(x, y)`: source-over, so shapes and text can overlap cleanly.
fn blend_pixel(buf: &mut [u8], stride: u32, x: u32, y: u32, rgba: [u8; 4], cover: f32) {
    let sa = rgba[3] as f32 / 255.0 * cover.clamp(0.0, 1.0);
    if sa <= 0.0 {
        return;
    }
    let idx = ((y * stride + x) * 4) as usize;
    let da = buf[idx + 3] as f32 / 255.0;
    let oa = sa + da * (1.0 - sa);
    let mix = |s: u8, d: u8| ((s as f32 * sa + d as f32 * da * (1.0 - sa)) / oa).round().clamp(0.0, 255.0) as u8;
    let (b, g, r) = (buf[idx], buf[idx + 1], buf[idx + 2]);
    buf[idx] = mix(rgba[2], b);
    buf[idx + 1] = mix(rgba[1], g);
    buf[idx + 2] = mix(rgba[0], r);
    buf[idx + 3] = (oa * 255.0).round() as u8;
}

/// Draws `text` into `buf` (`stride`×`height`) from `x`, vertically centred,
/// stopping before a glyph would cross `max_x` (trailing glyphs are dropped
/// rather than squeezed). Returns where the caret ended.
fn draw_text(buf: &mut [u8], stride: u32, height: u32, x: f32, text: &str, font: &FontVec, rgba: [u8; 4], max_x: u32) -> f32 {
    let scale = PxScale::from(TITLE_PX);
    let scaled = font.as_scaled(scale);
    let line_h = scaled.ascent() - scaled.descent();
    let baseline = ((height as f32 - line_h) / 2.0 + scaled.ascent()).round();
    let mut caret = x;
    let mut prev = None;
    for c in text.chars() {
        let gid = font.glyph_id(c);
        if let Some(p) = prev {
            caret += scaled.kern(p, gid);
        }
        let advance = scaled.h_advance(gid);
        if (caret + advance).ceil() as u32 > max_x {
            break;
        }
        if let Some(outline) = font.outline_glyph(gid.with_scale_and_position(scale, point(caret, baseline))) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px >= 0 && py >= 0 && (px as u32) < stride && (py as u32) < height {
                    blend_pixel(buf, stride, px as u32, py as u32, rgba, coverage);
                }
            });
        }
        caret += advance;
        prev = Some(gid);
    }
    caret
}

/// The width `text` takes in `font`, px.
fn text_width(text: &str, font: &FontVec) -> f32 {
    let scaled = font.as_scaled(PxScale::from(TITLE_PX));
    let mut w = 0.0;
    let mut prev = None;
    for c in text.chars() {
        let gid = font.glyph_id(c);
        if let Some(p) = prev {
            w += scaled.kern(p, gid);
        }
        w += scaled.h_advance(gid);
        prev = Some(gid);
    }
    w
}

/// What the title bar's label shows, and in what colours.
#[derive(Clone, Debug, PartialEq)]
pub struct TitleSource {
    pub label: TitleLabel,
    pub icon: Option<std::sync::Arc<Icon>>,
    pub colors: BarColors,
}

impl Default for TitleSource {
    fn default() -> Self {
        Self {
            label: TitleLabel::default(),
            icon: None,
            colors: super::identity::bar_colors(None, false, super::identity::material(false)),
        }
    }
}

/// Rasterize the label (icon, bold name, quiet document title; or the plain
/// title) into a straight-alpha buffer `HEADER_BAR_HEIGHT` tall, at most
/// `max_width` wide. `None` when there is nothing to draw.
fn rasterize_label(src: &TitleSource, max_width: u32) -> Option<(u32, u32, Vec<u8>)> {
    if max_width == 0 {
        return None;
    }
    let height = HEADER_BAR_HEIGHT as u32;
    let ink = src.colors.ink;
    let quiet = src.colors.quiet;
    // How wide it will be, so the buffer is no wider than its contents.
    let (width, plain) = match &src.label.name {
        Some(name) => {
            let mut w = 0.0;
            if src.icon.is_some() {
                w += (super::identity::ICON_PX + ICON_GAP) as f32;
            }
            w += name_font().map_or(0.0, |f| text_width(name, f));
            if let Some(doc) = &src.label.doc {
                w += title_font().map_or(0.0, |f| text_width(&format!("  ·  {doc}"), f));
            }
            (w, false)
        }
        None => {
            let t = src.label.plain.trim();
            if t.is_empty() {
                return None;
            }
            (title_font().map_or(0.0, |f| text_width(t, f)), true)
        }
    };
    let width = (width.ceil() as u32).clamp(1, max_width);
    let mut buf = vec![0u8; (width * height * 4) as usize];
    if plain {
        let font = title_font()?;
        draw_text(&mut buf, width, height, 0.0, src.label.plain.trim(), font, ink, width);
        return Some((width, height, buf));
    }
    let mut x = 0.0;
    if let Some(icon) = &src.icon {
        let top = (height - icon.size) / 2;
        for iy in 0..icon.size {
            for ix in 0..icon.size {
                if ix >= width {
                    break;
                }
                let i = ((iy * icon.size + ix) * 4) as usize;
                let px = [icon.rgba[i], icon.rgba[i + 1], icon.rgba[i + 2], icon.rgba[i + 3]];
                blend_pixel(&mut buf, width, ix, top + iy, px, 1.0);
            }
        }
        x += (icon.size + ICON_GAP) as f32;
    }
    if let (Some(name), Some(font)) = (&src.label.name, name_font()) {
        x = draw_text(&mut buf, width, height, x, name, font, ink, width);
    }
    if let (Some(doc), Some(font)) = (&src.label.doc, title_font()) {
        draw_text(&mut buf, width, height, x, &format!("  ·  {doc}"), font, quiet, width);
    }
    Some((width, height, buf))
}

// === The controls ===

/// One shape drawn into a control's 32 px target.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layer {
    shape: Shape,
    fill: Option<[u8; 3]>,
    stroke: [u8; 4],
    stroke_px: f32,
    scale: f32,
    /// Degrees; negative is anticlockwise.
    turn: f32,
    alpha: f32,
}

fn fill_of(shape: Shape) -> [u8; 3] {
    match shape {
        Shape::Circle => SPINNER_RED,
        Shape::Square => SPINNER_BLUE,
        Shape::Triangle => SPINNER_YELLOW,
    }
}

fn outline_of(shape: Shape) -> [u8; 4] {
    let c = match shape {
        Shape::Circle => SPINNER_RED_OUTLINE,
        Shape::Square => SPINNER_BLUE_OUTLINE,
        Shape::Triangle => SPINNER_YELLOW_OUTLINE,
    };
    [c[0], c[1], c[2], 0xff]
}

/// The control in slot `slot`: 0 minimize (left), 1 maximize, 2 close.
fn slot_shape(slot: usize) -> Shape {
    match slot {
        0 => Shape::Triangle,
        1 => Shape::Square,
        _ => Shape::Circle,
    }
}

/// How a control looks this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ControlState {
    /// 0 inactive .. 1 focused (the focus crossfade).
    focus: f32,
    hover: bool,
    busy: bool,
    mode: crate::anim::Mode,
    /// The spinner's phase, 0..1.
    phase: f64,
    /// The title's quiet ink, for inactive outlines.
    quiet: [u8; 4],
}

/// The shapes slot `slot` draws in `state`. A hovered slot always shows its
/// own control, so what you are about to press is never mid-morph.
fn control_layers(slot: usize, state: &ControlState) -> Vec<Layer> {
    let own = slot_shape(slot);
    let filled = |shape: Shape, fill: [u8; 3], alpha: f32| Layer {
        shape,
        fill: Some(fill),
        stroke: outline_of(shape),
        stroke_px: OUTLINE_PX,
        scale: 1.0,
        turn: 0.0,
        alpha,
    };
    let outlined = |shape: Shape, stroke: [u8; 4], alpha: f32| Layer {
        shape,
        fill: None,
        stroke,
        stroke_px: INACTIVE_OUTLINE_PX,
        scale: 1.0,
        turn: 0.0,
        alpha,
    };
    if state.busy && !state.hover {
        return vec![match state.mode {
            crate::anim::Mode::Full => {
                let m = busy::morph(slot, state.phase);
                Layer {
                    scale: m.scale as f32,
                    turn: m.turn as f32,
                    ..filled(m.shape, fill_of(m.shape), m.alpha as f32)
                }
            }
            // Reduced: each slot blinks its own colour in place.
            crate::anim::Mode::Reduced if busy::blink_on(slot, state.phase) => filled(own, fill_of(own), 1.0),
            // Off (and reduced between blinks): the shapes' own outlines,
            // which an inactive window (quiet ink) never shows.
            _ => outlined(own, outline_of(own), 1.0),
        }];
    }
    if state.hover {
        return vec![filled(own, fill_of(own), 1.0)];
    }
    let f = state.focus.clamp(0.0, 1.0);
    let active_stroke = [state.quiet[0], state.quiet[1], state.quiet[2], 0xd8];
    let mut layers = Vec::with_capacity(2);
    if f < 0.99 {
        layers.push(outlined(own, state.quiet, 1.0 - f));
    }
    if f > 0.01 {
        layers.push(outlined(own, active_stroke, f));
    }
    layers
}

/// Signed distance from `p` to `shape` in a 32 px target, px, negative
/// inside. The mockup's geometry: a 14 px circle and rounded square centred
/// in the target, a 14 px wide downward triangle.
fn shape_sdf(shape: Shape, p: (f32, f32)) -> f32 {
    match shape {
        Shape::Circle => ((p.0 - 16.0).powi(2) + (p.1 - 16.0).powi(2)).sqrt() - 7.0,
        Shape::Square => {
            let r = 1.5;
            let qx = (p.0 - 16.0).abs() - (7.0 - r);
            let qy = (p.1 - 16.0).abs() - (7.0 - r);
            (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
        }
        Shape::Triangle => triangle_sdf(p, (9.0, 11.5), (23.0, 11.5), (16.0, 23.0)),
    }
}

/// The point a shape turns and scales about: the centre of its box.
fn shape_center(shape: Shape) -> (f32, f32) {
    match shape {
        Shape::Triangle => (16.0, 17.25),
        _ => (16.0, 16.0),
    }
}

/// Exact signed distance to a triangle (Inigo Quilez's `sdTriangle`).
fn triangle_sdf(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    let sub = |u: (f32, f32), v: (f32, f32)| (u.0 - v.0, u.1 - v.1);
    let dot = |u: (f32, f32), v: (f32, f32)| u.0 * v.0 + u.1 * v.1;
    let (e0, e1, e2) = (sub(b, a), sub(c, b), sub(a, c));
    let (v0, v1, v2) = (sub(p, a), sub(p, b), sub(p, c));
    let seg = |v: (f32, f32), e: (f32, f32)| {
        let t = (dot(v, e) / dot(e, e)).clamp(0.0, 1.0);
        (v.0 - e.0 * t, v.1 - e.1 * t)
    };
    let (pq0, pq1, pq2) = (seg(v0, e0), seg(v1, e1), seg(v2, e2));
    let s = (e0.0 * e2.1 - e0.1 * e2.0).signum();
    let cand = |pq: (f32, f32), v: (f32, f32), e: (f32, f32)| (dot(pq, pq), s * (v.0 * e.1 - v.1 * e.0));
    let (a0, b0) = cand(pq0, v0, e0);
    let (a1, b1) = cand(pq1, v1, e1);
    let (a2, b2) = cand(pq2, v2, e2);
    let d = a0.min(a1).min(a2);
    let side = b0.min(b1).min(b2);
    -d.sqrt() * side.signum()
}

/// Rasterize a control's layers into a `BUTTON_WIDTH`² straight-alpha buffer.
fn rasterize_control(layers: &[Layer]) -> Vec<u8> {
    let size = BUTTON_WIDTH;
    let mut buf = vec![0u8; (size * size * 4) as usize];
    for layer in layers {
        if layer.alpha <= 0.0 {
            continue;
        }
        let c = shape_center(layer.shape);
        let (sin, cos) = (-layer.turn.to_radians()).sin_cos();
        let scale = layer.scale.max(0.05);
        for y in 0..size {
            for x in 0..size {
                // Into the shape's own frame: unturn, unscale about its centre.
                let (dx, dy) = (x as f32 + 0.5 - c.0, y as f32 + 0.5 - c.1);
                let (rx, ry) = (dx * cos - dy * sin, dx * sin + dy * cos);
                let p = (c.0 + rx / scale, c.1 + ry / scale);
                let d = shape_sdf(layer.shape, p) * scale;
                if let Some(fill) = layer.fill {
                    let cover = (0.5 - d).clamp(0.0, 1.0);
                    blend_pixel(&mut buf, size, x, y, [fill[0], fill[1], fill[2], 0xff], cover * layer.alpha);
                }
                let half = layer.stroke_px * scale.max(0.6) / 2.0;
                let cover = (half + 0.5 - d.abs()).clamp(0.0, 1.0);
                blend_pixel(&mut buf, size, x, y, layer.stroke, cover * layer.alpha);
            }
        }
    }
    buf
}

// === The frame ===

/// Rasterizes a "glass" bar with its top corners rounded at `top_left` and
/// `top_right` (the bottom edge meets the client area, so it stays square),
/// with an anti-aliased edge.
fn rasterize_glass_bar(width: u32, height: u32, top_left: f32, top_right: f32, rgba: [u8; 4]) -> Vec<u8> {
    let mut buf = vec![0u8; (width * height * 4) as usize];
    let limit = (width as f32 / 2.0).min(height as f32);
    let (tl, tr) = (top_left.min(limit), top_right.min(limit));
    for y in 0..height {
        for x in 0..width {
            let xf = x as f32 + 0.5;
            let yf = y as f32 + 0.5;
            let corner = |cx: f32, cy: f32, r: f32| {
                let d = ((xf - cx).powi(2) + (yf - cy).powi(2)).sqrt();
                (r - d + 0.5).clamp(0.0, 1.0)
            };
            let cover = if yf < tl && xf < tl {
                corner(tl, tl, tl)
            } else if yf < tr && xf > width as f32 - tr {
                corner(width as f32 - tr, tr, tr)
            } else {
                1.0
            };
            if cover > 0.0 {
                blend_pixel(&mut buf, width, x, y, rgba, cover);
            }
        }
    }
    buf
}

/// What the flat bar (F0) was last drawn from; it is redrawn when any of it
/// changes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FlatKey {
    width: u32,
    shape: Radii,
    glass: [u8; 4],
    glass_inactive: [u8; 4],
    accent: [u8; 3],
    dark: bool,
    /// The focus level in twentieths: the crossfade is stepped, not smooth.
    focus: u8,
}

/// The flat bar: one colour (the glass, mixed from its inactive to its active
/// level by `focus`), the rim of light along its top and the seam of the
/// accent along its bottom, in the waterdrop's top corners. This is what a bar
/// is when there is nothing to frost, so it keeps the frosted bar's edges.
fn rasterize_flat_bar(key: &FlatKey, height: u32) -> Vec<u8> {
    let f = key.focus as f32 / 20.0;
    let mixed = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
    let fill = [
        mixed(key.glass_inactive[0], key.glass[0]),
        mixed(key.glass_inactive[1], key.glass[1]),
        mixed(key.glass_inactive[2], key.glass[2]),
        mixed(key.glass_inactive[3], key.glass[3]),
    ];
    let mut buf = rasterize_glass_bar(key.width, height, key.shape.top_left() as f32, key.shape.top_right() as f32, fill);
    let seam = super::frost::SEAM_INACTIVE + (super::frost::SEAM - super::frost::SEAM_INACTIVE) * f;
    let rim = [255, 255, 255, (super::frost::rim_alpha(key.dark) * 255.0).round() as u8];
    let seam = [key.accent[0], key.accent[1], key.accent[2], (seam * 255.0).round() as u8];
    for x in 0..key.width {
        // Corners that are cut leave the edge rows partly or wholly empty.
        for (y, edge) in [(0, rim), (height - 1, seam)] {
            let cover = alpha_of(&buf, key.width, x, y) as f32 / fill[3].max(1) as f32;
            blend_pixel(&mut buf, key.width, x, y, edge, cover.min(1.0));
        }
    }
    buf
}

fn alpha_of(buf: &[u8], stride: u32, x: u32, y: u32) -> u8 {
    buf[((y * stride + x) * 4 + 3) as usize]
}

/// Rasterize a ~1px rounded-corner arc (transparent everywhere else) of
/// `radius`, so the window border follows the frame's rounding rather than
/// squaring off its corners. `center` is the arc's center of curvature within
/// the `size`×`size` buffer (top-left corner: `(radius, radius)`; top-right:
/// `(0, radius)`).
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

/// The four border arcs for `radii` (top-left, top-right, bottom-right,
/// bottom-left), each a buffer its corner's radius square. A square corner
/// gets an empty 1 px buffer, which is never drawn.
fn corner_buffers(radii: Radii) -> [MemoryRenderBuffer; 4] {
    let arc = |r: f64, center: fn(f32) -> (f32, f32)| {
        let size = r.round().max(1.0) as u32;
        let r = r as f32;
        memory_buffer(size, size, rasterize_corner(size, r, center(r), [0, 0, 0, BORDER_ALPHA_U8]))
    };
    [
        arc(radii.top_left(), |r| (r, r)),
        arc(radii.top_right(), |r| (0.0, r)),
        arc(radii.bottom_right(), |_| (0.0, 0.0)),
        arc(radii.bottom_left(), |r| (r, 0.0)),
    ]
}

/// Straight alpha (how everything here is rasterized) to premultiplied, which
/// is what the renderer blends textures as. With binary edges it made no
/// difference; anti-aliased ones drawn straight showed as a bright fringe.
fn premultiply(mut pixels: Vec<u8>) -> Vec<u8> {
    for px in pixels.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
    pixels
}

fn memory_buffer(width: u32, height: u32, pixels: Vec<u8>) -> MemoryRenderBuffer {
    let pixels = premultiply(pixels);
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
    ControlButton, Theme, WindowDecoration, WindowGeometry,
};

/// Window decoration state stored in window user data
pub struct SSDState {
    pub header_bar: HeaderBar,
    
    // GeckoOS decoration metadata (themes, hit testing, etc.)
    #[cfg(feature = "decorations")]
    pub decoration: Option<WindowDecoration>,
}

/// The decoration answers that geometry and hit-testing need. See
/// `WindowElement::is_ssd`.
#[derive(Default)]
struct SsdFlags {
    negotiated: std::cell::Cell<bool>,
    suppressed: std::cell::Cell<bool>,
}

/// Header bar rendering state
#[derive(Debug, Clone)]
pub struct HeaderBar {
    pub pointer_loc: Option<Point<f64, Logical>>,
    pub width: u32,
    pub close_button_hover: bool,
    pub maximize_button_hover: bool,
    pub minimize_button_hover: bool,  // NEW: Added minimize button support

    // Rendering buffers: the glass, and the three controls (each its whole
    // 32 px target, the shape drawn centred in it).
    pub background: MemoryRenderBuffer,
    pub close_button: MemoryRenderBuffer,
    pub maximize_button: MemoryRenderBuffer,
    pub minimize_button: MemoryRenderBuffer,
    /// What each control (minimize, maximize, close) was last drawn as.
    drawn_controls: [Option<Vec<Layer>>; 3],

    // Full visible window height (header + client), needed to size the side
    // borders (which extend the whole window, unlike the header-only widgets).
    pub window_height: u32,
    // Rounded corners (arcs at each corner's own radius); straight edges run
    // between them. Index: top-left, top-right, bottom-right, bottom-left.
    border_corners: [MemoryRenderBuffer; 4],
    /// Maximized or fullscreen: square corners all round. Set by the caller
    /// before `redraw`, which reshapes the frame when it changes.
    pub square: bool,
    /// Whether the client content is clipped to round bottom corners. Without
    /// it (depth off, or the shaders failed) the bottom stays square, as it
    /// was before depth, and the border follows.
    pub bottom_round: bool,
    /// The corners `redraw` last shaped the frame with.
    shaped: Radii,
    /// 0 (inactive) ..= 1 (active): how bright the glass and title are, and
    /// how far the controls have gone from outlines to their colours.
    pub focus: f32,
    /// The app has stopped answering pings: the controls run the spinner.
    pub busy: bool,
    pub border_top: SolidColorBuffer,
    pub border_bottom: SolidColorBuffer,
    pub border_left: SolidColorBuffer,
    pub border_right: SolidColorBuffer,

    // The label (icon, app name, document title). Re-rasterized only when
    // what it says or the space available for it changes.
    pub title: TitleSource,
    pub title_buffer: Option<MemoryRenderBuffer>,
    pub title_px_w: u32,
    pub title_px_h: u32,
    /// The flat bar's glass (F0) was last drawn from this; `None` while the
    /// bar is frosted (the `FrostElement` draws it) or before the first draw.
    drawn_flat: Option<FlatKey>,
    /// Frost draws the glass (`shell::frost`, F1): the header draws none.
    /// Set by the caller before `redraw`.
    pub frosted: bool,

    // Theme support
    #[cfg(feature = "decorations")]
    pub theme: Theme,
    /// Last left-click timestamp and frame-relative location on the titlebar
    /// drag region, used for double-click maximize/restore detection.
    last_titlebar_click: Option<(u32, Point<f64, Logical>)>,
}

// Layout constants
pub const HEADER_BAR_HEIGHT: i32 = 32;
pub(crate) const BTN_LEFT: u32 = 0x110;
pub(crate) const BTN_MIDDLE: u32 = 0x112;
const DOUBLE_CLICK_MS: u32 = 400;
const DOUBLE_CLICK_DIST_SQ: f64 = 36.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TitlebarAction {
    StartResize(ResizeEdge),
    Close,
    MaximizeToggle,
    Minimize,
    Lower,
    StartMove,
}
const BUTTON_HEIGHT: u32 = HEADER_BAR_HEIGHT as u32;
const BUTTON_WIDTH: u32 = 32;

/// Depth of the invisible band around an SSD frame that starts a resize.
/// It lies outside the visible window (see `WindowElement::ssd_resize_band`).
pub const RESIZE_BAND: i32 = 8;
/// How far a corner's diagonal grab reaches along each edge: the band plus
/// the frame's largest corner curve.
const RESIZE_CORNER: i32 = RESIZE_BAND + FRAME_RADII.0[0] as i32;
/// The smallest frame a resize will make: the three buttons plus a little
/// title, over a strip of client. Below about three buttons wide, the
/// button hit-tests' `width - BUTTON_WIDTH * n` had nothing left to mean.
pub const MIN_FRAME_SIZE: (i32, i32) = (BUTTON_WIDTH as i32 * 3 + 64, HEADER_BAR_HEIGHT + 48);

/// The corners a frame is shaped with: square when it fills its space, the
/// waterdrop otherwise, its bottom corners square when the client content
/// isn't clipped to match.
pub fn frame_shape(square: bool, bottom_round: bool) -> Radii {
    if square {
        return Radii::SQUARE;
    }
    let mut r = FRAME_RADII;
    if !bottom_round {
        r.0[2] = 0.0;
        r.0[3] = 0.0;
    }
    r
}

impl HeaderBar {
    /// Create a new header bar with default or themed colors
    pub fn new() -> Self {
        let shaped = frame_shape(false, true);
        Self {
            pointer_loc: None,
            width: 0,
            close_button_hover: false,
            maximize_button_hover: false,
            minimize_button_hover: false,
            // Drawn by the first redraw().
            background: memory_buffer(1, 1, vec![0; 4]),
            close_button: memory_buffer(1, 1, vec![0; 4]),
            maximize_button: memory_buffer(1, 1, vec![0; 4]),
            minimize_button: memory_buffer(1, 1, vec![0; 4]),
            drawn_controls: [None, None, None],
            window_height: 0,
            border_corners: corner_buffers(shaped),
            square: false,
            bottom_round: true,
            shaped,
            focus: 1.0,
            busy: false,
            border_top: SolidColorBuffer::default(),
            border_bottom: SolidColorBuffer::default(),
            border_left: SolidColorBuffer::default(),
            border_right: SolidColorBuffer::default(),
            title: TitleSource::default(),
            title_buffer: None,
            title_px_w: 0,
            title_px_h: 0,
            drawn_flat: None,
            frosted: false,
            #[cfg(feature = "decorations")]
            theme: Theme::default(),
            last_titlebar_click: None,
        }
    }

    /// The bar's own corners: the frame's two top ones, logical px. The bar's
    /// bottom meets the client, so it stays square.
    pub fn bar_radii(&self) -> Radii {
        Radii([self.shaped.top_left(), self.shaped.top_right(), 0.0, 0.0])
    }

    /// The corners the frame was last shaped with, whole px.
    fn corner_px(&self) -> [i32; 4] {
        self.shaped.0.map(|r| r.round() as i32)
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
        self.last_titlebar_click = None;
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
    
    /// Classify a pointer press at `self.pointer_loc` by button and double-click state.
    pub(crate) fn resolve_pointer_press(&mut self, button: u32, time: u32) -> Option<TitlebarAction> {
        if button == BTN_MIDDLE {
            self.last_titlebar_click = None;
            return self.header_point().map(|_| TitlebarAction::Lower);
        }
        if button != BTN_LEFT {
            self.last_titlebar_click = None;
            return None;
        }
        if let Some(edges) = self.resize_edge() {
            self.last_titlebar_click = None;
            return Some(TitlebarAction::StartResize(edges));
        }
        let loc = self.header_point()?;
        if loc.x >= self.width.saturating_sub(BUTTON_WIDTH) as f64 {
            self.last_titlebar_click = None;
            return Some(TitlebarAction::Close);
        }
        if loc.x >= self.width.saturating_sub(BUTTON_WIDTH * 2) as f64 {
            self.last_titlebar_click = None;
            return Some(TitlebarAction::MaximizeToggle);
        }
        if loc.x >= self.width.saturating_sub(BUTTON_WIDTH * 3) as f64 {
            self.last_titlebar_click = None;
            return Some(TitlebarAction::Minimize);
        }
        let is_double = self
            .last_titlebar_click
            .take()
            .is_some_and(|(prev_time, prev_loc)| {
                let dt = time.wrapping_sub(prev_time);
                let dx = loc.x - prev_loc.x;
                let dy = loc.y - prev_loc.y;
                dt <= DOUBLE_CLICK_MS && (dx * dx + dy * dy) <= DOUBLE_CLICK_DIST_SQ
            });
        if is_double {
            Some(TitlebarAction::MaximizeToggle)
        } else {
            self.last_titlebar_click = Some((time, loc));
            Some(TitlebarAction::StartMove)
        }
    }

    /// Handle pointer button press on the SSD titlebar/resize band.
    pub fn clicked_button<B: Backend>(
        &mut self,
        seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        serial: Serial,
        time: u32,
        button: u32,
    ) {
        let Some(action) = self.resolve_pointer_press(button, time) else {
            return;
        };
        match action {
            TitlebarAction::StartResize(edges) => {
                info!("SSD resize band press: edges={:?}", edges);
                self.handle_resize(seat, state, window, serial, edges);
            }
            TitlebarAction::Close => {
                info!("Close button clicked");
                self.handle_close(window);
            }
            TitlebarAction::MaximizeToggle => {
                info!("Maximize/restore triggered from SSD titlebar");
                self.handle_maximize(state, window);
            }
            TitlebarAction::Minimize => {
                info!("Minimize button clicked");
                self.handle_minimize(state, window);
            }
            TitlebarAction::Lower => {
                info!("Middle-click lowering window from SSD titlebar");
                self.handle_lower(state, window);
            }
            TitlebarAction::StartMove => {
                self.handle_move(seat, state, window, serial);
            }
        }
    }

    /// Handle click with fallback simple hit testing (left button compatibility helper).
    #[allow(dead_code)]
    pub fn clicked<B: Backend>(
        &mut self,
        seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        serial: Serial,
    ) {
        self.clicked_button(seat, state, window, serial, 0, BTN_LEFT);
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
    /// close/maximize) -- `EvoState::minimize_window` keeps the window in
    /// its workspace with everything needed to restore it. Deferred to an
    /// idle callback like the other buttons: it moves keyboard focus, and the
    /// click that got us here is still inside the pointer-button dispatch.
    fn handle_minimize<B: Backend>(&self, state: &mut EvoState<B>, window: &WindowElement) {
        let window = window.clone();
        state.handle.insert_idle(move |data| data.minimize_window(&window));
    }

    /// Lower the window to the back of the app stack (middle-click titlebar).
    fn handle_lower<B: Backend>(&self, state: &mut EvoState<B>, window: &WindowElement) {
        let window = window.clone();
        state.handle.insert_idle(move |data| data.lower_window(&window));
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
        match window.0.underlying_surface() {
            WindowSurface::Wayland(w) => {
                let seat = seat.clone();
                let toplevel = w.clone();
                state
                    .handle
                    .insert_idle(move |data| data.resize_request_xdg(&toplevel, &seat, serial, edges));
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(w) => {
                let seat = seat.clone();
                let surface = w.clone();
                state
                    .handle
                    .insert_idle(move |data| data.resize_request_x11(&surface, &seat, serial, edges));
            }
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
    
    /// Redraw the header bar: the frame's shape, the label in `title`, and
    /// the controls (hover, focus, busy).
    pub fn redraw(&mut self, width: u32, window_height: u32, title: &TitleSource) {
        if width == 0 {
            self.width = 0;
            return;
        }

        let shape = frame_shape(self.square, self.bottom_round);
        let reshaped = shape != self.shaped;
        let resized = width != self.width || reshaped;
        if resized {
            self.width = width;
            self.shaped = shape;
        }
        // The flat glass depends on the window's width, its colours and its
        // focus level: (re)rasterize it when one changes rather than on every
        // hover-triggered redraw. A frosted bar has none: its element draws it.
        if self.frosted {
            self.drawn_flat = None;
        } else {
            let colors = &title.colors;
            let key = FlatKey {
                width,
                shape,
                glass: colors.glass,
                glass_inactive: colors.glass_inactive,
                accent: colors.accent,
                dark: colors.dark,
                focus: (self.focus.clamp(0.0, 1.0) * 20.0).round() as u8,
            };
            if self.drawn_flat != Some(key) {
                self.background = memory_buffer(width, HEADER_BAR_HEIGHT as u32, rasterize_flat_bar(&key, HEADER_BAR_HEIGHT as u32));
                self.drawn_flat = Some(key);
            }
        }
        if reshaped {
            self.border_corners = corner_buffers(shape);
        }

        // === BORDERS === sized to the full window (width x window_height, and
        // the thin side strips down the whole height). Re-created only on a
        // real size change so damage tracking sees stable buffers otherwise.
        if resized || window_height != self.window_height {
            self.window_height = window_height;
            let [tl, tr, br, bl] = self.corner_px();
            let w = width as i32;
            let h = window_height.max(1) as i32;
            // Each edge runs BETWEEN the corners at its ends. The client
            // content is clipped to the same radius at its bottom
            // (`anim::shader`), so the frame reads as one rounded card. A
            // square frame has no radius.
            self.border_top.update(((w - tl - tr).max(0), BORDER_THICKNESS), BORDER_COLOR);
            self.border_bottom.update(((w - bl - br).max(0), BORDER_THICKNESS), BORDER_COLOR);
            self.border_left.update((BORDER_THICKNESS, (h - tl - bl).max(0)), BORDER_COLOR);
            self.border_right.update((BORDER_THICKNESS, (h - tr - br).max(0)), BORDER_COLOR);
        }

        // === TITLE === re-rasterize only when what it says or the space
        // available for it (window width minus the three right-hand buttons)
        // changes.
        if resized || *title != self.title {
            self.title = title.clone();
            let reserved = BUTTON_WIDTH * 3 + TITLE_PAD_LEFT + TITLE_GAP_BEFORE_BUTTONS;
            let avail = width.saturating_sub(reserved);
            match rasterize_label(&self.title, avail) {
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

        // === CONTROLS === slot 0 minimize, 1 maximize, 2 close (rightmost).
        let point = self.header_point();
        let hovered = |slot: u32| {
            point.is_some_and(|l| {
                let right = width.saturating_sub(BUTTON_WIDTH * (2 - slot)) as f64;
                l.x >= right - BUTTON_WIDTH as f64 && l.x < right
            })
        };
        self.minimize_button_hover = hovered(0);
        self.maximize_button_hover = hovered(1);
        self.close_button_hover = hovered(2);
        let settings = crate::anim::settings();
        let mode = if settings.enabled() { settings.mode } else { crate::anim::Mode::Off };
        // The spinner's frames, a few dozen per loop: enough to read as
        // motion, few enough that a frame is often drawn twice.
        let phase = (busy::phase(crate::anim::now()) * 48.0).floor() / 48.0;
        let base = ControlState {
            focus: (self.focus * 20.0).round() / 20.0,
            hover: false,
            busy: self.busy,
            mode,
            phase,
            quiet: self.title.colors.quiet,
        };
        let hovers = [self.minimize_button_hover, self.maximize_button_hover, self.close_button_hover];
        for slot in 0..3 {
            let layers = control_layers(slot, &ControlState { hover: hovers[slot], ..base });
            if self.drawn_controls[slot].as_ref() == Some(&layers) {
                continue;
            }
            let buffer = memory_buffer(BUTTON_WIDTH, BUTTON_HEIGHT, rasterize_control(&layers));
            match slot {
                0 => self.minimize_button = buffer,
                1 => self.maximize_button = buffer,
                _ => self.close_button = buffer,
            }
            self.drawn_controls[slot] = Some(layers);
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
        // header, which is why the HeaderBar tracks window_height. Rounded
        // corners follow the frame's (the waterdrop); straight edges run
        // between them.
        let full_h = self.window_height.max(HEADER_BAR_HEIGHT as u32) as i32;
        let w_i = self.width as i32;
        let [tl, tr, br, bl] = self.corner_px();
        // The header's own brightness, dimmed while the window is inactive.
        let dim = crate::anim::depth::HEADER_INACTIVE;
        let head = alpha * (dim + (1.0 - dim) * self.focus.clamp(0.0, 1.0));
        for (buf, off) in [
            (&self.border_top, Point::<i32, Logical>::from((tl, 0))),
            (&self.border_bottom, Point::from((bl, full_h - BORDER_THICKNESS))),
            (&self.border_left, Point::from((0, tl))),
            (&self.border_right, Point::from((w_i - BORDER_THICKNESS, tr))),
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
        // Rounded corners (arcs at each corner's radius), none where square.
        let [c_tl, c_tr, c_br, c_bl] = &self.border_corners;
        let corners = [
            (tl, c_tl, Point::<i32, Logical>::from((0, 0))),
            (tr, c_tr, Point::from((w_i - tr, 0))),
            (br, c_br, Point::from((w_i - br, full_h - br))),
            (bl, c_bl, Point::from((0, full_h - bl))),
        ];
        for (r, corner, off) in corners {
            if r <= 0 {
                continue;
            }
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
        //
        // The controls take the window's alpha, not the header's dimmed one:
        // focus already shows in them (outlines when inactive).
        for (slot, buffer) in [(3, &self.minimize_button), (2, &self.maximize_button), (1, &self.close_button)] {
            let loc = location + (header_end_offset - button_offset.upscale(slot)).to_physical_precise_round(scale);
            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                loc.to_f64(),
                buffer,
                Some(alpha),
                None,
                None,
                Kind::Unspecified,
            ) {
                elements.push(DecorationRenderElement::from(el).into());
            } else {
                warn!("Failed to import window control texture");
            }
        }

        // Title (item #9) -- over the glass, left-aligned, vertically
        // centered. Pushed before the background so the (semi-opaque) glass
        // doesn't clip it, same as the buttons.
        if let Some(title_buf) = self.title_buffer.as_ref() {
            let ty = (HEADER_BAR_HEIGHT as u32).saturating_sub(self.title_px_h) as i32 / 2;
            let title_off: Point<i32, Logical> = Point::from((TITLE_PAD_LEFT as i32, ty));
            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (location + title_off.to_physical_precise_round(scale)).to_f64(),
                title_buf,
                Some(head),
                None,
                None,
                Kind::Unspecified,
            ) {
                elements.push(DecorationRenderElement::from(el).into());
            } else {
                warn!("Failed to import title text texture");
            }
        }

        // Background (drawn last so it sits behind the buttons) -- the flat
        // glass, in the waterdrop's top corners, with its rim and seam. A
        // frosted bar has none here: a `FrostElement` follows these elements
        // and is its glass.
        if self.frosted {
            return elements;
        }
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
    
    /// Whether Evo draws this window's frame right now.
    ///
    /// This is what `SpaceElement::geometry`/`bbox`/`is_in_input_region` ask,
    /// and Space calls those from inside `map_element`, which several
    /// header-bar paths reach while they still hold `decoration_state()`'s
    /// `RefMut` (alpha2 #7 was that panic). So the answer lives in `Cell`s,
    /// outside the `RefCell`: reading it can never conflict with a borrow.
    pub fn is_ssd(&self) -> bool {
        self.user_data()
            .get::<SsdFlags>()
            .is_some_and(|flags| flags.negotiated.get() && !flags.suppressed.get())
    }

    /// Whether the window fills its space (maximized or fullscreen): it is
    /// drawn square, with no shadow. The committed state, i.e. what is on
    /// screen now, like `ssd_resizable_frame`.
    pub fn is_square(&self) -> bool {
        match self.0.underlying_surface() {
            WindowSurface::Wayland(toplevel) => toplevel.with_committed_state(|state| {
                state.is_some_and(|state| {
                    state.states.contains(xdg_toplevel::State::Maximized)
                        || state.states.contains(xdg_toplevel::State::Fullscreen)
                })
            }),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(surface) => surface.is_maximized() || surface.is_fullscreen(),
        }
    }

    /// Whether the client negotiated server-side decoration, whether or not
    /// Evo is drawing it at the moment (it isn't while fullscreen).
    pub fn ssd_negotiated(&self) -> bool {
        self.user_data()
            .get::<SsdFlags>()
            .is_some_and(|flags| flags.negotiated.get())
    }

    /// Enable or disable server-side decorations
    pub fn set_ssd(&self, ssd: bool) {
        self.user_data().insert_if_missing(SsdFlags::default);
        if let Some(flags) = self.user_data().get::<SsdFlags>() {
            flags.negotiated.set(ssd);
        }
    }

    /// Hide the frame without forgetting that it was negotiated: a
    /// fullscreen window is bare, and gets its header back when it leaves.
    pub fn set_ssd_suppressed(&self, suppressed: bool) {
        self.user_data().insert_if_missing(SsdFlags::default);
        if let Some(flags) = self.user_data().get::<SsdFlags>() {
            flags.suppressed.set(suppressed);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::Mode;

    /// The alpha of pixel `(x, y)` in a straight-alpha buffer `w` wide.
    fn alpha_at(buf: &[u8], w: u32, x: u32, y: u32) -> u8 {
        buf[((y * w + x) * 4 + 3) as usize]
    }

    fn rgb_at(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [buf[i + 2], buf[i + 1], buf[i]]
    }

    #[test]
    fn buffers_are_premultiplied_for_the_renderer() {
        // ARGB8888 in memory is B, G, R, A.
        assert_eq!(premultiply(vec![200, 100, 50, 128]), vec![100, 50, 25, 128]);
        assert_eq!(premultiply(vec![200, 100, 50, 255]), vec![200, 100, 50, 255]);
        assert_eq!(premultiply(vec![200, 100, 50, 0]), vec![0, 0, 0, 0]);
    }

    #[test]
    fn the_frame_is_a_waterdrop_and_square_when_maximized() {
        assert_eq!(frame_shape(false, true), Radii([16.0, 4.0, 16.0, 16.0]));
        assert!(frame_shape(true, true).is_square(), "maximized or fullscreen: square");
        assert_eq!(frame_shape(false, false), Radii([16.0, 4.0, 0.0, 0.0]), "no clip: square bottom");
    }

    #[test]
    fn the_glass_rounds_top_left_wide_and_top_right_tight() {
        let (w, h) = (200, HEADER_BAR_HEIGHT as u32);
        let buf = rasterize_glass_bar(w, h, 16.0, 4.0, [255, 255, 255, 255]);
        // 3 px in from each top corner: outside the 16 px curve, inside the 4 px one.
        assert_eq!(alpha_at(&buf, w, 2, 2), 0, "top-left is round");
        assert!(alpha_at(&buf, w, w - 3, 2) > 200, "top-right is nearly sharp");
        assert_eq!(alpha_at(&buf, w, w - 1, 0), 0, "but not square");
        assert_eq!(alpha_at(&buf, w, 0, h - 1), 255, "the bottom meets the client");
        let square = rasterize_glass_bar(w, h, 0.0, 0.0, [255, 255, 255, 255]);
        assert_eq!(alpha_at(&square, w, 0, 0), 255);
        assert_eq!(alpha_at(&square, w, w - 1, 0), 255);
    }

    #[test]
    fn border_edges_run_between_each_corner() {
        let mut bar = HeaderBar::new();
        bar.redraw(300, 232, &TitleSource::default());
        assert_eq!(bar.corner_px(), [16, 4, 16, 16]);
        // The right edge starts under the 4 px corner, the left under the 16.
        let h = 232;
        let len = |b: &SolidColorBuffer| {
            let e = SolidColorRenderElement::from_buffer(b, (0, 0), 1.0, 1.0, Kind::Unspecified);
            smithay::backend::renderer::element::Element::geometry(&e, 1.0.into()).size
        };
        assert_eq!(len(&bar.border_top).w, 300 - 16 - 4);
        assert_eq!(len(&bar.border_right).h, h - 4 - 16);
        assert_eq!(len(&bar.border_left).h, h - 16 - 16);
        bar.square = true;
        bar.redraw(300, 232, &TitleSource::default());
        assert_eq!(bar.corner_px(), [0, 0, 0, 0]);
        assert_eq!(len(&bar.border_top).w, 300);
    }

    fn state(focus: f32) -> ControlState {
        ControlState {
            focus,
            hover: false,
            busy: false,
            mode: Mode::Full,
            phase: 0.0,
            quiet: [0x2a, 0x26, 0x20, 0x8c],
        }
    }

    #[test]
    fn controls_are_the_spinners_shapes_in_its_colours() {
        let close = control_layers(2, &state(1.0));
        assert_eq!(close.len(), 1);
        assert_eq!((close[0].shape, close[0].fill), (Shape::Circle, None));
        assert_eq!(close[0].stroke, [0x2a, 0x26, 0x20, 0xd8]);
        assert_eq!(control_layers(1, &state(1.0))[0].fill, None);
        assert_eq!(control_layers(0, &state(1.0))[0].shape, Shape::Triangle);

        let hover_close = control_layers(2, &ControlState { hover: true, ..state(1.0) });
        assert_eq!(hover_close[0].fill, Some(SPINNER_RED), "hover reveals spinner red");
        assert_eq!(hover_close[0].stroke, [0xa1, 0x2c, 0x2c, 0xff]);

        let hover_max = control_layers(1, &ControlState { hover: true, ..state(1.0) });
        assert_eq!(hover_max[0].fill, Some(SPINNER_BLUE), "hover reveals spinner blue");

        let hover_min = control_layers(0, &ControlState { hover: true, ..state(1.0) });
        assert_eq!(hover_min[0].shape, Shape::Triangle);
        assert_eq!(hover_min[0].fill, Some(SPINNER_YELLOW), "hover reveals spinner yellow");
    }

    #[test]
    fn the_shape_is_14px_centred_in_its_32px_target() {
        let w = BUTTON_WIDTH;
        let rest = rasterize_control(&control_layers(1, &state(1.0)));
        assert_eq!(alpha_at(&rest, w, 16, 16), 0, "at rest the centre stays unfilled");
        assert!(alpha_at(&rest, w, 16, 9) > 0, "at rest the monochrome outline is drawn");

        let buf = rasterize_control(&control_layers(1, &ControlState { hover: true, ..state(1.0) }));
        assert_eq!(alpha_at(&buf, w, 16, 16), 255);
        assert_eq!(rgb_at(&buf, w, 16, 16), SPINNER_BLUE);
        assert_eq!(alpha_at(&buf, w, 3, 3), 0, "the target's corner stays empty");
        assert_eq!(alpha_at(&buf, w, 16, 7), 0, "above the shape");
        // The outline: its edge is the dark blue, not the fill.
        let edge = rgb_at(&buf, w, 16, 9);
        assert!(edge[2] < SPINNER_BLUE[2], "outlined: {edge:?}");
    }

    #[test]
    fn an_inactive_window_draws_outlines_in_the_quiet_ink_crossfading() {
        let off = control_layers(2, &state(0.0));
        assert_eq!(off.len(), 1);
        assert_eq!((off[0].fill, off[0].stroke), (None, [0x2a, 0x26, 0x20, 0x8c]));
        let buf = rasterize_control(&off);
        assert_eq!(alpha_at(&buf, BUTTON_WIDTH, 16, 16), 0, "no fill");
        let mid = control_layers(2, &state(0.5));
        assert_eq!(mid.len(), 2, "both, mid-crossfade");
        assert!((mid[0].alpha + mid[1].alpha - 1.0).abs() < 1e-6);
    }

    #[test]
    fn busy_runs_the_spinner_and_a_hovered_slot_stays_put() {
        let busy = ControlState { busy: true, ..state(0.0) };
        let shapes: Vec<Shape> = (0..3).map(|s| control_layers(s, &busy)[0].shape).collect();
        assert_eq!(shapes, vec![Shape::Circle, Shape::Square, Shape::Triangle]);
        assert!(control_layers(0, &busy)[0].fill.is_some(), "the spinner is in colour, even inactive");
        let hovered = control_layers(2, &ControlState { hover: true, ..busy });
        assert_eq!(hovered[0].shape, Shape::Circle, "close is still close under the pointer");
        // Reduced: own shapes, blinking. Off: own shapes, own outlines.
        let reduced = ControlState { mode: Mode::Reduced, ..busy };
        assert_eq!(control_layers(0, &reduced)[0].shape, Shape::Triangle);
        let off = ControlState { mode: Mode::Off, ..busy };
        let l = control_layers(1, &off)[0];
        assert_eq!((l.shape, l.fill, l.stroke), (Shape::Square, None, [0x33, 0x64, 0x7c, 0xff]));
    }

    fn flat_key(width: u32, focus: u8, dark: bool) -> FlatKey {
        let colors = super::super::identity::bar_colors(Some([0xea, 0xb3, 0x08]), dark, [0xfb, 0xfa, 0xf7]);
        FlatKey {
            width,
            shape: Radii([16.0, 4.0, 0.0, 0.0]),
            glass: colors.glass,
            glass_inactive: colors.glass_inactive,
            accent: colors.accent,
            dark,
            focus,
        }
    }

    #[test]
    fn the_flat_bar_has_a_rim_on_top_and_the_accent_for_a_seam() {
        let (w, h) = (200, HEADER_BAR_HEIGHT as u32);
        let buf = rasterize_flat_bar(&flat_key(w, 20, false), h);
        // The top row is lighter than the glass under it (white at 75%).
        let rim = rgb_at(&buf, w, 100, 0);
        let body = rgb_at(&buf, w, 100, 10);
        assert!(rim[2] > body[2], "the rim is whiter: {rim:?} over {body:?}");
        // The bottom row takes the accent (gold: blue falls well below red).
        let seam = rgb_at(&buf, w, 100, h - 1);
        assert!(seam[0] as i32 - seam[2] as i32 > 40, "{seam:?}");
        // The rounded corner is still cut, rim and seam included.
        assert_eq!(alpha_at(&buf, w, 0, 0), 0);
        assert_eq!(alpha_at(&buf, w, w - 1, 0), 0, "the sharp corner is still a hair round");
        assert!(alpha_at(&buf, w, 0, h - 1) >= 230, "the bottom stays square");
    }

    #[test]
    fn the_flat_bars_seam_and_glass_fade_with_focus() {
        let (w, h) = (120, HEADER_BAR_HEIGHT as u32);
        let on = rasterize_flat_bar(&flat_key(w, 20, false), h);
        let off = rasterize_flat_bar(&flat_key(w, 0, false), h);
        let strength = |buf: &[u8]| {
            let p = rgb_at(buf, w, 60, h - 1);
            p[0] as i32 - p[2] as i32
        };
        assert!(strength(&on) > strength(&off) * 2, "the seam fades: {} vs {}", strength(&on), strength(&off));
        let mid = rasterize_flat_bar(&flat_key(w, 10, false), h);
        assert!(strength(&mid) < strength(&on) && strength(&mid) > strength(&off));
    }

    #[test]
    fn a_dark_flat_bar_has_a_faint_rim() {
        let (w, h) = (120, HEADER_BAR_HEIGHT as u32);
        let dark = rasterize_flat_bar(&flat_key(w, 20, true), h);
        let light = rasterize_flat_bar(&flat_key(w, 20, false), h);
        let lift = |buf: &[u8]| rgb_at(buf, w, 60, 0)[0] as i32 - rgb_at(buf, w, 60, 8)[0] as i32;
        assert!(lift(&dark) > 0 && lift(&dark) < 60, "14%: {}", lift(&dark));
        // On a near-white glass a white rim has nowhere to go.
        assert!(lift(&light) >= 0);
    }

    #[test]
    fn a_frosted_bar_draws_no_glass_of_its_own() {
        let mut flat = HeaderBar::new();
        flat.redraw(300, 232, &TitleSource::default());
        assert!(flat.drawn_flat.is_some());
        flat.frosted = true;
        flat.redraw(300, 232, &TitleSource::default());
        assert!(flat.drawn_flat.is_none(), "the element draws it");
        assert_eq!(flat.bar_radii(), Radii([16.0, 4.0, 0.0, 0.0]));
        flat.square = true;
        flat.redraw(300, 232, &TitleSource::default());
        assert!(flat.bar_radii().is_square());
    }

    #[test]
    fn the_label_is_icon_name_then_quiet_document() {
        let colors = super::super::identity::bar_colors(Some([0xea, 0xb3, 0x08]), false, [0xfb, 0xfa, 0xf7]);
        let icon = Icon { size: 20, rgba: [255, 0, 0, 255].repeat(400) };
        let src = TitleSource {
            label: super::super::identity::title_label(Some("Notes"), "Groceries"),
            icon: Some(std::sync::Arc::new(icon)),
            colors,
        };
        if title_font().is_none() {
            return; // no DejaVu on this machine: the bar draws no text
        }
        let (w, h, buf) = rasterize_label(&src, 400).unwrap();
        assert_eq!(h, HEADER_BAR_HEIGHT as u32);
        assert_eq!(rgb_at(&buf, w, 10, 16), [255, 0, 0], "the icon, centred");
        assert_eq!(alpha_at(&buf, w, 10, 2), 0);
        let narrow = rasterize_label(&src, 60).unwrap();
        assert!(narrow.0 <= 60, "clipped to the space it has");
        let plain = TitleSource { label: super::super::identity::title_label(None, "xterm"), icon: None, colors };
        let (pw, _, _) = rasterize_label(&plain, 400).unwrap();
        assert!(pw < w, "no icon, no name");
    }

    #[test]
    fn header_bar_respects_button_code_double_click_and_middle_click() {
        let mut bar = HeaderBar::new();
        bar.width = 400;
        bar.window_height = 300;
        let btn_right: u32 = 0x111;

        // Right-clicks inside controls and titlebar are ignored.
        bar.pointer_enter((390.0, 10.0).into());
        assert_eq!(bar.resolve_pointer_press(btn_right, 100), None);
        bar.pointer_enter((350.0, 10.0).into());
        assert_eq!(bar.resolve_pointer_press(btn_right, 110), None);
        bar.pointer_enter((100.0, 10.0).into());
        assert_eq!(bar.resolve_pointer_press(btn_right, 120), None);

        // Middle-click anywhere on the titlebar lowers the window.
        bar.pointer_enter((100.0, 10.0).into());
        assert_eq!(bar.resolve_pointer_press(BTN_MIDDLE, 130), Some(TitlebarAction::Lower));

        // Left-clicks on controls act directly without priming double-click.
        bar.pointer_enter((390.0, 10.0).into());
        assert_eq!(bar.resolve_pointer_press(BTN_LEFT, 140), Some(TitlebarAction::Close));
        bar.pointer_enter((350.0, 10.0).into());
        assert_eq!(bar.resolve_pointer_press(BTN_LEFT, 150), Some(TitlebarAction::MaximizeToggle));
        bar.pointer_enter((320.0, 10.0).into());
        assert_eq!(bar.resolve_pointer_press(BTN_LEFT, 160), Some(TitlebarAction::Minimize));

        // Left-click titlebar then quick nearby second click toggles maximize; 3rd starts fresh move.
        bar.pointer_enter((120.0, 12.0).into());
        assert_eq!(bar.resolve_pointer_press(BTN_LEFT, 200), Some(TitlebarAction::StartMove));
        bar.pointer_enter((123.0, 14.0).into());
        assert_eq!(bar.resolve_pointer_press(BTN_LEFT, 450), Some(TitlebarAction::MaximizeToggle));
        assert_eq!(bar.resolve_pointer_press(BTN_LEFT, 500), Some(TitlebarAction::StartMove));

        // Too slow (> 400ms) or too far (> 6px) starts a move instead of double-click.
        assert_eq!(bar.resolve_pointer_press(BTN_LEFT, 1000), Some(TitlebarAction::StartMove));
        bar.pointer_enter((150.0, 14.0).into());
        assert_eq!(bar.resolve_pointer_press(BTN_LEFT, 1050), Some(TitlebarAction::StartMove));
    }
}
