//! Lumen: the shared snapshot-draw program every Lumen effect draws through,
//! and the switch, status and counters `evo.GetLumenState` reports.
//! Motion spec: `docs/internal/evo-animation-plan.md`, "Lumen".
//!
//! # What it draws
//!
//! One window picture (a [`Snapshot`], uploaded once with mipmaps) into a
//! rect, with, per frame ([`Params`]):
//!
//! - a **rounded clip** of any radius, up to a full circle (clamped to half
//!   the short side), anti-aliased over a pixel;
//! - a **blur** by mip LOD bias plus four taps: no extra passes, no
//!   framebuffer. `blur` is in output pixels and reads as roughly that radius;
//! - an accent **tint** (0 the picture, 1 an orb of the accent that keeps a
//!   little of the picture's light and dark, with a soft lens highlight when
//!   the shape is round);
//! - an outer **glow** of the accent, `glow_radius` px wide;
//! - **alpha**;
//! - **squash and stretch**: the shape scaled by `stretch` along `angle` and by
//!   `1 / stretch` across it (area kept), done to the quad's corners so the
//!   texture follows exactly.
//!
//! # Using it (other lanes)
//!
//! ```ignore
//! // When the effect begins (on the compositor thread, with the snapshot):
//! let source = lumen::Source::new(snapshot);
//! // Each frame, `place` being where the snapshot's top-left pixel sits
//! // (output-physical, e.g. render_origin + snapshot.origin):
//! let mut p = lumen::Params::picture(&source, place);   // the plain picture
//! p.size = [p.size[0] * s, p.size[1] * s];              // scale about its centre
//! p.center[1] += drift_px;
//! p.blur = 10.0 * scale * t;
//! p.alpha = 1.0 - t;
//! let element = source.element(renderer, &p, lumen::Effect::Recede)?; // None: fall back
//! ```
//!
//! `element` returns `None` when Lumen is off, the backend has no GLES, the
//! program failed to build or the upload failed: draw your plain fallback then.
//! Each element drawn counts a frame for its [`Effect`]; call
//! [`record_progress`] with your effect's progress so suites can shoot at
//! 25/50/75%. [`LumenElement`] is a `RenderElement<R>` for any
//! `R: GlesAccess`; `GhostElement::Warp` already holds one.
//!
//! The GL gotchas of the old warp apply here and are handled: smithay leaves
//! `VertexAttribDivisor 1` on attribute slots (reset to 0 before drawing),
//! damage is honoured per rect with a scissor converted through the frame's
//! projection, and every piece of state touched is put back.
//!
//! # Switch
//!
//! `EVO_LUMEN=off` at start, `evo.SetLumen {enabled}` after. Off, every effect
//! draws its plain fallback (lane B's genie-lite for minimize). Reduced mode
//! never gets here (opacity only); off mode animates nothing.

use std::{
    cell::{Cell, RefCell},
    ffi::CString,
    sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
};

use serde_json::{json, Value};
use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{ffi, GlesError, GlesTexture},
        utils::{CommitCounter, OpaqueRegions},
        ImportMem, Texture,
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Size},
};
use tracing::{info, warn};

use super::{
    shader::GlesAccess,
    snapshot::{Snapshot, READBACK_FORMAT},
};

// === Effects, counters, switch ===

/// The Lumen effects, for the counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Bloom,
    Droplet,
    Evaporate,
    Recede,
    Glint,
}

impl Effect {
    pub const ALL: [Effect; 5] = [Effect::Bloom, Effect::Droplet, Effect::Evaporate, Effect::Recede, Effect::Glint];

    pub fn name(self) -> &'static str {
        match self {
            Effect::Bloom => "bloom",
            Effect::Droplet => "droplet",
            Effect::Evaporate => "evaporate",
            Effect::Recede => "recede",
            Effect::Glint => "glint",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

static FRAMES: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
/// Last progress per effect, as f64 bits.
static PROGRESS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// Count one frame drawn through `effect`. [`LumenElement`] does this itself
/// when it draws; an effect drawn by a program of its own calls it.
pub fn count_frame(effect: Effect) {
    FRAMES[effect.index()].fetch_add(1, Ordering::Relaxed);
}

pub fn frames(effect: Effect) -> u64 {
    FRAMES[effect.index()].load(Ordering::Relaxed)
}

/// The progress (0..1) of the last frame of `effect`: what a suite polls to
/// take a shot part-way.
pub fn record_progress(effect: Effect, progress: f64) {
    PROGRESS[effect.index()].store(progress.to_bits(), Ordering::Relaxed);
}

/// [`count_frame`] and [`record_progress`] together, for an effect drawn by
/// a program of its own.
pub fn record_frame(effect: Effect, progress: f64) {
    count_frame(effect);
    record_progress(effect, progress);
}

pub fn last_progress(effect: Effect) -> f64 {
    f64::from_bits(PROGRESS[effect.index()].load(Ordering::Relaxed))
}

static ENABLED: AtomicBool = AtomicBool::new(true);
static ENV_READ: std::sync::Once = std::sync::Once::new();
/// 0 not tried, 1 compiled, 2 failed (or no GLES to compile on).
static STATUS: AtomicU8 = AtomicU8::new(0);
static MIPMAPS: AtomicBool = AtomicBool::new(true);

/// Whether Lumen is switched on (`EVO_LUMEN`, `SetLumen`).
pub fn enabled() -> bool {
    ENV_READ.call_once(|| {
        if std::env::var("EVO_LUMEN")
            .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "0" | "none" | "false"))
        {
            ENABLED.store(false, Ordering::Relaxed);
            info!("lumen: off (EVO_LUMEN), effects draw their plain fallbacks");
        }
    });
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    enabled();
    if ENABLED.swap(on, Ordering::Relaxed) != on {
        info!("lumen: {}", if on { "on" } else { "off, effects draw their plain fallbacks" });
    }
}

