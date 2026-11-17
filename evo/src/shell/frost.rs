//! The frosted title bar (`docs/internal/pyxis-glass-and-lock-plan.md`, part 1).
//!
//! Bloom's overlay is transparent, so a `backdrop-filter` there blurs nothing.
//! The title bar is Evo's own and Evo composites every window, so this is the
//! one place in the shell where glass can be real. A bar is drawn by a
//! `FrostElement` (`anim::shader`) from a texture made here.
//!
//! # Tiers
//!
//! Each falls back to the one below; `GetFrostState` says which a window is on.
//!
//! - **F1, wallpaper frost.** The desktop window (Bloom) is snapshotted at a
//!   quarter of its size when its commit counter moves, at most four times a
//!   second (the wallpaper moves a few px a second, and after a 20 px blur the
//!   difference between four updates and ten is invisible; a paused or still
//!   wallpaper costs nothing). The picture is blurred on the CPU (three box
//!   passes, about a gaussian) and veiled for the theme into one texture, the
//!   *frost field*. Each bar draws the crop of it under itself, scaled up.
//!   Over another window it shows the wallpaper, not that window.
//! - **F0, flat.** One colour: Bloom's material veiled for the theme (or Evo's
//!   own), drawn as the memory buffer the glass always was. This is what a
//!   backend with no `snapshot_window` (winit, x11), a failed shader, a bar
//!   whose desktop has not been captured yet, and `SetFrost off` all draw.
//!   Never worse than the bar before frost.
//!
//! # Where F2 plugs in
//!
//! F2 (true frost, the windows below the bar included) is not built. It needs
//! a second source for the same thing: a field per bar instead of one shared
//! field. `choose_tier` is where a third answer goes (it already takes the
//! mode, `Mode::Full` being reserved for it), `bar_elements` is where a bar
//! picks its `Backdrop`, and the veil, the blur and the element are shared:
//! F2 only changes what is captured (the stack from the bar's window down,
//! rendered offscreen through `anim::snapshot::render_to_mem`, cropped to the
//! bar plus a blur margin) and where the pixels come from.
//!
//! # The veil
//!
//! The backdrop is mixed toward the theme's colour in OKLab, its lightness is
//! held in a band, and its chroma is raised a little and capped (`veil`,
//! parameters in `identity::VeilParams`). The band is what makes the ink safe:
//! Evo never chooses ink per frame, the light bar is always light and the dark
//! bar always dark, so the fixed ink clears 7:1 over any backdrop. Unit tests
//! check that over a sweep of input colours.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Mutex, Once, OnceLock,
    },
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use smithay::{
    backend::renderer::{
        element::Id,
        gles::{GlesTexProgram, GlesTexture},
        utils::{with_renderer_surface_state, CommitCounter, RendererSurfaceStateUserData},
        ImportMem,
    },
    desktop::space::SpaceElement,
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Buffer, Physical, Point, Rectangle, Size},
    wayland::compositor::{with_surface_tree_downward, TraversalAction},
};
use tracing::{info, warn};

use super::identity::{
    veil_params, BarColors, Radii, VeilParams, INACTIVE_CHROMA, INACTIVE_MIX,
};
use super::WindowElement;
use crate::{
    anim::{
        shader::{self, FrostElement, FrostParams, GlesAccess},
        snapshot::READBACK_FORMAT,
    },
    state::{Backend, EvoState},
};

// === The switches ===

/// How much of the frost Evo builds. `Full` is F2's, reserved: it behaves as
/// `Wallpaper` until F2 exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    Wallpaper,
    Full,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Wallpaper => "wallpaper",
            Mode::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "none" | "flat" => Some(Mode::Off),
            "wallpaper" | "on" | "1" => Some(Mode::Wallpaper),
            "full" => Some(Mode::Full),
            _ => None,
        }
    }

    fn code(self) -> u8 {
        self as u8
    }

    fn from_code(code: u8) -> Mode {
        match code {
            0 => Mode::Off,
            2 => Mode::Full,
            _ => Mode::Wallpaper,
        }
    }
}

static MODE: AtomicU8 = AtomicU8::new(Mode::Wallpaper as u8);
static ENV_READ: Once = Once::new();
/// Set when frost is turned on, so the next tick captures at once instead of
/// waiting for the desktop to commit.
static FORCE: AtomicBool = AtomicBool::new(true);

/// `EVO_FROST=off|wallpaper|full` at start (default wallpaper),
/// `evo.SetFrost {mode}` after.
pub fn mode() -> Mode {
    ENV_READ.call_once(|| {
        if let Some(mode) = std::env::var("EVO_FROST").ok().as_deref().and_then(Mode::parse) {
            MODE.store(mode.code(), Ordering::Relaxed);
        }
    });
    Mode::from_code(MODE.load(Ordering::Relaxed))
}

pub fn set_mode(mode_: Mode) {
    mode();
    let was = Mode::from_code(MODE.swap(mode_.code(), Ordering::Relaxed));
    if was == Mode::Off && mode_ != Mode::Off {
        FORCE.store(true, Ordering::Relaxed);
    }
    if was != Mode::Full && mode_ == Mode::Full {
        F2_FORCE.store(true, Ordering::Relaxed);
    }
    info!("frost: {}", mode_.as_str());
}

/// Which tier a window's bar is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tier {
    #[default]
    F0,
    F1,
    F2,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::F0 => "f0",
            Tier::F1 => "f1",
            Tier::F2 => "f2",
        }
    }
}

/// The tier a bar gets. `program` is whether the frost shader is usable,
/// `backdrop` whether there is a captured field to crop.
pub fn choose_tier(mode: Mode, program: bool, backdrop: bool) -> Tier {
    match mode {
        Mode::Off => Tier::F0,
        Mode::Wallpaper | Mode::Full if program && backdrop => Tier::F1,
        _ => Tier::F0,
    }
}

/// The tier a bar gets when per-window F2 captures are also considered.
pub fn choose_bar_tier(mode: Mode, program: bool, f1_backdrop: bool, f2_backdrop: bool) -> Tier {
    if mode == Mode::Full && program && f2_backdrop {
        Tier::F2
    } else {
        choose_tier(mode, program, f1_backdrop)
    }
}

// === What the suites read ===

/// An F1/F2 update that takes longer than this, ms, counts as over budget. An
/// update runs on the compositor thread at most four times a second, so this
/// is a frame's worth: measured on the 4 GB VM (software GL, the capture's
/// readback included) an update took 4-7 ms, under 3% of a core; on the dev
/// box's release build the CPU part is ~1.5 ms.
const BUDGET_MS: f64 = 10.0;

static UPDATES_F1: AtomicU64 = AtomicU64::new(0);
static UPDATES_F2: AtomicU64 = AtomicU64::new(0);
static SKIPPED: AtomicU64 = AtomicU64::new(0);
static OVER_BUDGET: AtomicU64 = AtomicU64::new(0);
static LAST_MS: AtomicU64 = AtomicU64::new(0);
static TIERS: Mutex<BTreeMap<u32, Tier>> = Mutex::new(BTreeMap::new());
static F2_FORCE: AtomicBool = AtomicBool::new(false);

fn record_update(ms: f64) {
    UPDATES_F1.fetch_add(1, Ordering::Relaxed);
    LAST_MS.store(ms.to_bits(), Ordering::Relaxed);
    if ms > BUDGET_MS {
        OVER_BUDGET.fetch_add(1, Ordering::Relaxed);
        warn!("frost: an update took {ms:.2} ms (budget {BUDGET_MS} ms)");
    }
}

/// Record an F2 update's duration; returns `false` if it exceeded `BUDGET_MS`
/// so the caller can drop the F2 field and fall back to `Tier::F1`.
pub fn record_f2_update(ms: f64) -> bool {
    UPDATES_F2.fetch_add(1, Ordering::Relaxed);
    LAST_MS.store(ms.to_bits(), Ordering::Relaxed);
    if ms > BUDGET_MS {
        OVER_BUDGET.fetch_add(1, Ordering::Relaxed);
        warn!("frost: an F2 update took {ms:.2} ms (budget {BUDGET_MS} ms)");
        return false;
    }
    true
}

/// `{mode, program, tiers: {<window_id>: "f0"|"f1"|"f2"}, updates: {f1},
/// f2_updates, last_ms, over_budget, skipped}` for `GetFrostState` (echo-cli
/// only, not in the bridge). `skipped` counts captures whose pixels were the
/// last one's, which cost no update.
pub fn state_json() -> Value {
    let tiers: serde_json::Map<String, Value> = TIERS
        .lock()
        .map(|t| t.iter().map(|(id, tier)| (id.to_string(), json!(tier.as_str()))).collect())
        .unwrap_or_default();
    json!({
        "mode": mode().as_str(),
        "program": shader::frost_status(),
        "tiers": tiers,
        "updates": { "f1": UPDATES_F1.load(Ordering::Relaxed) },
        "f2_updates": UPDATES_F2.load(Ordering::Relaxed),
        "last_ms": f64::from_bits(LAST_MS.load(Ordering::Relaxed)),
        "over_budget": OVER_BUDGET.load(Ordering::Relaxed),
        "skipped": SKIPPED.load(Ordering::Relaxed),
    })
}

// === Colour: OKLab and the veil ===

fn decode_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0f32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let s = i as f32 / 255.0;
            *v = if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) };
        }
        t
    })
}

const ENCODE_STEPS: usize = 16384;

/// Linear light to an sRGB byte, through a table: the field has tens of
/// thousands of pixels, and a `powf` per channel was most of the update.
fn encode_lut() -> &'static [u8; ENCODE_STEPS + 1] {
    static LUT: OnceLock<[u8; ENCODE_STEPS + 1]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u8; ENCODE_STEPS + 1];
        for (i, v) in t.iter_mut().enumerate() {
            let l = i as f32 / ENCODE_STEPS as f32;
            let s = if l <= 0.0031308 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
            *v = (s * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        t
    })
}