/// Whether an effect starting now should take a snapshot for Lumen: switched
/// on, and the program has not already failed to build.
pub fn usable() -> bool {
    enabled() && STATUS.load(Ordering::Relaxed) != 2
}

/// Whether new uploads get mipmaps (the blur needs them; on by default).
pub fn set_mipmaps(on: bool) {
    MIPMAPS.store(on, Ordering::Relaxed);
}

/// `"untried"`, `"compiled"` or `"failed"`.
pub fn status() -> &'static str {
    match STATUS.load(Ordering::Relaxed) {
        1 => "compiled",
        2 => "failed",
        _ => "untried",
    }
}

/// `evo.GetLumenState`'s answer. `extra` fields (a signal count, another
/// lane's own program status) are merged in by the caller.
pub fn state_json() -> Value {
    let mut frames = serde_json::Map::new();
    let mut progress = serde_json::Map::new();
    for e in Effect::ALL {
        frames.insert(e.name().into(), json!(self::frames(e)));
        progress.insert(e.name().into(), json!(last_progress(e)));
    }
    json!({
        "enabled": enabled(),
        "program": status(),
        "frames": frames,
        "last_progress": progress,
    })
}

// === Accent ===

/// Pyxis's own accent, the hue a colourless window borrows.
pub const DEFAULT_ACCENT: [f32; 3] = [0.42, 0.55, 1.0];

/// Parse `"#rrggbb"` (or `"rrggbb"`) to linear-ish 0..1 floats.
pub fn parse_hex(s: &str) -> Option<[f32; 3]> {
    let h = s.trim().strip_prefix('#').unwrap_or(s.trim());
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(|b| b as f32 / 255.0);
    Some([byte(0)?, byte(2)?, byte(4)?])
}

fn rgb_to_hsl([r, g, b]: [f32; 3]) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-6);
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s.min(1.0), l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(1.0) * 6.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r + m, g + m, b + m]
}

/// A window's average colour made into an accent: pushed toward saturation
/// and a middling lightness, so a white or grey window doesn't make a grey
/// orb. A colourless average borrows [`DEFAULT_ACCENT`]'s hue, in proportion
/// to how colourless it is.
pub fn accent_from_average(avg: [f32; 3]) -> [f32; 3] {
    let (h, s, l) = rgb_to_hsl(avg);
    let chroma = avg.iter().cloned().fold(0.0f32, f32::max) - avg.iter().cloned().fold(1.0f32, f32::min);
    let (dh, ds, _) = rgb_to_hsl(DEFAULT_ACCENT);
    // Below ~0.18 of chroma the hue is noise: lean on the default.
    let own = (chroma / 0.18).clamp(0.0, 1.0);
    let s = (s * own + ds * (1.0 - own)).max(0.55);
    // Interpolate hue the short way round.
    let mut dhue = h - dh;
    if dhue > 0.5 {
        dhue -= 1.0;
    } else if dhue < -0.5 {
        dhue += 1.0;
    }
    let h = dh + dhue * own;
    let l = l.clamp(0.48, 0.64);
    hsl_to_rgb(h, s, l)
}

/// The average colour of premultiplied RGBA bytes (alpha-weighted), sampled
/// sparsely. `None` for a fully transparent picture.
pub fn average_rgba(px: &[u8]) -> Option<[f32; 3]> {
    let n = px.len() / 4;
    let step = (n / 4096).max(1);
    let (mut r, mut g, mut b, mut a) = (0u64, 0u64, 0u64, 0u64);
    for i in (0..n).step_by(step) {
        r += px[i * 4] as u64;
        g += px[i * 4 + 1] as u64;
        b += px[i * 4 + 2] as u64;
        a += px[i * 4 + 3] as u64;
    }
    (a > 0).then(|| [r as f32 / a as f32, g as f32 / a as f32, b as f32 / a as f32].map(|c| c.min(1.0)))
}

/// A snapshot's accent: its average colour through [`accent_from_average`],
/// or [`DEFAULT_ACCENT`] for a fully transparent picture.
pub fn accent_of(snapshot: &Snapshot) -> [f32; 3] {
    average_rgba(snapshot.pixels()).map_or(DEFAULT_ACCENT, accent_from_average)
}

// === The program ===

const VERTEX: &str = r#"
uniform mat3 u_proj;
attribute vec2 a_pos;
attribute vec2 a_local;
varying vec2 v_local;
void main() {
    vec3 p = u_proj * vec3(a_pos, 1.0);
    gl_Position = vec4(p.xy, 0.0, 1.0);
    v_local = a_local;
}
"#;

/// `v_local` is the fragment's place in the unstretched shape, in pixels from
/// its centre. Output is premultiplied (blend ONE, ONE_MINUS_SRC_ALPHA).
const FRAGMENT: &str = r#"
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
uniform sampler2D u_tex;
uniform vec2 u_half;
uniform float u_radius;
uniform vec4 u_uv;
uniform float u_bias;
uniform vec2 u_tap;
uniform vec3 u_accent;
uniform float u_tint;
uniform float u_glow;
uniform float u_glow_r;
uniform float u_alpha;
varying vec2 v_local;

float rbox(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

vec4 sample_at(vec2 uv) {
    return texture2D(u_tex, clamp(uv, min(u_uv.xy, u_uv.zw), max(u_uv.xy, u_uv.zw)), u_bias);
}

void main() {
    float d = rbox(v_local, u_half, u_radius);
    float cov = clamp(0.5 - d, 0.0, 1.0);
    vec4 body = vec4(0.0);
    if (cov > 0.0) {
        vec2 t = clamp(v_local / (2.0 * u_half) + 0.5, 0.0, 1.0);
        vec2 uv = mix(u_uv.xy, u_uv.zw, t);
        vec4 c = sample_at(uv);
        if (u_tap.x > 0.0 || u_tap.y > 0.0) {
            c = c * 2.0
              + sample_at(uv + vec2( u_tap.x,  u_tap.y))
              + sample_at(uv + vec2(-u_tap.x,  u_tap.y))
              + sample_at(uv + vec2( u_tap.x, -u_tap.y))
              + sample_at(uv + vec2(-u_tap.x, -u_tap.y));
            c /= 6.0;
        }
        if (u_tint > 0.0) {
            float luma = dot(c.rgb, vec3(0.299, 0.587, 0.114));
            float lu = c.a > 0.001 ? luma / c.a : 0.5;
            float roundness = clamp(u_radius / max(min(u_half.x, u_half.y), 1.0), 0.0, 1.0);
            float m = min(u_half.x, u_half.y);
            float hl = 1.0 - smoothstep(0.0, 0.95 * m, length(v_local + vec2(0.32, 0.38) * u_half));
            vec3 orb = u_accent * (0.72 + 0.5 * lu) + vec3(0.32 * hl * roundness);
            c = mix(c, vec4(min(orb, vec3(1.0)), 1.0), u_tint);
        }
        body = c * cov;
    }
    float g = 0.0;
    if (u_glow > 0.0 && u_glow_r > 0.0) {
        float x = max(d, 0.0) / u_glow_r;
        g = u_glow * 0.55 * exp(-3.0 * x * x) * (1.0 - cov);
    }
    vec4 o = body + vec4(u_accent * g, g) * (1.0 - body.a);
    gl_FragColor = o * u_alpha;
}
"#;

#[derive(Clone, Copy, Debug)]
struct Program {
    program: ffi::types::GLuint,
    u_proj: ffi::types::GLint,
    u_tex: ffi::types::GLint,
    u_half: ffi::types::GLint,
    u_radius: ffi::types::GLint,
    u_uv: ffi::types::GLint,
    u_bias: ffi::types::GLint,
    u_tap: ffi::types::GLint,
    u_accent: ffi::types::GLint,
    u_tint: ffi::types::GLint,
    u_glow: ffi::types::GLint,
    u_glow_r: ffi::types::GLint,
    u_alpha: ffi::types::GLint,
    a_pos: ffi::types::GLuint,
    a_local: ffi::types::GLuint,
}

/// What a context remembers: the program, or that building it failed.
struct ProgramState(Option<Program>);

/// Compile one shader stage. Shared with the other Lumen programs.
///
/// # Safety
/// `gl` must be current (inside `with_context`).
pub unsafe fn compile_shader(gl: &ffi::Gles2, kind: ffi::types::GLenum, src: &str) -> Result<ffi::types::GLuint, String> {
    let shader = gl.CreateShader(kind);
    let text = CString::new(src).map_err(|e| e.to_string())?;
    let ptr = text.as_ptr();
    gl.ShaderSource(shader, 1, &ptr, std::ptr::null());
    gl.CompileShader(shader);
    let mut ok = 0;
    gl.GetShaderiv(shader, ffi::COMPILE_STATUS, &mut ok);
    if ok == ffi::FALSE as i32 {
        let mut len = 0;
        gl.GetShaderiv(shader, ffi::INFO_LOG_LENGTH, &mut len);
        let mut log = vec![0u8; len.max(1) as usize];
        gl.GetShaderInfoLog(shader, len, std::ptr::null_mut(), log.as_mut_ptr() as *mut _);
        gl.DeleteShader(shader);
        return Err(String::from_utf8_lossy(&log).trim_end_matches('\0').trim().to_string());
    }
    Ok(shader)
}

/// Link a vertex and fragment source into a program. Shared with the other
/// Lumen programs.
///
/// # Safety
/// `gl` must be current (inside `with_context`).
pub unsafe fn link_program(gl: &ffi::Gles2, vertex: &str, fragment: &str) -> Result<ffi::types::GLuint, String> {
    let vs = compile_shader(gl, ffi::VERTEX_SHADER, vertex)?;
    let fs = compile_shader(gl, ffi::FRAGMENT_SHADER, fragment).inspect_err(|_| gl.DeleteShader(vs))?;
    let program = gl.CreateProgram();
    gl.AttachShader(program, vs);
    gl.AttachShader(program, fs);
    gl.LinkProgram(program);
    gl.DetachShader(program, vs);
    gl.DetachShader(program, fs);
    gl.DeleteShader(vs);
    gl.DeleteShader(fs);
    let mut ok = 0;
    gl.GetProgramiv(program, ffi::LINK_STATUS, &mut ok);
    if ok == ffi::FALSE as i32 {
        let mut len = 0;
        gl.GetProgramiv(program, ffi::INFO_LOG_LENGTH, &mut len);
        let mut log = vec![0u8; len.max(1) as usize];
        gl.GetProgramInfoLog(program, len, std::ptr::null_mut(), log.as_mut_ptr() as *mut _);
        gl.DeleteProgram(program);
        return Err(String::from_utf8_lossy(&log).trim_end_matches('\0').trim().to_string());
    }
    Ok(program)
}

unsafe fn build_program(gl: &ffi::Gles2) -> Result<Program, String> {
    let program = link_program(gl, VERTEX, FRAGMENT)?;
    let uniform = |name: &str| {
        let c = CString::new(name).unwrap();
        gl.GetUniformLocation(program, c.as_ptr())
    };
    let attrib = |name: &str| {
        let c = CString::new(name).unwrap();
        gl.GetAttribLocation(program, c.as_ptr())
    };
    let p = Program {
        program,
        u_proj: uniform("u_proj"),
        u_tex: uniform("u_tex"),
        u_half: uniform("u_half"),
        u_radius: uniform("u_radius"),
        u_uv: uniform("u_uv"),
        u_bias: uniform("u_bias"),
        u_tap: uniform("u_tap"),
        u_accent: uniform("u_accent"),
        u_tint: uniform("u_tint"),
        u_glow: uniform("u_glow"),
        u_glow_r: uniform("u_glow_r"),
        u_alpha: uniform("u_alpha"),
        a_pos: attrib("a_pos") as u32,
        a_local: attrib("a_local") as u32,
    };
    // Every uniform is used by the shader, so a missing one means the link
    // went wrong (or a driver optimised one out, which would draw nonsense).
    let locations = [
        p.u_proj, p.u_tex, p.u_half, p.u_radius, p.u_uv, p.u_bias, p.u_tap, p.u_accent, p.u_tint, p.u_glow,
        p.u_glow_r, p.u_alpha, p.a_pos as i32, p.a_local as i32,
    ];
    if locations.iter().any(|&l| l < 0) {
        gl.DeleteProgram(program);
        return Err("a location is missing from the linked program".into());
    }
    Ok(p)
}

fn program<R: GlesAccess>(renderer: &mut R) -> Option<Program> {
    if !enabled() {
        return None;
    }
    let Some(gles) = renderer.gles_renderer() else {
        if STATUS.swap(2, Ordering::Relaxed) != 2 {
            warn!("lumen: this backend has no GLES renderer, effects draw their plain fallbacks");
        }
        return None;
    };
    if let Some(state) = gles.egl_context().user_data().get::<ProgramState>() {
        return state.0;
    }
    let result = match gles
        .with_context(|gl| unsafe { build_program(gl) })
        .map_err(|e: GlesError| e.to_string())
        .and_then(|r| r)
    {
        Ok(program) => {
            info!("lumen program compiled");
            STATUS.store(1, Ordering::Relaxed);
            Some(program)
        }
        Err(err) => {
            warn!("lumen program failed to build, effects draw their plain fallbacks: {err}");
            STATUS.store(2, Ordering::Relaxed);
            None
        }
    };
    gles.egl_context().user_data().insert_if_missing(|| ProgramState(result));
    result
}

// === The picture ===

#[derive(Clone)]
struct Uploaded {
    texture: GlesTexture,
    mipmapped: bool,
}

/// A window's picture for Lumen: the snapshot, its average-colour accent, and
/// its texture, uploaded (with mipmaps) on first draw. One per effect run;
/// dropping it frees both copies.
pub struct Source {
    snapshot: Snapshot,
    average_accent: [f32; 3],
    id: Id,
    commit: Cell<CommitCounter>,
    texture: RefCell<Option<Uploaded>>,
    warned: Cell<bool>,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("lumen::Source").field("snapshot", &self.snapshot).finish()
    }
}