fn encode(l: f32) -> u8 {
    encode_lut()[(l.clamp(0.0, 1.0) * ENCODE_STEPS as f32).round() as usize]
}

/// The cube root of `x` in `0..=1`, good to a few parts in a million: a bit
/// hack for a first guess and Newton's method after it. `f32::cbrt` calls into
/// libm, and six of them per pixel were most of an update.
fn cbrt(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    let mut y = f32::from_bits(x.to_bits() / 3 + 0x2a50_8000);
    for _ in 0..3 {
        y -= (y * y * y - x) / (3.0 * y * y);
    }
    y
}

/// An sRGB colour as OKLab `[L, a, b]` (Björn Ottosson's matrices).
pub fn oklab(rgb: [u8; 3]) -> [f32; 3] {
    let lut = decode_lut();
    let (r, g, b) = (lut[rgb[0] as usize], lut[rgb[1] as usize], lut[rgb[2] as usize]);
    let l = cbrt(0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b);
    let m = cbrt(0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b);
    let s = cbrt(0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b);
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab as linear sRGB, unclamped.
fn linear_of(lab: [f32; 3]) -> [f32; 3] {
    let l = lab[0] + 0.396_337_78 * lab[1] + 0.215_803_76 * lab[2];
    let m = lab[0] - 0.105_561_346 * lab[1] - 0.063_854_17 * lab[2];
    let s = lab[0] - 0.089_484_18 * lab[1] - 1.291_485_5 * lab[2];
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_4 * s,
        -0.004_196_086 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

fn in_gamut(rgb: [f32; 3]) -> bool {
    rgb.iter().all(|c| (-0.000_5..=1.000_5).contains(c))
}

/// OKLab as an sRGB colour. One that falls outside the gamut loses chroma and
/// keeps its lightness, so the band the veil guarantees survives the trip.
pub fn srgb_of(lab: [f32; 3]) -> [u8; 3] {
    let mut rgb = linear_of(lab);
    if !in_gamut(rgb) {
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..9 {
            let mid = (lo + hi) / 2.0;
            if in_gamut(linear_of([lab[0], lab[1] * mid, lab[2] * mid])) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        rgb = linear_of([lab[0], lab[1] * lo, lab[2] * lo]);
    }
    [encode(rgb[0]), encode(rgb[1]), encode(rgb[2])]
}

/// OKLab lightness of an sRGB colour.
#[cfg(test)]
pub fn lightness(rgb: [u8; 3]) -> f32 {
    oklab(rgb)[0]
}

/// OKLab chroma of an sRGB colour.
#[cfg(test)]
pub fn chroma(rgb: [u8; 3]) -> f32 {
    let lab = oklab(rgb);
    lab[1].hypot(lab[2])
}

fn veil_lab(lab: [f32; 3], target: [f32; 3], p: &VeilParams, inactive: bool) -> [f32; 3] {
    let mix = (p.mix + if inactive { INACTIVE_MIX } else { 0.0 }).clamp(0.0, 1.0) as f32;
    let lerp = |a: f32, b: f32| a + (b - a) * mix;
    let l = lerp(lab[0], target[0]).clamp(p.l_min as f32, p.l_max as f32);
    let (a, b) = (lerp(lab[1], target[1]), lerp(lab[2], target[2]));
    let c = a.hypot(b);
    if c <= 1e-6 {
        return [l, 0.0, 0.0];
    }
    let mut want = (c * p.chroma_gain as f32).min(p.chroma_cap as f32);
    if inactive {
        want *= INACTIVE_CHROMA as f32;
    }
    [l, a / c * want, b / c * want]
}

/// What a backdrop colour becomes as glass: mixed toward the theme's colour,
/// lightness held in the theme's band, chroma raised and capped. An inactive
/// window's glass takes more of the veil and keeps little of the colour.
pub fn veil(rgb: [u8; 3], p: &VeilParams, inactive: bool) -> [u8; 3] {
    srgb_of(veil_lab(oklab(rgb), oklab(p.target), p, inactive))
}

// === The blur ===

/// The wallpaper is blurred like CSS `blur(20px)`: a gaussian of this
/// deviation in logical px, about 5 texels at the capture's quarter scale (2.5
/// after the halving).
const BLUR_SIGMA: f32 = 20.0;
/// The capture's size as a fraction of the desktop window's.
pub const CAPTURE_FRACTION: f64 = 0.25;
/// A blur this wide needs no more detail than half of the capture's, so the
/// picture is halved before it is blurred and veiled (the OKLab maths is most
/// of an update), and the texture is an eighth of the desktop's size; the GPU's
/// bilinear filter scales it back up.
const FIELD_DOWNSAMPLE: usize = 2;

/// The widths of `n` box blurs whose composite approximates a gaussian of
/// deviation `sigma` (Kutskir's, after Wells).
pub fn box_sizes(sigma: f32, n: usize) -> Vec<usize> {
    let nf = n as f32;
    let ideal = (12.0 * sigma * sigma / nf + 1.0).sqrt();
    let mut lower = ideal.floor() as i32;
    if lower % 2 == 0 {
        lower -= 1;
    }
    let lower = lower.max(1);
    let upper = lower + 2;
    let (l, nl) = (lower as f32, lower as f32);
    let m = ((12.0 * sigma * sigma - nf * l * nl - 4.0 * nf * l - 3.0 * nf) / (-4.0 * l - 4.0)).round();
    (0..n).map(|i| if (i as f32) < m { lower as usize } else { upper as usize }).collect()
}

/// One box blur of radius `r` along a line of `n` pixels starting at `start`,
/// `step` apart, edges held (the pixel past the end is the end).
fn blur_line(src: &[[f32; 3]], dst: &mut [[f32; 3]], start: usize, step: usize, n: usize, r: usize) {
    let at = |i: isize| src[start + i.clamp(0, n as isize - 1) as usize * step];
    let r = r as isize;
    let width = (2 * r + 1) as f32;
    let mut sum = [0f32; 3];
    for i in -r..=r {
        let p = at(i);
        for c in 0..3 {
            sum[c] += p[c];
        }
    }
    for i in 0..n as isize {
        let out = &mut dst[start + i as usize * step];
        for c in 0..3 {
            out[c] = sum[c] / width;
        }
        let (add, drop) = (at(i + r + 1), at(i - r));
        for c in 0..3 {
            sum[c] += add[c] - drop[c];
        }
    }
}

/// Blur `buf` (`w` by `h` pixels) in place with three box passes each way.
pub fn gaussian_blur(buf: &mut [[f32; 3]], w: usize, h: usize, sigma: f32) {
    if w == 0 || h == 0 || sigma <= 0.0 {
        return;
    }
    let mut tmp = vec![[0f32; 3]; buf.len()];
    for size in box_sizes(sigma, 3) {
        let r = (size - 1) / 2;
        if r == 0 {
            continue;
        }
        for y in 0..h {
            blur_line(buf, &mut tmp, y * w, 1, w, r);
        }
        for x in 0..w {
            blur_line(&tmp, buf, x, w, h, r);
        }
    }
}

/// `buf` (`w` by `h`) averaged in `n` by `n` blocks; a last partial block
/// row or column is dropped. Returns the new size too.
pub fn downsample(buf: &[[f32; 3]], w: usize, h: usize, n: usize) -> (Vec<[f32; 3]>, usize, usize) {
    let (fw, fh) = ((w / n).max(1), (h / n).max(1));
    let n = n.min(w).min(h).max(1);
    let area = (n * n) as f32;
    let mut out = Vec::with_capacity(fw * fh);
    for y in 0..fh {
        for x in 0..fw {
            let mut sum = [0f32; 3];
            for dy in 0..n {
                for dx in 0..n {
                    let p = buf[(y * n + dy) * w + x * n + dx];
                    for c in 0..3 {
                        sum[c] += p[c];
                    }
                }
            }
            out.push(sum.map(|v| v / area));
        }
    }
    (out, fw, fh)
}

// === The field: the wallpaper, blurred and veiled ===

/// One veiled copy of the field, as the pixels the texture is made from and
/// the texture once a renderer has uploaded it.
struct Variant {
    pixels: Vec<u8>,
    texture: RefCell<Option<GlesTexture>>,
}

/// The wallpaper at a quarter scale, blurred, and what is made from it.
struct Field {
    /// How many times the picture or its theme has changed; what a bar's
    /// damage counter follows.
    generation: u64,
    /// The texture's size: the capture's, halved.
    size: (u32, u32),
    /// The part of the output, physical px, the texture stands for: x, y, w, h.
    covers: [f64; 4],
    /// The last capture as it came (pixels, size, place), to tell an
    /// identical one.
    raw: Vec<u8>,
    raw_size: (u32, u32),
    raw_covers: [f64; 4],
    blurred: Vec<[f32; 3]>,
    /// The theme the variants are veiled for.
    dark: bool,
    /// Active, inactive. The inactive one is made when a bar asks for it.
    variants: [Option<Rc<Variant>>; 2],
}

thread_local! {
    static FIELD: RefCell<Option<Field>> = const { RefCell::new(None) };
    static F2_FIELDS: RefCell<BTreeMap<u32, Field>> = const { RefCell::new(BTreeMap::new()) };
    static F2_NEXT_GEN: Cell<u64> = const { Cell::new(1) };
}

fn next_f2_gen() -> u64 {
    F2_NEXT_GEN.with(|g| {
        let cur = g.get();
        g.set(cur + 1);
        cur
    })
}

/// What `ingest` did with a capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ingested {
    /// A new picture: blurred, veiled, in the field.
    Updated,
    /// The same pixels as before: nothing to do.
    Same,
    /// Not a picture of a usable size.
    Refused,
}

fn make_variant(blurred: &[[f32; 3]], p: &VeilParams, inactive: bool) -> Variant {
    let target = oklab(p.target);
    let mut pixels = Vec::with_capacity(blurred.len() * 4);
    // After the blur neighbouring texels are mostly the same colour: the last
    // answer is reused while the input holds.
    let mut last: Option<([u8; 3], [u8; 3])> = None;
    for px in blurred {
        let rgb = px.map(|v| v.round().clamp(0.0, 255.0) as u8);
        let c = match last {
            Some((from, to)) if from == rgb => to,
            _ => {
                let to = srgb_of(veil_lab(oklab(rgb), target, p, inactive));
                last = Some((rgb, to));
                to
            }
        };
        pixels.extend_from_slice(&[c[0], c[1], c[2], 0xff]);
    }
    Variant { pixels, texture: RefCell::new(None) }
}

/// Alpha-blend premultiplied RGBA `over` onto `base` in physical output
/// coordinates. Both buffers are top-row-first `[R, G, B, A]` with `size =
/// (width, height)` covering `[x, y, w, h]` in physical output pixels.
pub fn composite_rgba(
    base: &mut [u8],
    base_size: (u32, u32),
    base_covers: [f64; 4],
    over: &[u8],
    over_size: (u32, u32),
    over_covers: [f64; 4],
) {
    let (bw, bh) = (base_size.0 as usize, base_size.1 as usize);
    let (ow, oh) = (over_size.0 as usize, over_size.1 as usize);
    if bw == 0 || bh == 0 || ow == 0 || oh == 0 {
        return;
    }
    if base.len() < bw * bh * 4 || over.len() < ow * oh * 4 {
        return;
    }
    if !(base_covers[2] > 0.0 && base_covers[3] > 0.0 && over_covers[2] > 0.0 && over_covers[3] > 0.0) {
        return;
    }

    let base_px_w = base_covers[2] / bw as f64;
    let base_px_h = base_covers[3] / bh as f64;
    let over_sx = ow as f64 / over_covers[2];
    let over_sy = oh as f64 / over_covers[3];

    let x_start = (((over_covers[0] - base_covers[0]) / base_px_w).floor().max(0.0) as usize).min(bw);
    let x_end = ((((over_covers[0] + over_covers[2]) - base_covers[0]) / base_px_w).ceil().max(0.0) as usize).min(bw);
    let y_start = (((over_covers[1] - base_covers[1]) / base_px_h).floor().max(0.0) as usize).min(bh);
    let y_end = ((((over_covers[1] + over_covers[3]) - base_covers[1]) / base_px_h).ceil().max(0.0) as usize).min(bh);

    for by in y_start..y_end {
        let phys_y = base_covers[1] + (by as f64 + 0.5) * base_px_h;
        let oy_f = (phys_y - over_covers[1]) * over_sy;
        if oy_f < 0.0 || oy_f >= oh as f64 {
            continue;
        }
        let oy = oy_f as usize;
        for bx in x_start..x_end {
            let phys_x = base_covers[0] + (bx as f64 + 0.5) * base_px_w;
            let ox_f = (phys_x - over_covers[0]) * over_sx;
            if ox_f < 0.0 || ox_f >= ow as f64 {
                continue;
            }
            let ox = ox_f as usize;
            let si = (oy * ow + ox) * 4;
            let src = &over[si..si + 4];
            let sa = src[3];
            if sa == 0 {
                continue;
            }
            let di = (by * bw + bx) * 4;
            let dst = &mut base[di..di + 4];
            if sa == 255 {
                dst.copy_from_slice(src);
            } else {
                let inv_a = 1.0 - (sa as f32 / 255.0);
                for c in 0..4 {
                    dst[c] = (src[c] as f32 + dst[c] as f32 * inv_a).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

fn ingest_into(
    slot: &mut Option<Field>,
    rgba: &[u8],
    size: (u32, u32),
    covers: [f64; 4],
    scale: f64,
    dark: bool,
    next_gen: impl FnOnce(Option<&Field>) -> u64,
) -> Ingested {
    let (w, h) = (size.0 as usize, size.1 as usize);
    if w < 4 || h < 4 || rgba.len() != w * h * 4 || !(covers[2] > 0.0 && covers[3] > 0.0) {
        return Ingested::Refused;
    }
    if let Some(f) = slot.as_ref() {
        // The place can still have moved under identical pixels.
        if f.raw_size == size && f.raw_covers == covers && f.raw == rgba {
            return Ingested::Same;
        }
    }
    let picture: Vec<[f32; 3]> = rgba
        .chunks_exact(4)
        .map(|p| {
            // A snapshot is premultiplied; the desktop is opaque, so this
            // is the colour unless something has made it translucent.
            let a = p[3].max(1) as f32 / 255.0;
            [p[0] as f32 / a, p[1] as f32 / a, p[2] as f32 / a].map(|v| v.min(255.0))
        })
        .collect();
    let covers_in = covers;
    // Halved first and blurred after, at the halved size's deviation: the
    // same blur for a quarter of the work.
    let (mut blurred, fw, fh) = downsample(&picture, w, h, FIELD_DOWNSAMPLE);
    gaussian_blur(&mut blurred, fw, fh, BLUR_SIGMA * scale as f32 / FIELD_DOWNSAMPLE as f32);
    // The texture stands for the part of the picture the blocks covered.
    let covers = [
        covers[0],
        covers[1],
        covers[2] * (fw * FIELD_DOWNSAMPLE) as f64 / w as f64,
        covers[3] * (fh * FIELD_DOWNSAMPLE) as f64 / h as f64,
    ];
    let params = veil_params(dark);
    let active = make_variant(&blurred, &params, false);
    let generation = next_gen(slot.as_ref());
    *slot = Some(Field {
        generation,
        size: (fw as u32, fh as u32),
        covers,
        raw: rgba.to_vec(),
        raw_size: size,
        raw_covers: covers_in,
        blurred,
        dark,
        variants: [Some(Rc::new(active)), None],
    });
    Ingested::Updated
}

/// Take a capture of the desktop (`rgba`, premultiplied, `size` texels, which
/// stands for `covers` of the output in physical px; `scale` is texels per
/// logical px) into the field: blur it, veil it for `dark`. `Same` when it is
/// the last capture's pixels.
pub fn ingest(rgba: &[u8], size: (u32, u32), covers: [f64; 4], scale: f64, dark: bool) -> Ingested {
    FIELD.with(|field| {
        let mut field = field.borrow_mut();
        ingest_into(&mut field, rgba, size, covers, scale, dark, |prev| prev.map_or(1, |f| f.generation + 1))
    })
}

/// Ingest a composited F2 backdrop for window `win_id`.
pub fn ingest_f2(win_id: u32, rgba: &[u8], size: (u32, u32), covers: [f64; 4], scale: f64, dark: bool) -> Ingested {
    F2_FIELDS.with(|fields| {
        let mut fields = fields.borrow_mut();
        let mut slot = fields.remove(&win_id);
        let res = ingest_into(&mut slot, rgba, size, covers, scale, dark, |_| next_f2_gen());
        if let Some(f) = slot {
            fields.insert(win_id, f);
        }
        res
    })
}

/// Remove the F2 field for `win_id` so the bar drops cleanly back to F1.
pub fn clear_f2(win_id: u32) {
    F2_FIELDS.with(|fields| {
        fields.borrow_mut().remove(&win_id);
    });
}

fn clear_all_f2() {
    F2_FIELDS.with(|fields| fields.borrow_mut().clear());
}

/// Whether window `win_id` has a cached F2 field.
pub fn has_f2_field(win_id: u32) -> bool {
    F2_FIELDS.with(|fields| fields.borrow().contains_key(&win_id))
}

/// Whether there is a picture to frost.
pub fn has_field() -> bool {
    FIELD.with(|f| f.borrow().is_some())
}

/// The field's generation (0 with none).
#[cfg(test)]
pub fn generation() -> u64 {
    FIELD.with(|f| f.borrow().as_ref().map_or(0, |f| f.generation))
}

fn field_variant(
    f: &mut Field,
    dark: bool,
    inactive: bool,
    bump_gen: impl FnOnce(u64) -> u64,
) -> Option<(Rc<Variant>, u64, (u32, u32), [f64; 4])> {
    if f.dark != dark {
        f.dark = dark;
        f.variants = [None, None];
        f.generation = bump_gen(f.generation);
    }
    let slot = inactive as usize;
    if f.variants[slot].is_none() {
        let v = make_variant(&f.blurred, &veil_params(dark), inactive);
        f.variants[slot] = Some(Rc::new(v));
    }
    Some((f.variants[slot].clone()?, f.generation, f.size, f.covers))
}

/// The variant for a theme and activity, made if it isn't yet; a theme change
/// remakes both and counts as a new generation (the bars' counters move).
fn variant(dark: bool, inactive: bool) -> Option<(Rc<Variant>, u64, (u32, u32), [f64; 4])> {
    FIELD.with(|field| {
        let mut field = field.borrow_mut();
        let f = field.as_mut()?;
        field_variant(f, dark, inactive, |g| g + 1)
    })
}

fn variant_f2(win_id: u32, dark: bool, inactive: bool) -> Option<(Rc<Variant>, u64, (u32, u32), [f64; 4])> {
    F2_FIELDS.with(|fields| {
        let mut fields = fields.borrow_mut();
        let f = fields.get_mut(&win_id)?;
        field_variant(f, dark, inactive, |_| next_f2_gen())
    })
}

/// A veiled field uploaded as a texture.
#[derive(Clone)]
pub struct Backdrop {
    texture: GlesTexture,
    size: (u32, u32),
    /// The output rect, physical px, the texture stands for: x, y, w, h.
    covers: [f64; 4],
    generation: u64,
}

fn upload_variant<R: GlesAccess>(
    renderer: &mut R,
    v: Rc<Variant>,
    generation: u64,
    size: (u32, u32),
    covers: [f64; 4],
) -> Option<Backdrop> {
    let cached = v.texture.borrow().clone();
    let texture = match cached {
        Some(t) => t,
        None => {
            let gles = renderer.gles_renderer()?;
            let tex = ImportMem::import_memory(
                gles,
                &v.pixels,
                READBACK_FORMAT,
                Size::<i32, Buffer>::from((size.0 as i32, size.1 as i32)),
                false,
            )
            .map_err(|e| warn!("frost: the field did not upload, bars draw flat: {e}"))
            .ok()?;
            *v.texture.borrow_mut() = Some(tex.clone());
            tex
        }
    };
    Some(Backdrop { texture, size, covers, generation })
}

/// The texture for one variant, uploaded the way Lumen's snapshots are: CPU
/// pixels through `import_memory` into the drawing renderer's own type (udev's
/// is a `MultiRenderer`, which can't wrap a texture made elsewhere).
fn backdrop<R: GlesAccess>(renderer: &mut R, dark: bool, inactive: bool) -> Option<Backdrop> {
    let (v, generation, size, covers) = variant(dark, inactive)?;
    upload_variant(renderer, v, generation, size, covers)
}

fn backdrop_f2<R: GlesAccess>(renderer: &mut R, win_id: u32, dark: bool, inactive: bool) -> Option<Backdrop> {
    let (v, generation, size, covers) = variant_f2(win_id, dark, inactive)?;
    upload_variant(renderer, v, generation, size, covers)
}

/// What a bar needs to draw as F1 or F2: the tier, the program and the textures.
pub struct Gear {
    pub tier: Tier,
    program: GlesTexProgram,
    active: Backdrop,
    inactive: Option<Backdrop>,
}

/// F1's gear for this frame, or `None` (the bar draws flat): frost off, no
/// GLES, a failed shader, or nothing captured yet. `need_inactive` is whether
/// the window is not wholly focused, so the inactive variant is made only when
/// something shows it.
pub fn gear<R: GlesAccess>(renderer: &mut R, dark: bool, need_inactive: bool) -> Option<Gear> {
    if mode() == Mode::Off {
        return None;
    }
    let program = shader::frost_program(renderer);
    if choose_tier(mode(), program.is_some(), has_field()) == Tier::F0 {
        return None;
    }
    let program = program?;
    let active = backdrop(renderer, dark, false)?;
    let inactive = if need_inactive { backdrop(renderer, dark, true) } else { None };
    Some(Gear { tier: Tier::F1, program, active, inactive })
}

/// Per-window gear for this frame: chooses `Tier::F2` in `Mode::Full` when the
/// window has a cached F2 backdrop, falling back to `Tier::F1` or `None` (`F0`).
pub fn gear_for_window<R: GlesAccess>(
    window: &WindowElement,
    renderer: &mut R,
    dark: bool,
    need_inactive: bool,
) -> Option<Gear> {
    let m = mode();
    if m == Mode::Off {
        return None;
    }
    let program = shader::frost_program(renderer);
    let win_id = window.frost_window_id();
    let has_f2 = m == Mode::Full && win_id.is_some_and(has_f2_field);
    if choose_bar_tier(m, program.is_some(), has_field(), has_f2) == Tier::F2 {
        if let (Some(program), Some(id)) = (program, win_id) {
            if let Some(active) = backdrop_f2(renderer, id, dark, false) {
                let inactive = if need_inactive { backdrop_f2(renderer, id, dark, true) } else { None };
                return Some(Gear { tier: Tier::F2, program, active, inactive });
            }
        }
    }
    gear(renderer, dark, need_inactive)
}

// === The bars ===

/// The part of the field's texture under a bar: `bar` is the bar's rect on the
/// output, physical px; the answer is in texels, for `Element::src`.
pub fn crop(covers: [f64; 4], texels: (u32, u32), bar: Rectangle<i32, Physical>) -> Rectangle<f64, Buffer> {
    let sx = texels.0 as f64 / covers[2];
    let sy = texels.1 as f64 / covers[3];
    Rectangle::new(
        Point::from(((bar.loc.x as f64 - covers[0]) * sx, (bar.loc.y as f64 - covers[1]) * sy)),
        Size::from((bar.size.w as f64 * sx, bar.size.h as f64 * sy)),
    )
}

/// How strong the grain is: static noise laid over the glass as an overlay.
pub const GRAIN: f32 = 0.09;
/// The rim of light along the bar's top edge, its alpha: light theme, dark.
pub const RIM_LIGHT: f32 = 0.75;
pub const RIM_DARK: f32 = 0.14;
/// The seam along the bottom, the app's accent at this alpha; an inactive
/// window's is fainter.
pub const SEAM: f32 = 0.60;
pub const SEAM_INACTIVE: f32 = 0.22;

pub fn rim_alpha(dark: bool) -> f32 {
    if dark {
        RIM_DARK
    } else {
        RIM_LIGHT
    }
}

/// The identity of one layer of a bar's frost, and the facts that moved its
/// commit counter last.
struct LayerTrack {
    id: Id,
    commit: Cell<CommitCounter>,
    last: RefCell<Option<LayerKey>>,
}

impl Default for LayerTrack {
    fn default() -> Self {
        Self { id: Id::new(), commit: Cell::new(CommitCounter::default()), last: RefCell::new(None) }
    }
}

/// Everything that decides how one layer looks. The element's commit counter
/// moves when (and only when) this does, so a bar at rest over a still
/// wallpaper costs no damage.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LayerKey {
    tier: Tier,
    generation: u64,
    src: [f64; 4],
    size: (i32, i32),
    params: FrostParams,
}

/// A window's frost state, in its user data: the ids its layers keep, where
/// the bar is on the output, its window id, and the tier it was last drawn on.
#[derive(Default)]
pub struct FrostSlot {
    anchor: Cell<Option<Point<i32, Physical>>>,
    win_id: Cell<Option<u32>>,
    tier: Cell<Tier>,
    /// Active, inactive.
    layers: [LayerTrack; 2],
}

impl WindowElement {
    fn frost_slot(&self) -> &FrostSlot {
        self.user_data().insert_if_missing(FrostSlot::default);
        self.user_data().get::<FrostSlot>().expect("just inserted")
    }

    /// Where the window's frame starts on the output, physical px, as drawn
    /// this frame. The bar's frost is a crop of the desktop at that place; a
    /// snapshot of the window (taken with its own render origin) still crops
    /// where the window really is. Set by `render::stack_elements`.
    pub fn set_frost_anchor(&self, at: Point<i32, Physical>) {
        self.frost_slot().anchor.set(Some(at));
    }

    pub fn frost_anchor(&self) -> Option<Point<i32, Physical>> {
        self.frost_slot().anchor.get()
    }

    /// Associate this window element with its compositor `window_id` for per-bar F2 lookup.
    pub fn set_frost_window_id(&self, id: u32) {
        self.frost_slot().win_id.set(Some(id));
    }

    pub fn frost_window_id(&self) -> Option<u32> {
        self.frost_slot().win_id.get()
    }

    /// The tier the bar was last drawn on.
    pub fn frost_tier(&self) -> Tier {
        self.frost_slot().tier.get()
    }

    pub fn set_frost_tier(&self, tier: Tier) {
        self.frost_slot().tier.set(tier);
    }
}

/// What a bar needs from its window and frame to draw its frost.
pub struct BarFacts<'a> {
    /// Where the bar is drawn in this call's coordinates.
    pub geo: Rectangle<i32, Physical>,
    /// The bar's top corners, physical px (the bottom ones are square).
    pub radii: Radii,
    /// Physical px per logical px.
    pub px: f64,
    /// 0 (inactive) .. 1 (focused), mid-crossfade between.
    pub focus: f32,
    pub colors: &'a BarColors,
    pub alpha: f32,
}

/// The frost layers for one window's bar, front to back: the active glass,
/// and, while the window is (or is becoming) inactive, the inactive glass
/// beneath it, the active one fading over it as focus comes. `None` when the
/// bar isn't on screen to crop (never drawn yet).
pub fn bar_elements(window: &WindowElement, gear: &Gear, bar: &BarFacts<'_>) -> Vec<FrostElement> {
    let slot = window.frost_slot();
    let Some(anchor) = slot.anchor.get() else {
        return Vec::new();
    };
    let on_screen = Rectangle::new(anchor, bar.geo.size);
    let focus = bar.focus.clamp(0.0, 1.0);
    let colors = bar.colors;
    let layer = |which: usize, backdrop: &Backdrop, alpha: f32, seam_alpha: f32| {
        let src = crop(backdrop.covers, backdrop.size, on_screen);
        let params = FrostParams {
            radii: bar.radii,
            px: bar.px as f32,
            grain: GRAIN,
            rim: rim_alpha(colors.dark),
            seam: [
                colors.accent[0] as f32 / 255.0,
                colors.accent[1] as f32 / 255.0,
                colors.accent[2] as f32 / 255.0,
                seam_alpha,
            ],
            alpha,
        };
        let track = &slot.layers[which];
        let key = LayerKey {
            tier: gear.tier,
            generation: backdrop.generation,
            src: [src.loc.x, src.loc.y, src.size.w, src.size.h],
            size: (bar.geo.size.w, bar.geo.size.h),
            params,
        };
        if track.last.borrow().as_ref() != Some(&key) {
            let mut commit = track.commit.get();
            commit.increment();
            track.commit.set(commit);
            *track.last.borrow_mut() = Some(key);
        }
        FrostElement::new(
            gear.program.clone(),
            backdrop.texture.clone(),
            track.id.clone(),
            track.commit.get(),
            bar.geo,
            src,
            params,
        )
    };
    let mut out = Vec::with_capacity(2);
    match &gear.inactive {
        // Mid-crossfade (or inactive): the inactive glass beneath, the active
        // one over it at the focus level.
        Some(inactive) if focus < 0.99 => {
            if focus > 0.01 {
                out.push(layer(0, &gear.active, bar.alpha * focus, SEAM));
            }
            out.push(layer(1, inactive, bar.alpha, SEAM_INACTIVE));
        }
        _ => out.push(layer(0, &gear.active, bar.alpha, SEAM)),
    }
    out
}

impl Gear {
    /// Whether a window at `focus` shows the inactive glass at all.
    pub fn wants_inactive(focus: f32) -> bool {
        focus < 0.99
    }
}

// === Capture: the compositor thread's half ===

/// How often the field may be recaptured.
pub const MIN_INTERVAL: Duration = Duration::from_millis(250);

/// When to capture the desktop again: when it has committed (or changed size)
/// since the last capture and the last one is at least `MIN_INTERVAL` old. A
/// change that comes too soon is not lost: it is captured at the first tick
/// after the interval.
#[derive(Debug, Default)]
pub struct Cadence {
    last_at: Option<Duration>,
    last_commit: Option<CommitCounter>,
    last_frame: Option<(i32, i32)>,
}

impl Cadence {
    pub fn due(&self, now: Duration, commit: CommitCounter, frame: (i32, i32), force: bool) -> bool {
        if let Some(at) = self.last_at {
            if now.saturating_sub(at) < MIN_INTERVAL {
                return false;
            }
        }
        force || self.last_commit != Some(commit) || self.last_frame != Some(frame)
    }

    /// A capture was tried at `now` for this desktop state.
    pub fn mark(&mut self, now: Duration, commit: CommitCounter, frame: (i32, i32)) {
        self.last_at = Some(now);
        self.last_commit = Some(commit);
        self.last_frame = Some(frame);
    }
}

const F2_MIN_INTERVAL: Duration = Duration::from_millis(100);
const F2_OVER_BUDGET_COOLDOWN: Duration = Duration::from_secs(1);

#[derive(Clone, PartialEq, Eq)]
struct F2Signature {
    desktop_gen: u64,
    dark: bool,
    bar_rect: (i32, i32, i32, i32),
    lowers: Vec<(u32, (i32, i32, i32, i32), Option<CommitCounter>)>,
}

#[derive(Default)]
struct F2Tracker {
    last_at: Option<Duration>,
    sigs: BTreeMap<u32, F2Signature>,
    cooldown_until: BTreeMap<u32, Duration>,
}

thread_local! {
    static CADENCE: RefCell<Cadence> = RefCell::new(Cadence::default());
    static F2_TRACKER: RefCell<F2Tracker> = RefCell::new(F2Tracker::default());
}

fn desktop_raw() -> Option<(Vec<u8>, (u32, u32), [f64; 4], u64)> {
    FIELD.with(|f| {
        f.borrow()
            .as_ref()
            .map(|field| (field.raw.clone(), field.raw_size, field.raw_covers, field.generation))
    })
}

fn rect_tuple(r: Rectangle<i32, smithay::utils::Logical>) -> (i32, i32, i32, i32) {
    (r.loc.x, r.loc.y, r.size.w, r.size.h)
}

/// One counter for a surface and every subsurface under it. vela paints the
/// page into a subsurface, so the desktop's own toplevel surface hardly ever
/// commits while the wallpaper changes: watching it alone left the frost
/// showing the palette it first saw. Any commit anywhere in the tree moves the
/// sum.
fn tree_commit(surface: &WlSurface) -> CommitCounter {
    let mut sum = 0usize;
    with_surface_tree_downward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        // The walk already holds each surface's states: read the renderer
        // state from them. `with_renderer_surface_state` here would take the
        // same lock again and hang the compositor thread (it did, on device).
        |_, states, _| {
            let commit = states
                .data_map
                .get::<RendererSurfaceStateUserData>()
                .map(|r| r.lock().unwrap().current_commit())
                .unwrap_or_default();
            sum = sum.wrapping_add(commit.distance(Some(CommitCounter::from(0))).unwrap_or(0));
        },
        |_, _, _| true,
    );
    CommitCounter::from(sum)
}

impl<B: Backend> EvoState<B> {
    /// Keep the frost field and `GetFrostState` current. Called every loop pass
    /// (`process_pending_window_actions`); it captures at most four times a
    /// second, and only when the desktop has committed.
    pub fn frost_tick(&mut self) {
        let mut tiers = BTreeMap::new();
        for (&id, w) in &self.windows_by_id {
            if w.is_ssd() {
                w.set_frost_window_id(id);
                tiers.insert(id, w.frost_tier());
            }
        }
        if let Ok(mut book) = TIERS.lock() {
            *book = tiers;
        }
        let m = mode();
        if m == Mode::Off {
            clear_all_f2();
            F2_TRACKER.with(|t| {
                let mut t = t.borrow_mut();
                t.sigs.clear();
                t.cooldown_until.clear();
            });
            return;
        }
        if m != Mode::Full {
            clear_all_f2();
            F2_TRACKER.with(|t| {
                let mut t = t.borrow_mut();
                t.sigs.clear();
                t.cooldown_until.clear();
            });
        }
        let Some(desktop) = self.windows_by_id.values().find(|w| w.is_shell()).cloned() else {
            return;
        };
        let Some(surface) = desktop.wl_surface() else {
            return;
        };
        let has_buffer = with_renderer_surface_state(&surface, |s| s.buffer().is_some()).unwrap_or(false);
        let commit = tree_commit(&surface);
        if !has_buffer {
            return;
        }
        let Some(output) = self.space.outputs().next().cloned() else {
            return;
        };
        let frame = SpaceElement::geometry(&desktop.0).size;
        let now = crate::anim::now();
        let force = FORCE.load(Ordering::Relaxed);
        if CADENCE.with(|c| c.borrow().due(now, commit, (frame.w, frame.h), force)) {
            CADENCE.with(|c| c.borrow_mut().mark(now, commit, (frame.w, frame.h)));
            FORCE.store(false, Ordering::Relaxed);

            let started = Instant::now();
            let output_scale = output.current_scale().fractional_scale();
            let scale = CAPTURE_FRACTION * output_scale;
            if let Some(snapshot) = self.backend_data.snapshot_window(&desktop, scale) {
                // Where the picture is on the output: the buffer's origin (the window's
                // location less its geometry's), the snapshot's own offset from that,
                // and its size, all in physical px.
                let out_origin = self.space.output_geometry(&output).map(|g| g.loc).unwrap_or_default();
                let location = self.space.element_location(&desktop).unwrap_or_default();
                let origin = (location - SpaceElement::geometry(&desktop.0).loc - out_origin)
                    .to_f64()
                    .to_physical(output_scale);
                let covers = [
                    origin.x + snapshot.origin.x as f64 / CAPTURE_FRACTION,
                    origin.y + snapshot.origin.y as f64 / CAPTURE_FRACTION,
                    snapshot.size.w as f64 / CAPTURE_FRACTION,
                    snapshot.size.h as f64 / CAPTURE_FRACTION,
                ];
                let size = (snapshot.size.w.max(0) as u32, snapshot.size.h.max(0) as u32);
                match ingest(snapshot.pixels(), size, covers, scale, crate::shell::identity::dark()) {
                    Ingested::Updated => record_update(started.elapsed().as_secs_f64() * 1000.0),
                    Ingested::Same => {
                        SKIPPED.fetch_add(1, Ordering::Relaxed);
                    }
                    Ingested::Refused => warn!("frost: the desktop capture is unusable ({}x{})", size.0, size.1),
                }
            }
        }

        if m == Mode::Full {
            self.frost_f2_tick(&output, now);
        }
    }

    fn frost_f2_tick(&mut self, output: &Output, now: Duration) {
        let f2_force = F2_FORCE.swap(false, Ordering::Relaxed);
        let due = f2_force
            || F2_TRACKER.with(|t| {
                t.borrow()
                    .last_at
                    .map_or(true, |at| now.saturating_sub(at) >= F2_MIN_INTERVAL)
            });
        if !due {
            return;
        }
        let Some((base_raw, base_size, base_covers, desktop_gen)) = desktop_raw() else {
            return;
        };
        F2_TRACKER.with(|t| t.borrow_mut().last_at = Some(now));

        let output_scale = output.current_scale().fractional_scale();
        let scale = CAPTURE_FRACTION * output_scale;
        let out_origin = self.space.output_geometry(output).map(|g| g.loc).unwrap_or_default();
        let dark = crate::shell::identity::dark();

        // `space.elements()` yields windows in bottom-to-top stacking order.
        let stack: Vec<WindowElement> = self.space.elements().cloned().collect();
        let mut snapshots: Vec<Option<(Vec<u8>, (u32, u32), [f64; 4])>> = vec![None; stack.len()];
        let mut seen_ssd_ids = BTreeSet::new();

        for (i, w) in stack.iter().enumerate() {
            if !w.is_ssd() || self.workspace_manager.is_minimized(w) {
                continue;
            }
            let Some(id) = self.window_id_for(w) else {
                continue;
            };
            seen_ssd_ids.insert(id);
            let Some(w_loc) = self.space.element_location(w) else {
                clear_f2(id);
                continue;
            };
            let bar_rect = Rectangle::new(w_loc, Size::from((w.geometry().size.w, super::ssd::HEADER_BAR_HEIGHT)));
            if bar_rect.size.w <= 0 || bar_rect.size.h <= 0 {
                clear_f2(id);
                continue;
            }

            let mut overlapping = Vec::new();
            let mut lowers_sig = Vec::new();
            for (j, lower) in stack[..i].iter().enumerate() {
                if lower.is_shell() || lower.is_overlay() || self.workspace_manager.is_minimized(lower) {
                    continue;
                }
                let Some(lower_bbox) = self.space.element_bbox(lower) else {
                    continue;
                };
                if !lower_bbox.overlaps(bar_rect) {
                    continue;
                }
                let lower_id = self.window_id_for(lower).unwrap_or(0);
                let lower_commit = lower.wl_surface().map(|s| tree_commit(&s));
                overlapping.push(j);
                lowers_sig.push((lower_id, rect_tuple(lower_bbox), lower_commit));
            }

            if overlapping.is_empty() {
                clear_f2(id);
                F2_TRACKER.with(|t| {
                    t.borrow_mut().sigs.remove(&id);
                });
                continue;
            }

            let in_cooldown = F2_TRACKER.with(|t| {
                t.borrow()
                    .cooldown_until
                    .get(&id)
                    .is_some_and(|&until| now < until)
            });
            if in_cooldown {
                clear_f2(id);
                continue;
            }

            let sig = F2Signature {
                desktop_gen,
                dark,
                bar_rect: rect_tuple(bar_rect),
                lowers: lowers_sig,
            };
            let unchanged = has_f2_field(id)
                && F2_TRACKER.with(|t| t.borrow().sigs.get(&id) == Some(&sig));
            if unchanged {
                continue;
            }

            let started = Instant::now();
            let mut buf = base_raw.clone();
            let mut any_composited = false;
            for j in overlapping {
                if snapshots[j].is_none() {
                    let lower = &stack[j];
                    if let Some(snap) = self.backend_data.snapshot_window(lower, scale) {
                        let lower_loc = self.space.element_location(lower).unwrap_or_default();
                        let lower_origin = (lower_loc - SpaceElement::geometry(&lower.0).loc - out_origin)
                            .to_f64()
                            .to_physical(output_scale);
                        let over_covers = [
                            lower_origin.x + snap.origin.x as f64 / CAPTURE_FRACTION,
                            lower_origin.y + snap.origin.y as f64 / CAPTURE_FRACTION,
                            snap.size.w as f64 / CAPTURE_FRACTION,
                            snap.size.h as f64 / CAPTURE_FRACTION,
                        ];
                        let over_size = (snap.size.w.max(0) as u32, snap.size.h.max(0) as u32);
                        snapshots[j] = Some((snap.pixels().to_vec(), over_size, over_covers));
                    }
                }
                if let Some((over_px, over_size, over_covers)) = &snapshots[j] {
                    composite_rgba(&mut buf, base_size, base_covers, over_px, *over_size, *over_covers);
                    any_composited = true;
                }
            }

            if !any_composited {
                clear_f2(id);
                continue;
            }

            match ingest_f2(id, &buf, base_size, base_covers, scale, dark) {
                Ingested::Updated => {
                    let ms = started.elapsed().as_secs_f64() * 1000.0;
                    if !record_f2_update(ms) {
                        clear_f2(id);
                        F2_TRACKER.with(|t| {
                            let mut t = t.borrow_mut();
                            t.sigs.remove(&id);
                            t.cooldown_until.insert(id, now + F2_OVER_BUDGET_COOLDOWN);
                        });
                    } else {
                        F2_TRACKER.with(|t| {
                            t.borrow_mut().sigs.insert(id, sig);
                        });
                    }
                }
                Ingested::Same => {
                    F2_TRACKER.with(|t| {
                        t.borrow_mut().sigs.insert(id, sig);
                    });
                }
                Ingested::Refused => {
                    clear_f2(id);
                }
            }
        }

        F2_FIELDS.with(|fields| {
            fields.borrow_mut().retain(|id, _| seen_ssd_ids.contains(id));
        });
        F2_TRACKER.with(|t| {
            let mut t = t.borrow_mut();
            t.sigs.retain(|id, _| seen_ssd_ids.contains(id));
            t.cooldown_until.retain(|id, _| seen_ssd_ids.contains(id));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::identity::{contrast, INK_DARK, INK_LIGHT, VEIL_DARK, VEIL_LIGHT};

    /// A sweep over the whole sRGB cube, coarse enough to be quick and fine
    /// enough to meet every hue at several lightnesses.
    fn sweep() -> impl Iterator<Item = [u8; 3]> {
        let steps: Vec<u8> = (0..=255u16).step_by(17).map(|v| v as u8).collect();
        steps.clone().into_iter().flat_map(move |r| {
            let steps = steps.clone();
            steps.clone().into_iter().flat_map(move |g| steps.clone().into_iter().map(move |b| [r, g, b]))
        })
    }

    /// OKLab and sRGB bytes round-trip to within a code or two (the encoder is
    /// a table), so lightness measured on a result is the lightness it has.
    const L_SLACK: f32 = 0.004;
    const C_SLACK: f32 = 0.006;

    #[test]
    fn the_fast_cube_root_is_a_cube_root() {
        for i in 0..=2000 {
            let x = (i as f32 / 2000.0).powi(3);
            let want = x.cbrt();
            assert!((cbrt(x) - want).abs() <= 2e-6 + want * 2e-6, "{x}: {} vs {want}", cbrt(x));
        }
        assert_eq!(cbrt(0.0), 0.0);
        assert_eq!(cbrt(-1.0), 0.0);
    }

    #[test]
    fn oklab_round_trips_through_srgb_bytes() {
        for rgb in sweep().step_by(7) {
            let back = srgb_of(oklab(rgb));
            for i in 0..3 {
                assert!((back[i] as i32 - rgb[i] as i32).abs() <= 1, "{rgb:?} -> {back:?}");
            }
        }
        assert!((lightness([255, 255, 255]) - 1.0).abs() < 0.01);
        assert!(lightness([0, 0, 0]).abs() < 0.01);
    }

    #[test]
    fn the_lightness_band_holds_for_any_backdrop() {
        for inactive in [false, true] {
            for rgb in sweep() {
                let light = lightness(veil(rgb, &VEIL_LIGHT, inactive));
                assert!(light >= 0.86 - L_SLACK, "light {rgb:?} inactive={inactive}: L={light}");
                let dark = lightness(veil(rgb, &VEIL_DARK, inactive));
                assert!(dark <= 0.30 + L_SLACK, "dark {rgb:?} inactive={inactive}: L={dark}");
            }
        }
    }

    #[test]
    fn the_fixed_ink_clears_seven_to_one_over_any_backdrop() {
        for inactive in [false, true] {
            for rgb in sweep() {
                let light = veil(rgb, &VEIL_LIGHT, inactive);
                assert!(contrast(light, INK_DARK) >= 7.0, "dark ink on {light:?} (from {rgb:?})");
                let dark = veil(rgb, &VEIL_DARK, inactive);
                assert!(contrast(dark, INK_LIGHT) >= 7.0, "light ink on {dark:?} (from {rgb:?})");
            }
        }
    }

    #[test]
    fn the_grain_cannot_cost_the_contrast_either() {
        // The overlay at 9% moves a channel by at most 0.09 of its distance to
        // 0 or 1 (overlay of an extreme noise value), so take the worst of
        // both and ask for 7:1 still, for the lightest dark-theme and the
        // darkest light-theme result.
        let worst = |c: [u8; 3], toward: f32| c.map(|v| (v as f32 + (toward - v as f32) * GRAIN).round() as u8);
        for rgb in sweep().step_by(5) {
            let light = veil(rgb, &VEIL_LIGHT, false);
            assert!(contrast(worst(light, 0.0), INK_DARK) >= 7.0, "{light:?}");
            let dark = veil(rgb, &VEIL_DARK, false);
            assert!(contrast(worst(dark, 255.0), INK_LIGHT) >= 7.0, "{dark:?}");
        }
    }

    #[test]
    fn chroma_is_capped_and_the_inactive_glass_keeps_little_of_it() {
        for rgb in sweep() {
            let c = chroma(veil(rgb, &VEIL_LIGHT, false));
            assert!(c <= 0.06 + C_SLACK, "light {rgb:?}: C={c}");
            let c = chroma(veil(rgb, &VEIL_DARK, false));
            assert!(c <= 0.07 + C_SLACK, "dark {rgb:?}: C={c}");
            let c = chroma(veil(rgb, &VEIL_LIGHT, true));
            assert!(c <= 0.06 * INACTIVE_CHROMA as f32 + C_SLACK, "inactive {rgb:?}: C={c}");
        }
    }

    #[test]
    fn the_wallpapers_hue_carries() {
        // A warm backdrop gives a warm light bar and a cool one a cool bar.
        let warm = veil([0xe8, 0x9a, 0x3c], &VEIL_LIGHT, false);
        assert!(warm[0] > warm[2], "{warm:?}");
        let cool = veil([0x3c, 0x6a, 0xe8], &VEIL_LIGHT, false);
        assert!(cool[2] > cool[0], "{cool:?}");
        let violet = veil([0x7c, 0x3a, 0xed], &VEIL_DARK, false);
        assert!(violet[2] > violet[1], "{violet:?}");
        // A bright colour does not shout: the chroma is well under the saturated input's.
        assert!(chroma(warm) < chroma([0xe8, 0x9a, 0x3c]) / 2.0);
    }

    #[test]
    fn inactive_takes_more_veil_and_less_colour() {
        let backdrop = [0xe8, 0x9a, 0x3c];
        let on = veil(backdrop, &VEIL_LIGHT, false);
        let off = veil(backdrop, &VEIL_LIGHT, true);
        assert!(chroma(off) < chroma(on) * 0.5);
        // Toward the target: a dark backdrop in the light theme is lighter when inactive... or equal at the band.
        assert!(lightness(off) >= lightness(on) - 0.002);
    }

    #[test]
    fn a_mid_grey_stays_grey_on_the_light_bar_and_goes_indigo_on_the_dark_one() {
        let g = veil([128, 128, 128], &VEIL_LIGHT, false);
        assert!(chroma(g) < 0.02, "{g:?}");
        // The dark theme's colour is a deep indigo, and half of it is the veil.
        let d = veil([128, 128, 128], &VEIL_DARK, false);
        assert!(d[2] > d[0], "{d:?}");
    }

    #[test]
    fn box_sizes_make_a_gaussian_of_the_right_width() {
        let sizes = box_sizes(5.0, 3);
        assert_eq!(sizes.len(), 3);
        assert!(sizes.iter().all(|s| s % 2 == 1), "odd widths: {sizes:?}");
        // The variance of the composite is the sum of the boxes' (w^2 - 1) / 12.
        let var: f32 = sizes.iter().map(|&s| ((s * s) as f32 - 1.0) / 12.0).sum();
        assert!((var.sqrt() - 5.0).abs() < 0.25, "sigma {}", var.sqrt());
    }

    #[test]
    fn the_blur_of_an_impulse_is_a_gaussian_that_keeps_its_light() {
        let (w, h) = (81usize, 41usize);
        let mut buf = vec![[0f32; 3]; w * h];
        buf[20 * w + 40] = [100.0, 50.0, 0.0];
        gaussian_blur(&mut buf, w, h, 5.0);
        let total: f32 = buf.iter().map(|p| p[0]).sum();
        assert!((total - 100.0).abs() < 0.05, "mass {total}");
        let green: f32 = buf.iter().map(|p| p[1]).sum();
        assert!((green - 50.0).abs() < 0.05);
        // Standard deviation along the row through the impulse.
        let row: Vec<f32> = (0..w).map(|x| buf[20 * w + x][0]).collect();
        let mass: f32 = row.iter().sum();
        let mean: f32 = row.iter().enumerate().map(|(x, v)| x as f32 * v).sum::<f32>() / mass;
        let var: f32 = row.iter().enumerate().map(|(x, v)| (x as f32 - mean).powi(2) * v).sum::<f32>() / mass;
        assert!((var.sqrt() - 5.0).abs() < 0.6, "sigma {}", var.sqrt());
        assert!((mean - 40.0).abs() < 0.01, "centred: {mean}");
        // Its peak is far under the impulse.
        let peak = buf.iter().map(|p| p[0]).fold(0.0, f32::max);
        assert!(peak < 3.0, "peak {peak}");
    }

    #[test]
    fn a_flat_picture_stays_flat_edges_included() {
        let (w, h) = (24usize, 16usize);
        let mut buf = vec![[90.0f32, 120.0, 200.0]; w * h];
        gaussian_blur(&mut buf, w, h, 4.0);
        for p in &buf {
            for (v, want) in p.iter().zip([90.0, 120.0, 200.0]) {
                assert!((v - want).abs() < 1e-3, "{p:?}");
            }
        }
    }

    #[test]
    fn downsampling_averages_blocks_and_drops_a_ragged_edge() {
        let buf: Vec<[f32; 3]> = (0..5 * 3).map(|i| [i as f32, 0.0, 100.0]).collect();
        let (out, w, h) = downsample(&buf, 5, 3, 2);
        assert_eq!((w, h), (2, 1));
        // Block (0,0) is source 0, 1, 5, 6; block (1,0) is 2, 3, 7, 8.
        assert_eq!(out[0], [3.0, 0.0, 100.0]);
        assert_eq!(out[1], [5.0, 0.0, 100.0]);
    }

    #[test]
    fn a_zero_blur_changes_nothing() {
        let mut buf = vec![[1.0f32, 2.0, 3.0]; 16];
        buf[5] = [9.0, 9.0, 9.0];
        let before = buf.clone();
        gaussian_blur(&mut buf, 4, 4, 0.0);
        assert_eq!(buf, before);
    }

    fn picture(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let c = f(x, y);
                px.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        px
    }

    #[test]
    fn a_capture_is_blurred_veiled_and_counted_only_when_it_changes() {
        let (w, h) = (32u32, 20u32);
        let covers = [0.0, 0.0, 128.0, 80.0];
        let warm = picture(w, h, |x, _| if x < 16 { [0xe8, 0x9a, 0x3c] } else { [0x3c, 0x6a, 0xe8] });
        assert_eq!(ingest(&warm, (w, h), covers, 0.25, false), Ingested::Updated);
        let g1 = generation();
        assert_eq!(g1, 1);
        assert_eq!(ingest(&warm, (w, h), covers, 0.25, false), Ingested::Same, "identical pixels");
        assert_eq!(generation(), g1);
        // The picture standing for another place on the output is a change.
        assert_eq!(ingest(&warm, (w, h), [4.0, 0.0, 128.0, 80.0], 0.25, false), Ingested::Updated);
        let g2 = generation();
        assert_eq!(g2, g1 + 1);
        // The boundary is soft after the blur: the middle is neither colour.
        let (v, _, texels, _) = variant(false, false).unwrap();
        assert_eq!(texels, (16, 10), "the capture, halved");
        let tw = texels.0 as usize;
        let px = |x: usize, y: usize| {
            let i = (y * tw + x) * 4;
            [v.pixels[i], v.pixels[i + 1], v.pixels[i + 2]]
        };
        assert!(px(0, 5)[0] > px(0, 5)[2], "the left is warm");
        assert!(px(15, 5)[2] > px(15, 5)[0], "the right is cool");
        assert!(chroma(px(8, 5)) < chroma(px(1, 5)), "the seam between them is greyer than either side");
        assert!(v.pixels.chunks_exact(4).all(|p| p[3] == 255), "opaque");
        // Every texel of the frost is inside the light band.
        for p in v.pixels.chunks_exact(4) {
            assert!(lightness([p[0], p[1], p[2]]) >= 0.86 - L_SLACK);
        }
    }

    #[test]
    fn an_odd_sized_capture_is_covered_by_what_the_blocks_reached() {
        let odd = picture(33, 21, |_, _| [9, 9, 9]);
        assert_eq!(ingest(&odd, (33, 21), [0.0, 0.0, 132.0, 84.0], 0.25, false), Ingested::Updated);
        let (_, _, texels, covers) = variant(false, false).unwrap();
        assert_eq!(texels, (16, 10));
        assert_eq!((covers[2], covers[3]), (132.0 * 32.0 / 33.0, 84.0 * 20.0 / 21.0));
    }

    #[test]
    fn a_theme_change_reveils_the_same_picture_as_a_new_generation() {
        let (w, h) = (16u32, 12u32);
        let img = picture(w, h, |_, _| [0x80, 0x90, 0xa0]);
        ingest(&img, (w, h), [0.0, 0.0, 64.0, 48.0], 0.25, false);
        let g = generation();
        let (light, ..) = variant(false, false).unwrap();
        let (dark, g_dark, ..) = variant(true, false).unwrap();
        assert!(g_dark > g, "the bars' counters move");
        assert_ne!(light.pixels[..3], dark.pixels[..3]);
        assert!(lightness([dark.pixels[0], dark.pixels[1], dark.pixels[2]]) <= 0.30 + L_SLACK);
        // The inactive variant is made on request and is not the active one.
        let (off, ..) = variant(true, true).unwrap();
        assert_ne!(off.pixels[..3], dark.pixels[..3]);
        // Asking again for the same one is the same pixels, not a new generation.
        let (_, g_again, ..) = variant(true, true).unwrap();
        assert_eq!(g_again, g_dark);
    }

    #[test]
    fn a_bad_capture_is_refused() {
        assert_eq!(ingest(&[0; 16], (2, 2), [0.0, 0.0, 8.0, 8.0], 0.25, false), Ingested::Refused);
        assert_eq!(ingest(&[0; 15], (4, 4), [0.0, 0.0, 8.0, 8.0], 0.25, false), Ingested::Refused, "wrong length");
        let img = picture(8, 8, |_, _| [1, 2, 3]);
        assert_eq!(ingest(&img, (8, 8), [0.0, 0.0, 0.0, 8.0], 0.25, false), Ingested::Refused, "covers nothing");
    }

    #[test]
    fn a_bar_crops_the_field_where_it_sits_on_the_output() {
        // 320x200 texels stand for the 1280x800 output: x4.
        let covers = [0.0, 0.0, 1280.0, 800.0];
        let bar = Rectangle::new(Point::from((400, 100)), Size::from((640, 32)));
        let src = crop(covers, (320, 200), bar);
        assert_eq!((src.loc.x, src.loc.y, src.size.w, src.size.h), (100.0, 25.0, 160.0, 8.0));
        // A field that stands for a window not at the origin shifts the crop.
        let src = crop([100.0, 40.0, 1280.0, 800.0], (320, 200), bar);
        assert_eq!((src.loc.x, src.loc.y), (75.0, 15.0));
        // Beyond the field is left to the texture's edge clamp.
        let off = crop(covers, (320, 200), Rectangle::new(Point::from((-40, -8)), Size::from((100, 32))));
        assert_eq!((off.loc.x, off.loc.y), (-10.0, -2.0));
    }

    #[test]
    fn tiers_fall_back_to_flat() {
        assert_eq!(choose_tier(Mode::Wallpaper, true, true), Tier::F1);
        assert_eq!(choose_tier(Mode::Wallpaper, false, true), Tier::F0, "shader failed");
        assert_eq!(choose_tier(Mode::Wallpaper, true, false), Tier::F0, "nothing captured");
        assert_eq!(choose_tier(Mode::Off, true, true), Tier::F0);
        assert_eq!(choose_tier(Mode::Full, true, true), Tier::F1, "F2 is reserved; full is wallpaper for now");
    }

    #[test]
    fn modes_parse_and_name_themselves() {
        for m in [Mode::Off, Mode::Wallpaper, Mode::Full] {
            assert_eq!(Mode::parse(m.as_str()), Some(m));
            assert_eq!(Mode::from_code(m.code()), m);
        }
        assert_eq!(Mode::parse(" Wallpaper "), Some(Mode::Wallpaper));
        assert_eq!(Mode::parse("frosty"), None);
    }

    fn commit(n: u32) -> CommitCounter {
        let mut c = CommitCounter::default();
        for _ in 0..n {
            c.increment();
        }
        c
    }

    #[test]
    fn the_desktop_is_captured_when_it_commits_and_at_most_four_times_a_second() {
        let ms = Duration::from_millis;
        let mut c = Cadence::default();
        let frame = (1280, 800);
        // The first look captures.
        assert!(c.due(ms(0), commit(1), frame, false));
        c.mark(ms(0), commit(1), frame);
        // Nothing moved: never, however long it waits (a still wallpaper costs nothing).
        assert!(!c.due(ms(10_000), commit(1), frame, false));
        // A commit inside the interval waits for it, and is then taken.
        assert!(!c.due(ms(100), commit(2), frame, false));
        assert!(!c.due(ms(249), commit(2), frame, false));
        assert!(c.due(ms(250), commit(2), frame, false));
        // Ten commits a second is four captures a second.
        let mut taken = 0;
        let mut n = 2;
        c.mark(ms(250), commit(2), frame);
        for t in (250..1250).step_by(16) {
            n += 1;
            if c.due(ms(t), commit(n), frame, false) {
                c.mark(ms(t), commit(n), frame);
                taken += 1;
            }
        }
        assert!((3..=4).contains(&taken), "{taken}");
        // A new size is a change though the counter hasn't moved; force is too.
        assert!(c.due(ms(5_000), commit(n), (1920, 1080), false));
        assert!(c.due(ms(5_000), commit(n), frame, true));
    }

    #[test]
    fn the_state_names_what_the_suites_read() {
        let v = state_json();
        for key in ["mode", "program", "tiers", "updates", "f2_updates", "last_ms", "over_budget"] {
            assert!(v.get(key).is_some(), "{key}");
        }
        assert_eq!(v["mode"], "wallpaper");
        assert!(v["updates"].get("f1").is_some());
        assert_eq!(v["updates"].as_object().unwrap().len(), 1, "alpha5-frost.txt expects only f1 inside updates");
        assert!(["untried", "compiled", "failed"].contains(&v["program"].as_str().unwrap()));
    }

    #[test]
    fn bar_tiers_pick_f2_when_full_with_backdrop_and_fall_back_to_f1_or_f0() {
        assert_eq!(Tier::F2.as_str(), "f2");
        assert_eq!(choose_bar_tier(Mode::Full, true, true, true), Tier::F2);
        assert_eq!(choose_bar_tier(Mode::Full, true, true, false), Tier::F1);
        assert_eq!(choose_bar_tier(Mode::Full, true, false, false), Tier::F0);
        assert_eq!(choose_bar_tier(Mode::Full, false, true, true), Tier::F0);
        assert_eq!(choose_bar_tier(Mode::Wallpaper, true, true, true), Tier::F1);
        assert_eq!(choose_bar_tier(Mode::Off, true, true, true), Tier::F0);
    }

    #[test]
    fn composite_rgba_blends_in_z_order_and_clips_to_overlap() {
        // Base: 8x4 covering [0, 0, 32, 16] (4 physical px per texel), solid blue.
        let mut base = picture(8, 4, |_, _| [0, 0, 200]);
        let base_covers = [0.0, 0.0, 32.0, 16.0];
        // Lower window: 4x4 covering [8, 0, 16, 16], opaque red.
        let lower = picture(4, 4, |_, _| [200, 0, 0]);
        composite_rgba(&mut base, (8, 4), base_covers, &lower, (4, 4), [8.0, 0.0, 16.0, 16.0]);
        // Upper window (translucent ~50% premultiplied green): covers [16, 0, 24, 16] (extends past right edge).
        let mut upper = vec![0u8; 6 * 4 * 4];
        for px in upper.chunks_exact_mut(4) {
            px.copy_from_slice(&[0, 128, 0, 128]);
        }
        composite_rgba(&mut base, (8, 4), base_covers, &upper, (6, 4), [16.0, 0.0, 24.0, 16.0]);

        let px_at = |x: usize, y: usize| -> [u8; 4] {
            let i = (y * 8 + x) * 4;
            [base[i], base[i + 1], base[i + 2], base[i + 3]]
        };
        // x=0,1: untouched blue wallpaper.
        assert_eq!(px_at(0, 1), [0, 0, 200, 255]);
        assert_eq!(px_at(1, 1), [0, 0, 200, 255]);
        // x=2,3: opaque lower red window.
        assert_eq!(px_at(2, 1), [200, 0, 0, 255]);
        assert_eq!(px_at(3, 1), [200, 0, 0, 255]);
        // x=4,5: upper translucent green over lower red.
        let blend_red_green = px_at(4, 1);
        assert!(blend_red_green[0] > 90 && blend_red_green[0] < 110, "{blend_red_green:?}");
        assert_eq!(blend_red_green[1], 128);
        assert_eq!(blend_red_green[2], 0);
        // x=6,7: upper translucent green over blue wallpaper (clipped at base right edge).
        let blend_blue_green = px_at(7, 1);
        assert_eq!(blend_blue_green[0], 0);
        assert_eq!(blend_blue_green[1], 128);
        assert!(blend_blue_green[2] > 90 && blend_blue_green[2] < 110, "{blend_blue_green:?}");
    }

    #[test]
    fn f2_ingest_clear_and_budget_fallback_work() {
        let win_id = 4242;
        clear_f2(win_id);
        assert!(!has_f2_field(win_id));

        let (w, h) = (16u32, 8u32);
        let covers = [0.0, 0.0, 64.0, 32.0];
        let mut base = picture(w, h, |_, _| [20, 40, 200]);
        let over = picture(8, 8, |_, _| [220, 120, 20]);
        composite_rgba(&mut base, (w, h), covers, &over, (8, 8), [0.0, 0.0, 32.0, 32.0]);

        assert_eq!(ingest_f2(win_id, &base, (w, h), covers, 0.25, false), Ingested::Updated);
        assert!(has_f2_field(win_id));
        assert_eq!(ingest_f2(win_id, &base, (w, h), covers, 0.25, false), Ingested::Same);

        let (v, g1, ..) = variant_f2(win_id, false, false).unwrap();
        // Left side is warm (composited over window), right side is cool (wallpaper).
        let left = [v.pixels[0], v.pixels[1], v.pixels[2]];
        let right_idx = 7 * 4;
        let right = [v.pixels[right_idx], v.pixels[right_idx + 1], v.pixels[right_idx + 2]];
        assert!(left[0] > left[2], "warm over window: {left:?}");
        assert!(right[2] > right[0], "cool wallpaper: {right:?}");

        // Theme switch re-veils F2 field with a new generation.
        let (_, g2, ..) = variant_f2(win_id, true, false).unwrap();
        assert!(g2 > g1);

        // Within-budget update keeps F2; over-budget drops F2 back to F1.
        let f2_before = UPDATES_F2.load(Ordering::Relaxed);
        let ob_before = OVER_BUDGET.load(Ordering::Relaxed);
        assert!(record_f2_update(BUDGET_MS - 1.0));
        assert_eq!(UPDATES_F2.load(Ordering::Relaxed) - f2_before, 1);
        assert_eq!(OVER_BUDGET.load(Ordering::Relaxed), ob_before);

        assert!(!record_f2_update(BUDGET_MS + 1.0));
        assert_eq!(UPDATES_F2.load(Ordering::Relaxed) - f2_before, 2);
        assert_eq!(OVER_BUDGET.load(Ordering::Relaxed) - ob_before, 1);

        clear_f2(win_id);
        assert!(!has_f2_field(win_id));
        assert_eq!(choose_bar_tier(Mode::Full, true, true, has_f2_field(win_id)), Tier::F1);
    }

    /// The cost of an update at the size the device uses, for the report
    /// (`cargo test --release field_update_cost -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn field_update_cost() {
        let (w, h) = (320u32, 200u32);
        // Pastel, smooth, a little of every hue: what Daylight looks like.
        let img = picture(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            [(150.0 + 90.0 * fx) as u8, (140.0 + 70.0 * fy) as u8, (190.0 - 60.0 * fx * fy) as u8]
        });
        let mut worst = 0f64;
        let mut total = 0f64;
        for i in 0..20u32 {
            let mut img = img.clone();
            img[0] = i as u8; // never identical
            let t = Instant::now();
            ingest(&img, (w, h), [0.0, 0.0, 1280.0, 800.0], 0.25, i % 2 == 0);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            worst = worst.max(ms);
            total += ms;
        }
        println!("field update at {w}x{h}: mean {:.2} ms, worst {worst:.2} ms", total / 20.0);
        let blurred: Vec<[f32; 3]> = img.chunks_exact(4).map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
        let t = Instant::now();
        let (mut small, sw, sh) = downsample(&blurred, w as usize, h as usize, FIELD_DOWNSAMPLE);
        gaussian_blur(&mut small, sw, sh, 2.5);
        println!("  halve and blur alone: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let v = make_variant(&small, &veil_params(false), false);
        println!("  veil alone: {:.2} ms ({} bytes)", t.elapsed().as_secs_f64() * 1000.0, v.pixels.len());
    }
}