impl Source {
    pub fn new(snapshot: Snapshot) -> Self {
        let average_accent = accent_of(&snapshot);
        Self {
            snapshot,
            average_accent,
            id: Id::new(),
            commit: Cell::new(CommitCounter::default()),
            texture: RefCell::new(None),
            warned: Cell::new(false),
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The accent made from the picture's average colour (see
    /// [`accent_from_average`]); an app's own accent, when known, wins.
    pub fn average_accent(&self) -> [f32; 3] {
        self.average_accent
    }

    /// The snapshot's size in pixels.
    pub fn size(&self) -> [f64; 2] {
        [self.snapshot.size.w as f64, self.snapshot.size.h as f64]
    }

    /// The part of the texture covering `rect` (output-physical), given that
    /// the snapshot's top-left pixel sits at `top_left`: what to put in
    /// [`Params::uv`] to draw only the frame and not a client's shadow margin.
    pub fn uv_of(&self, top_left: Point<f64, Physical>, rect: [f64; 4]) -> [f32; 4] {
        let [w, h] = self.size();
        let u0 = (rect[0] - top_left.x) / w.max(1.0);
        let v0 = (rect[1] - top_left.y) / h.max(1.0);
        let u1 = (rect[0] + rect[2] - top_left.x) / w.max(1.0);
        let v1 = (rect[1] + rect[3] - top_left.y) / h.max(1.0);
        [u0, v0, u1, v1].map(|x| x.clamp(0.0, 1.0) as f32)
    }

    fn upload<R: GlesAccess>(&self, renderer: &mut R) -> Option<Uploaded> {
        if let Some(up) = self.texture.borrow().as_ref() {
            return Some(up.clone());
        }
        let gles = renderer.gles_renderer()?;
        let size = Size::<i32, Buffer>::from((self.snapshot.size.w, self.snapshot.size.h));
        let texture = ImportMem::import_memory(gles, self.snapshot.pixels(), READBACK_FORMAT, size, false)
            .map_err(|e| {
                if !self.warned.replace(true) {
                    warn!("lumen: the snapshot did not upload, drawing the plain fallback: {e}");
                }
            })
            .ok()?;
        // Mipmaps are the blur, and keep a picture shrunk to an icon from
        // shimmering. GLES 3 makes them for any size; on GLES 2 the call
        // fails and the texture is sampled bilinear (the taps still soften).
        let id = texture.tex_id();
        let mipmapped = MIPMAPS.load(Ordering::Relaxed)
            && gles
                .with_context(|gl| unsafe {
                    while gl.GetError() != ffi::NO_ERROR {}
                    gl.BindTexture(ffi::TEXTURE_2D, id);
                    gl.GenerateMipmap(ffi::TEXTURE_2D);
                    let ok = gl.GetError() == ffi::NO_ERROR;
                    gl.BindTexture(ffi::TEXTURE_2D, 0);
                    ok
                })
                .unwrap_or(false);
        let up = Uploaded { texture, mipmapped };
        *self.texture.borrow_mut() = Some(up.clone());
        Some(up)
    }

    /// One frame of this picture drawn with `params`, counted against
    /// `effect`. `None` when Lumen can't draw: draw the plain fallback.
    pub fn element<R: GlesAccess>(&self, renderer: &mut R, params: &Params, effect: Effect) -> Option<LumenElement> {
        let program = program(renderer)?;
        let up = self.upload(renderer)?;
        let mut commit = self.commit.get();
        commit.increment();
        self.commit.set(commit);
        let quad = Quad::new(params);
        Some(LumenElement {
            program,
            texture: up.texture,
            mipmapped: up.mipmapped,
            id: self.id.clone(),
            commit,
            params: *params,
            quad,
            effect,
        })
    }
}

// === Parameters and the quad ===

/// How one frame of a picture is drawn. Positions and lengths are
/// output-physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// The shape's centre.
    pub center: [f64; 2],
    /// The shape's size before squash and stretch.
    pub size: [f64; 2],
    /// Corner radius; clamped to half the short side (a circle or a pill).
    pub radius: f64,
    /// Blur, roughly its radius in pixels. 0 is sharp.
    pub blur: f64,
    /// 0 the picture, 1 an orb of `accent`.
    pub tint: f64,
    /// Outer glow strength, 0..1, and its width.
    pub glow: f64,
    pub glow_radius: f64,
    pub accent: [f32; 3],
    pub alpha: f32,
    /// Scale along `angle` (radians, 0 is +x); the cross axis gets the
    /// reciprocal. 1 is none. Clamped to `[1/MAX_STRETCH, MAX_STRETCH]`.
    pub stretch: f64,
    pub angle: f64,
    /// The part of the texture drawn into the shape: `[u0, v0, u1, v1]`.
    pub uv: [f32; 4],
}

/// The most squash or stretch the program will draw.
pub const MAX_STRETCH: f64 = 1.5;
/// Beyond the shape, a pixel of feather for the edge.
const FEATHER: f64 = 1.0;

impl Params {
    /// The whole picture, sharp and plain, its top-left pixel at `top_left`.
    pub fn picture(source: &Source, top_left: Point<f64, Physical>) -> Self {
        let [w, h] = source.size();
        Self {
            center: [top_left.x + w / 2.0, top_left.y + h / 2.0],
            size: [w, h],
            radius: 0.0,
            blur: 0.0,
            tint: 0.0,
            glow: 0.0,
            glow_radius: 0.0,
            accent: source.average_accent(),
            alpha: 1.0,
            stretch: 1.0,
            angle: 0.0,
            uv: [0.0, 0.0, 1.0, 1.0],
        }
    }

    /// The radius actually drawn.
    pub fn clamped_radius(&self) -> f64 {
        self.radius.clamp(0.0, self.size[0].min(self.size[1]).max(0.0) / 2.0)
    }

    pub fn clamped_stretch(&self) -> f64 {
        if self.stretch.is_finite() {
            self.stretch.clamp(1.0 / MAX_STRETCH, MAX_STRETCH)
        } else {
            1.0
        }
    }

    /// The quad's half extent beyond the shape: glow, blur spill, feather.
    fn margin(&self) -> f64 {
        let glow = if self.glow > 0.0 { self.glow_radius.max(0.0) } else { 0.0 };
        glow + FEATHER
    }

    /// Where a point given in the shape's own frame (pixels from its centre,
    /// unstretched) is drawn.
    pub fn map(&self, local: [f64; 2]) -> [f64; 2] {
        let k = self.clamped_stretch();
        let (s, c) = self.angle.sin_cos();
        // Rotate into the stretch axis, scale, rotate back.
        let a = local[0] * c + local[1] * s;
        let b = -local[0] * s + local[1] * c;
        let (a, b) = (a * k, b / k);
        [self.center[0] + a * c - b * s, self.center[1] + a * s + b * c]
    }
}

/// The four corners of the drawn quad: (position, local), and their bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Quad {
    vertices: [f32; 16],
    bounds: Rectangle<i32, Physical>,
}

const QUAD_INDICES: [u16; 6] = [0, 1, 2, 1, 3, 2];

impl Quad {
    fn new(p: &Params) -> Self {
        let m = p.margin();
        let hw = p.size[0].max(0.0) / 2.0 + m;
        let hh = p.size[1].max(0.0) / 2.0 + m;
        let corners = [[-hw, -hh], [hw, -hh], [-hw, hh], [hw, hh]];
        let mut vertices = [0f32; 16];
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (i, local) in corners.iter().enumerate() {
            let pos = p.map(*local);
            x0 = x0.min(pos[0]);
            y0 = y0.min(pos[1]);
            x1 = x1.max(pos[0]);
            y1 = y1.max(pos[1]);
            vertices[i * 4..i * 4 + 4].copy_from_slice(&[pos[0] as f32, pos[1] as f32, local[0] as f32, local[1] as f32]);
        }
        let (bx, by) = (x0.floor() as i32, y0.floor() as i32);
        let bounds = Rectangle::new(
            Point::from((bx, by)),
            Size::from(((x1.ceil() as i32 - bx).max(1), (y1.ceil() as i32 - by).max(1))),
        );
        Self { vertices, bounds }
    }
}

/// The mip LOD bias that reads as a blur of `px` output pixels: the level
/// where one texel covers about `px` of what is drawn.
pub fn blur_bias(px: f64) -> f32 {
    if px <= 0.0 {
        0.0
    } else {
        ((1.0 + px).log2()) as f32
    }
}

// === The element ===

/// One frame of a picture drawn by the Lumen program. Its commit moves every
/// frame, so the damage tracker repaints its box (and last frame's).
pub struct LumenElement {
    program: Program,
    texture: GlesTexture,
    mipmapped: bool,
    id: Id,
    commit: CommitCounter,
    params: Params,
    quad: Quad,
    effect: Effect,
}

impl std::fmt::Debug for LumenElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LumenElement")
            .field("effect", &self.effect)
            .field("bounds", &self.quad.bounds)
            .field("alpha", &self.params.alpha)
            .finish()
    }
}

impl LumenElement {
    pub fn params(&self) -> &Params {
        &self.params
    }
}

impl Element for LumenElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        let s = self.texture.size();
        Rectangle::from_size(Size::from((s.w as f64, s.h as f64)))
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.quad.bounds
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.params.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl<R: GlesAccess> RenderElement<R> for LumenElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        if damage.is_empty() || self.params.alpha <= 0.0 {
            return Ok(());
        }
        let mut result = Ok(());
        R::with_gles_frame(frame, &mut |gles| {
            let projection = *gles.projection();
            result = gles.with_context(|gl| unsafe { self.draw_gl(gl, &projection, dst, damage) });
        });
        result.map_err(R::gles_error)
    }

    /// Never scanned out: it is a drawn picture.
    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl LumenElement {
    /// # Safety
    /// Raw GL on the frame's context, as `GlesFrame::with_context` allows;
    /// every piece of state changed is put back (scissor to the whole target,
    /// attributes disabled, texture unbound).
    unsafe fn draw_gl(
        &self,
        gl: &ffi::Gles2,
        projection: &[f32; 9],
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
    ) {
        let p = &self.program;
        let q = &self.params;
        let mut viewport = [0i32; 4];
        gl.GetIntegerv(ffi::VIEWPORT, viewport.as_mut_ptr());

        gl.Enable(ffi::BLEND);
        gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
        gl.UseProgram(p.program);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, self.texture.tex_id());
        let min_filter = if self.mipmapped { ffi::LINEAR_MIPMAP_LINEAR } else { ffi::LINEAR };
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, min_filter as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE as i32);

        let (w, h) = (q.size[0].max(1e-3), q.size[1].max(1e-3));
        let blur = q.blur.max(0.0);
        // Four taps half a blur away, in texture units.
        let tap = [
            (blur * 0.5 * (q.uv[2] - q.uv[0]).abs() as f64 / w) as f32,
            (blur * 0.5 * (q.uv[3] - q.uv[1]).abs() as f64 / h) as f32,
        ];
        let bias = if self.mipmapped { blur_bias(blur) } else { 0.0 };
        gl.Uniform1i(p.u_tex, 0);
        gl.UniformMatrix3fv(p.u_proj, 1, ffi::FALSE, projection.as_ptr());
        gl.Uniform2f(p.u_half, (w / 2.0) as f32, (h / 2.0) as f32);
        gl.Uniform1f(p.u_radius, q.clamped_radius() as f32);
        gl.Uniform4f(p.u_uv, q.uv[0], q.uv[1], q.uv[2], q.uv[3]);
        gl.Uniform1f(p.u_bias, bias);
        gl.Uniform2f(p.u_tap, tap[0], tap[1]);
        gl.Uniform3f(p.u_accent, q.accent[0], q.accent[1], q.accent[2]);
        gl.Uniform1f(p.u_tint, q.tint.clamp(0.0, 1.0) as f32);
        gl.Uniform1f(p.u_glow, q.glow.clamp(0.0, 1.0) as f32);
        gl.Uniform1f(p.u_glow_r, q.glow_radius.max(0.0) as f32);
        gl.Uniform1f(p.u_alpha, q.alpha.clamp(0.0, 1.0));

        gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
        gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, 0);
        let stride = (4 * std::mem::size_of::<f32>()) as i32;
        let base = self.quad.vertices.as_ptr();
        // Smithay's instanced draws leave divisor 1 on the slots they used and
        // never reset it: left alone, every vertex reads the first one.
        if gl.VertexAttribDivisor.is_loaded() {
            for loc in [p.a_pos, p.a_local] {
                gl.VertexAttribDivisor(loc, 0);
            }
        }
        gl.EnableVertexAttribArray(p.a_pos);
        gl.VertexAttribPointer(p.a_pos, 2, ffi::FLOAT, ffi::FALSE, stride, base as *const _);
        gl.EnableVertexAttribArray(p.a_local);
        gl.VertexAttribPointer(p.a_local, 2, ffi::FLOAT, ffi::FALSE, stride, base.add(2) as *const _);

        for rect in damage {
            let abs = Rectangle::new(rect.loc + dst.loc, rect.size);
            if let Some([x, y, w, h]) = scissor_box(projection, viewport, abs) {
                gl.Scissor(x, y, w, h);
                gl.DrawElements(ffi::TRIANGLES, 6, ffi::UNSIGNED_SHORT, QUAD_INDICES.as_ptr() as *const _);
            }
        }

        gl.Scissor(0, 0, viewport[2], viewport[3]);
        gl.DisableVertexAttribArray(p.a_pos);
        gl.DisableVertexAttribArray(p.a_local);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        count_frame(self.effect);
    }
}

/// A damage rectangle (output-physical) as a GL scissor box in the target's
/// pixels: its corners through the projection (column-major 3x3, as smithay
/// holds it), to normalized device coordinates, to the viewport. Shared with
/// the other Lumen programs.
pub fn scissor_box(m: &[f32; 9], viewport: [i32; 4], rect: Rectangle<i32, Physical>) -> Option<[i32; 4]> {
    let project = |x: f32, y: f32| {
        let nx = m[0] * x + m[3] * y + m[6];
        let ny = m[1] * x + m[4] * y + m[7];
        (
            (nx + 1.0) * 0.5 * viewport[2] as f32 + viewport[0] as f32,
            (ny + 1.0) * 0.5 * viewport[3] as f32 + viewport[1] as f32,
        )
    };
    let (x0, y0) = (rect.loc.x as f32, rect.loc.y as f32);
    let (x1, y1) = ((rect.loc.x + rect.size.w) as f32, (rect.loc.y + rect.size.h) as f32);
    let a = project(x0, y0);
    let b = project(x1, y1);
    let left = (a.0.min(b.0) + 1e-3).floor() as i32;
    let right = (a.0.max(b.0) - 1e-3).ceil() as i32;
    let bottom = (a.1.min(b.1) + 1e-3).floor() as i32;
    let top = (a.1.max(b.1) - 1e-3).ceil() as i32;
    let (w, h) = (right - left, top - bottom);
    (w > 0 && h > 0).then_some([left, bottom, w, h])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Params {
        Params {
            center: [500.0, 400.0],
            size: [200.0, 100.0],
            radius: 16.0,
            blur: 0.0,
            tint: 0.0,
            glow: 0.0,
            glow_radius: 0.0,
            accent: DEFAULT_ACCENT,
            alpha: 1.0,
            stretch: 1.0,
            angle: 0.0,
            uv: [0.0, 0.0, 1.0, 1.0],
        }
    }

    #[test]
    fn the_radius_never_exceeds_half_the_short_side() {
        let mut p = params();
        p.radius = 500.0;
        assert_eq!(p.clamped_radius(), 50.0);
        p.size = [30.0, 30.0];
        assert_eq!(p.clamped_radius(), 15.0, "a circle");
        p.radius = -3.0;
        assert_eq!(p.clamped_radius(), 0.0);
    }

    #[test]
    fn stretch_is_bounded_and_keeps_area() {
        let mut p = params();
        p.stretch = 9.0;
        assert_eq!(p.clamped_stretch(), MAX_STRETCH);
        p.stretch = f64::NAN;
        assert_eq!(p.clamped_stretch(), 1.0);
        // Stretched 1.3 along x: a unit step along x maps to 1.3, along y to 1/1.3.
        p.stretch = 1.3;
        let o = p.map([0.0, 0.0]);
        let x = p.map([10.0, 0.0]);
        let y = p.map([0.0, 10.0]);
        assert!((x[0] - o[0] - 13.0).abs() < 1e-9 && (x[1] - o[1]).abs() < 1e-9);
        assert!((y[1] - o[1] - 10.0 / 1.3).abs() < 1e-9);
        // Along 90 degrees, the roles swap.
        p.angle = std::f64::consts::FRAC_PI_2;
        let y = p.map([0.0, 10.0]);
        assert!((y[1] - o[1] - 13.0).abs() < 1e-9 && (y[0] - o[0]).abs() < 1e-9);
    }

    #[test]
    fn the_quad_covers_the_shape_and_its_glow() {
        let mut p = params();
        let q = Quad::new(&p);
        assert_eq!(q.bounds, Rectangle::new((399, 349).into(), (202, 102).into()));
        p.glow = 0.5;
        p.glow_radius = 20.0;
        let q = Quad::new(&p);
        assert_eq!(q.bounds, Rectangle::new((379, 329).into(), (242, 142).into()));
        // Local coordinates are the unstretched shape's, whatever the stretch.
        p.stretch = 1.3;
        p.angle = 0.7;
        let q = Quad::new(&p);
        assert_eq!(&q.vertices[2..4], &[-121.0, -71.0]);
        assert_eq!(&q.vertices[14..16], &[121.0, 71.0]);
    }

    #[test]
    fn uv_of_crops_the_frame_out_of_a_margined_picture() {
        let snap = Snapshot::from_pixels(vec![0; 120 * 100 * 4], (120, 100).into(), (-10, -10).into(), 1.0);
        let s = Source::new(snap);
        let uv = s.uv_of(Point::from((90.0, 90.0)), [100.0, 100.0, 100.0, 80.0]);
        let want = [10.0 / 120.0, 0.1, 110.0 / 120.0, 0.9];
        for (a, b) in uv.iter().zip(want) {
            assert!((*a as f64 - b).abs() < 1e-6, "{uv:?}");
        }
    }

    #[test]
    fn a_white_window_makes_a_coloured_accent() {
        let a = accent_from_average([1.0, 1.0, 1.0]);
        let (_, s, l) = rgb_to_hsl(a);
        assert!(s >= 0.5 && (0.47..=0.65).contains(&l), "{a:?}");
        // It borrows the default hue: blue is the strongest channel.
        assert!(a[2] > a[0] && a[2] > a[1]);
        // A coloured window keeps its own hue, saturated.
        let red = accent_from_average([0.8, 0.3, 0.3]);
        assert!(red[0] > red[1] + 0.2 && red[0] > red[2] + 0.2, "{red:?}");
        let dull = accent_from_average([0.5, 0.45, 0.45]);
        assert!(rgb_to_hsl(dull).1 >= 0.54);
    }

    #[test]
    fn the_average_is_alpha_weighted_and_hex_parses() {
        // Two premultiplied pixels: opaque red and half-transparent nothing.
        let px = [255, 0, 0, 255, 0, 0, 0, 0];
        assert_eq!(average_rgba(&px), Some([1.0, 0.0, 0.0]));
        assert_eq!(average_rgba(&[0, 0, 0, 0]), None);
        assert_eq!(parse_hex("#ff8000"), Some([1.0, 128.0 / 255.0, 0.0]));
        assert_eq!(parse_hex("00ff00"), Some([0.0, 1.0, 0.0]));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("#gg0000"), None);
    }

    #[test]
    fn hsl_round_trips() {
        for c in [[0.2, 0.4, 0.9], [0.9, 0.1, 0.3], [0.3, 0.8, 0.2], [0.5, 0.5, 0.5]] {
            let (h, s, l) = rgb_to_hsl(c);
            let back = hsl_to_rgb(h, s, l);
            for (a, b) in c.iter().zip(back) {
                assert!((a - b).abs() < 1e-4, "{c:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn blur_bias_grows_with_the_blur() {
        assert_eq!(blur_bias(0.0), 0.0);
        assert!((blur_bias(1.0) - 1.0).abs() < 1e-6);
        assert!(blur_bias(6.0) > blur_bias(2.0));
    }

    #[test]
    fn the_scissor_box_follows_the_projection() {
        let m = [2.0 / 1000.0, 0.0, 0.0, 0.0, -2.0 / 500.0, 0.0, -1.0, 1.0, 1.0];
        let viewport = [0, 0, 1000, 500];
        let rect = Rectangle::<i32, Physical>::new((100, 50).into(), (200, 100).into());
        assert_eq!(scissor_box(&m, viewport, rect), Some([100, 350, 200, 100]));
        assert_eq!(scissor_box(&m, viewport, Rectangle::new((0, 0).into(), (0, 10).into())), None);
    }

    #[test]
    fn counters_and_state() {
        let before = frames(Effect::Droplet);
        count_frame(Effect::Droplet);
        assert!(frames(Effect::Droplet) > before);
        record_progress(Effect::Droplet, 0.25);
        let s = state_json();
        assert!(s["frames"]["droplet"].as_u64().unwrap() >= 1);
        for e in Effect::ALL {
            assert!(s["frames"].get(e.name()).is_some());
            assert!(s["last_progress"].get(e.name()).is_some());
        }
    }
}
