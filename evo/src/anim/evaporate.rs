//! Evaporate, Lumen's close: the window's last picture burns away from its
//! close button in a noisy front with a glowing edge, swelling slightly and
//! going soft as it goes. Spec: `docs/internal/evo-animation-plan.md`, "Lumen".
//!
//! # What is drawn
//!
//! The close snapshot (`begin_close_animation` already takes one) through a
//! program of our own: one quad, and per pixel a *field*
//!
//! ```text
//! f = 0.5 * noise + 0.5 * distance from the close button
//! noise = 0.88 * value noise (16 x 11 cells over the frame) + 0.12 * fine (64 x 44)
//! ```
//!
//! A pixel is drawn while `f > threshold(p)`, the threshold running −0.10 →
//! 1.02 with the progress, so nothing has gone at `p = 0` (f ≥ 0) and
//! everything has at `p = 1` (f ≤ 1). Pixels with `f` within [`BAND`] above the
//! threshold glow: mixed toward `accent·0.45 + white·0.55` by `k²`, `k` being 1
//! at the burning edge. The noise is hash-based and computed in the fragment
//! shader, so it costs one texture fetch per pixel. [`field`] here is the same
//! sum, line for line, so the tests check what the shader does.
//!
//! The pose (scale 1 → 1.04, cubic out, about the frame's centre or about
//! whatever the window was animating) is an ordinary [`TransformAnim`], so a
//! close mid-open starts from the open's pose ([`transform`]); the blur is a
//! mip LOD bias on the snapshot texture (uploaded mipmapped, as the warp's is),
//! the shadow is gone by 30% ([`shadow_keep`]) and the whole picture fades over
//! the last 20% ([`fade`]).
//!
//! # GL
//!
//! Its own program (Lumen's shared one clips, blurs and tints; it has no field
//! or threshold), built and drawn the way `anim/lumen.rs` draws, with its
//! scissor helper. The same gotchas hold: smithay's
//! instanced draws leave divisor 1 on attribute slots, so this sets 0 on its
//! own; it draws only inside each damage rect (a scissor through the frame's
//! projection); and it puts back every state it changes.
//!
//! # Fallback
//!
//! Reduced and off never get here (`begin_close_animation` keeps the plain
//! fade). With Lumen off (`EVO_LUMEN=off`, `evo.SetLumen {enabled:false}`) or
//! the program failed, a close is the foundation's scale/fade. A program that
//! fails on the first frame of an Evaporate already running draws the plain
//! snapshot fading out for the rest of it ([`fallback_alpha`]); a failure is
//! logged once.

use std::{
    cell::{Cell, RefCell},
    ffi::CString,
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};

use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{ffi, GlesError, GlesRenderer, GlesTexture},
        utils::{CommitCounter, OpaqueRegions},
        ImportMem, Texture,
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size},
};
use tracing::{info, warn};

use super::{
    curve::{Animatable, Animation, CubicBezier, Timing, Vec2},
    lumen::{self, link_program, scissor_box, Effect},
    settings::AnimationSettings,
    shader::GlesAccess,
    snapshot::{Snapshot, READBACK_FORMAT},
    transform::WindowTransform,
    window::{Origin, TransformAnim},
};

// === The spec's numbers ===

/// Reference duration at scale 1.
pub const DURATION: Duration = Duration::from_millis(380);
pub const SCALE_TO: f64 = 1.04;
/// `easeOutCubic` as a bezier, for the scale.
pub const SCALE_CURVE: CubicBezier = CubicBezier::new(0.33, 1.0, 0.68, 1.0);
pub const BLUR_PX: f64 = 2.5;
pub const THRESHOLD_FROM: f64 = -0.10;
pub const THRESHOLD_TO: f64 = 1.02;
/// Width of the glowing band, in field units.
pub const BAND: f64 = 0.06;
/// Width of the anti-aliased step at the threshold, in field units (about a
/// pixel of the noise's slope on a 1280 px frame).
pub const AA: f64 = 0.008;
pub const NOISE_CELLS: (f64, f64) = (16.0, 11.0);
pub const FINE_CELLS: (f64, f64) = (64.0, 44.0);
pub const FINE_WEIGHT: f64 = 0.12;
/// The shadow is gone by this much of the progress.
pub const SHADOW_GONE_AT: f64 = 0.30;
/// The picture fades over the last this-much of the progress.
pub const FADE_FROM: f64 = 0.80;
/// What the glow colour is made of: `accent * GLOW_ACCENT + white * (1 - GLOW_ACCENT)`.
pub const GLOW_ACCENT: f64 = 0.45;

// === Status and counters ===
//
// The switch and the counters are Lumen's (`anim::lumen`): this counts its
// frames as `frames.evaporate` and its progress as `last_progress.evaporate`
// in `GetLumenState`. The program is this module's own (a field and a
// threshold, not the shared clip/blur/tint), so its status is too
// (`GetLumenState.evaporate_program`).

/// 0 not tried, 1 compiled, 2 failed (or no GLES to compile on).
static STATUS: AtomicU8 = AtomicU8::new(0);

/// Whether a close starting now should evaporate: Lumen on, and this program
/// not already failed. (Reduced/off are the caller's: they never evaporate.)
pub fn usable() -> bool {
    lumen::enabled() && STATUS.load(Ordering::Relaxed) != 2
}

/// `"untried"`, `"compiled"` or `"failed"`.
pub fn status() -> &'static str {
    match STATUS.load(Ordering::Relaxed) {
        1 => "compiled",
        2 => "failed",
        _ => "untried",
    }
}

// === The maths (mirrors the shader) ===

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The threshold at progress `p` (linear, so the front moves at an even pace).
pub fn threshold(p: f64) -> f64 {
    lerp(THRESHOLD_FROM, THRESHOLD_TO, p.clamp(0.0, 1.0))
}

/// The pose's ease (scale and blur): cubic out.
pub fn eased(p: f64) -> f64 {
    1.0 - (1.0 - p.clamp(0.0, 1.0)).powi(3)
}

pub fn blur_px(p: f64) -> f64 {
    BLUR_PX * eased(p)
}

/// The LOD bias the blur is drawn as (a mip level about `2r` pixels across).
pub fn lod_bias(p: f64) -> f64 {
    (1.0 + blur_px(p)).log2()
}

/// The shadow's remaining opacity.
pub fn shadow_keep(p: f64) -> f64 {
    (1.0 - p / SHADOW_GONE_AT).clamp(0.0, 1.0)
}

/// The picture's overall opacity (times the pose's own).
pub fn fade(p: f64) -> f64 {
    ((1.0 - p) / (1.0 - FADE_FROM)).clamp(0.0, 1.0)
}

/// The opacity of the plain snapshot drawn when the program can't be, for an
/// Evaporate already under way: an even fade to 0 by the end.
pub fn fallback_alpha(p: f64) -> f64 {
    (1.0 - p).clamp(0.0, 1.0)
}

/// A float hash of a cell (Dave Hoskins' `hash12`, no `sin`, so it holds up in
/// mediump). In `f32` to match the GPU.
fn hash(x: f32, y: f32) -> f32 {
    let fract = |v: f32| v - v.floor();
    let mut p = [fract(x * 0.1031), fract(y * 0.1031), fract(x * 0.1031)];
    let d = p[0] * (p[1] + 33.33) + p[1] * (p[2] + 33.33) + p[2] * (p[0] + 33.33);
    p = [p[0] + d, p[1] + d, p[2] + d];
    fract((p[0] + p[1]) * p[2])
}

/// Smooth value noise in [0, 1].
fn vnoise(x: f64, y: f64) -> f64 {
    let (ix, iy) = (x.floor() as f32, y.floor() as f32);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let (ux, uy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let a = hash(ix, iy) as f64;
    let b = hash(ix + 1.0, iy) as f64;
    let c = hash(ix, iy + 1.0) as f64;
    let d = hash(ix + 1.0, iy + 1.0) as f64;
    lerp(lerp(a, b, ux), lerp(c, d, ux), uy)
}

/// Where the close button's centre sits in a frame of `size` logical pixels,
/// frame-normalized (0..1 each way): the SSD's top-right button.
pub fn button(size: (f64, f64)) -> (f64, f64) {
    let half = crate::shell::ssd::HEADER_BAR_HEIGHT as f64 / 2.0;
    let (w, h) = (size.0.max(1.0), size.1.max(1.0));
    ((1.0 - half / w).clamp(0.0, 1.0), (half / h).clamp(0.0, 1.0))
}

/// What the distance is divided by: the button to the frame's far
/// (bottom-left) corner, in pixels, so the distance term is 1 there.
pub fn distance_norm(size: (f64, f64), btn: (f64, f64)) -> f64 {
    (btn.0 * size.0).hypot((1.0 - btn.1) * size.1).max(1.0)
}

/// The field at frame-normalized `(x, y)` (outside 0..1 for a snapshot's
/// margin) of a frame `size` pixels across. In [0, 1].
pub fn field(x: f64, y: f64, size: (f64, f64)) -> f64 {
    let n = (1.0 - FINE_WEIGHT) * vnoise(x * NOISE_CELLS.0, y * NOISE_CELLS.1)
        + FINE_WEIGHT * vnoise(x * FINE_CELLS.0, y * FINE_CELLS.1);
    let btn = button(size);
    let d = (((x - btn.0) * size.0).hypot((y - btn.1) * size.1) / distance_norm(size, btn)).min(1.0);
    0.5 * n + 0.5 * d
}

/// How much of a pixel with field `f` is still drawn at threshold `th`.
pub fn coverage(f: f64, th: f64) -> f64 {
    smoothstep(th, th + AA, f)
}

/// How far a pixel is mixed toward the glow colour.
pub fn glow_weight(f: f64, th: f64) -> f64 {
    let k = (1.0 - (f - th) / BAND).clamp(0.0, 1.0);
    k * k
}

/// The glow colour for `accent` (straight RGB in 0..1).
pub fn glow_colour(accent: [f32; 3]) -> [f32; 3] {
    let w = (1.0 - GLOW_ACCENT) as f32;
    accent.map(|c| c * GLOW_ACCENT as f32 + w)
}

// === The pose ===

/// The close's `TransformAnim`: scale 1 → 1.04 (cubic out), from `prev`'s pose
/// at `now` when the window was still animating (a close mid-open starts where
/// the open was, instead of jumping to full size). Alpha holds at the pose's
/// (the fade is [`fade`], applied in the draw). `None` when settings say not to
/// animate or not to move (reduced keeps the plain fade).
pub fn transform(prev: Option<&TransformAnim>, now: Duration, settings: &AnimationSettings) -> Option<TransformAnim> {
    if !settings.motion() {
        return None;
    }
    let duration = settings.duration(DURATION)?;
    let (origin, s, d, a) = match prev {
        Some(p) => (p.origin, p.scale.value(now), p.offset.value(now), p.alpha.value(now)),
        None => (Origin::Frame(0.5, 0.5), Vec2::splat(1.0), Vec2::splat(0.0), 1.0),
    };
    Some(TransformAnim {
        origin,
        scale: Animation::new(s, s.scale(SCALE_TO), now, Timing::curve(duration, SCALE_CURVE)),
        offset: Animation::fixed(d),
        alpha: Animation::fixed(a.clamp(0.0, 1.0)),
    })
}

/// What a close is drawn as.
#[derive(Debug)]
pub enum Close {
    /// Evaporate, with its pose.
    Evaporate(TransformAnim),
    /// The foundation's scale/fade of the snapshot (reduced: a plain fade).
    Plain(TransformAnim),
}

/// The close for a window whose running animation (if any) is `prev`:
/// Evaporate when `usable` (Lumen on, the program not failed) and in full
/// mode, else the plain close (from `prev`'s pose too). `None` when off.
pub fn choose(prev: Option<&TransformAnim>, now: Duration, settings: &AnimationSettings, usable: bool) -> Option<Close> {
    if usable {
        if let Some(anim) = transform(prev, now, settings) {
            return Some(Close::Evaporate(anim));
        }
    }
    let plain = match prev {
        Some(prev) => super::minimize::close_from(prev, now, settings),
        None => TransformAnim::close(settings, now),
    };
    plain.map(Close::Plain)
}

/// Progress of the close at `now`, from its pose's scale animation.
pub fn progress(anim: &TransformAnim, now: Duration) -> f64 {
    let run = anim.scale.duration().as_secs_f64();
    if run <= 0.0 {
        return 1.0;
    }
    (now.saturating_sub(anim.scale.start()).as_secs_f64() / run).clamp(0.0, 1.0)
}

// === The GL program ===

const VERTEX: &str = r#"
uniform mat3 u_proj;
attribute vec2 a_pos;
attribute vec2 a_uv;
attribute vec2 a_fp;
varying vec2 v_uv;
varying vec2 v_fp;
void main() {
    vec3 p = u_proj * vec3(a_pos, 1.0);
    gl_Position = vec4(p.xy, 0.0, 1.0);
    v_uv = a_uv;
    v_fp = a_fp;
}
"#;

/// `v_fp` is the frame-normalized position (0..1 over the window's frame).
/// The field is `field()` above; keep the two the same.
const FRAGMENT: &str = r#"
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
uniform sampler2D u_tex;
uniform float u_alpha;
uniform float u_th;
uniform float u_lod;
uniform vec3 u_glow;
uniform vec2 u_px;
uniform vec2 u_btn;
uniform float u_dnorm;
varying vec2 v_uv;
varying vec2 v_fp;

float hash(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

float vnoise(vec2 x) {
    vec2 i = floor(x);
    vec2 f = x - i;
    vec2 u = f * f * (3.0 - 2.0 * f);
    float a = hash(i);
    float b = hash(i + vec2(1.0, 0.0));
    float c = hash(i + vec2(0.0, 1.0));
    float d = hash(i + vec2(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

void main() {
    float n = 0.88 * vnoise(v_fp * vec2(16.0, 11.0)) + 0.12 * vnoise(v_fp * vec2(64.0, 44.0));
    float d = min(length((v_fp - u_btn) * u_px) / u_dnorm, 1.0);
    float f = 0.5 * n + 0.5 * d;
    float cov = smoothstep(u_th, u_th + 0.008, f);
    if (cov <= 0.0) {
        discard;
    }
    vec4 c = texture2D(u_tex, v_uv, u_lod);
    float k = clamp(1.0 - (f - u_th) / 0.06, 0.0, 1.0);
    c.rgb = mix(c.rgb, u_glow * c.a, k * k);
    gl_FragColor = c * (cov * u_alpha);
}
"#;

#[derive(Clone, Copy, Debug)]
pub struct EvaporateProgram {
    program: ffi::types::GLuint,
    u_proj: ffi::types::GLint,
    u_tex: ffi::types::GLint,
    u_alpha: ffi::types::GLint,
    u_th: ffi::types::GLint,
    u_lod: ffi::types::GLint,
    u_glow: ffi::types::GLint,
    u_px: ffi::types::GLint,
    u_btn: ffi::types::GLint,
    u_dnorm: ffi::types::GLint,
    a_pos: ffi::types::GLuint,
    a_uv: ffi::types::GLuint,
    a_fp: ffi::types::GLuint,
}

/// What a context remembers: the program, or that building it failed.
struct ProgramState(Option<EvaporateProgram>);

unsafe fn build_program(gl: &ffi::Gles2) -> Result<EvaporateProgram, String> {
    let program = link_program(gl, VERTEX, FRAGMENT)?;
    let uniform = |name: &str| {
        let c = CString::new(name).unwrap();
        gl.GetUniformLocation(program, c.as_ptr())
    };
    let attrib = |name: &str| {
        let c = CString::new(name).unwrap();
        gl.GetAttribLocation(program, c.as_ptr())
    };
    let p = EvaporateProgram {
        program,
        u_proj: uniform("u_proj"),
        u_tex: uniform("u_tex"),
        u_alpha: uniform("u_alpha"),
        u_th: uniform("u_th"),
        u_lod: uniform("u_lod"),
        u_glow: uniform("u_glow"),
        u_px: uniform("u_px"),
        u_btn: uniform("u_btn"),
        u_dnorm: uniform("u_dnorm"),
        a_pos: attrib("a_pos") as u32,
        a_uv: attrib("a_uv") as u32,
        a_fp: attrib("a_fp") as u32,
    };
    let uniforms = [p.u_proj, p.u_tex, p.u_alpha, p.u_th, p.u_lod, p.u_glow, p.u_px, p.u_btn, p.u_dnorm];
    let attribs = [p.a_pos, p.a_uv, p.a_fp];
    if uniforms.iter().any(|&l| l < 0) || attribs.iter().any(|&l| (l as i32) < 0) {
        gl.DeleteProgram(program);
        return Err("a location is missing from the linked program".into());
    }
    Ok(p)
}

fn compile(gles: &mut GlesRenderer) -> Result<EvaporateProgram, String> {
    gles.with_context(|gl| unsafe { build_program(gl) })
        .map_err(|e: GlesError| e.to_string())?
}

/// The program for `renderer`, built on first use. `None` when the renderer
/// has no GLES or the build failed (logged once, not retried).
pub fn program<R: GlesAccess>(renderer: &mut R) -> Option<EvaporateProgram> {
    let Some(gles) = renderer.gles_renderer() else {
        if STATUS.swap(2, Ordering::Relaxed) != 2 {
            warn!("evaporate: this backend has no GLES renderer, closes use the plain fade");
        }
        return None;
    };
    if let Some(state) = gles.egl_context().user_data().get::<ProgramState>() {
        return state.0;
    }
    let result = match compile(gles) {
        Ok(program) => {
            info!("evaporate program compiled");
            STATUS.store(1, Ordering::Relaxed);
            Some(program)
        }
        Err(err) => {
            warn!("evaporate program failed to build, closes use the plain fade: {err}");
            STATUS.store(2, Ordering::Relaxed);
            None
        }
    };
    gles.egl_context().user_data().insert_if_missing(|| ProgramState(result));
    result
}

// === The payload ===

/// What an evaporating ghost carries: the close snapshot and the accent it
/// glows with. The snapshot is also what the fallback draws.
pub struct Evaporate {
    snapshot: Snapshot,
    accent: [f32; 3],
    id: Id,
    commit: Cell<CommitCounter>,
    texture: RefCell<Option<Uploaded>>,
    warned: Cell<bool>,
}

#[derive(Clone)]
struct Uploaded {
    texture: GlesTexture,
    mipmapped: bool,
}

impl std::fmt::Debug for Evaporate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Evaporate").field("snapshot", &self.snapshot).field("accent", &self.accent).finish()
    }
}

/// Where a frame is drawn.
pub struct Placement {
    /// The window's frame, output-logical (global space).
    pub frame: Rectangle<i32, Logical>,
    /// The window's render origin, global space (the snapshot's pixels sit at
    /// `render_origin + snapshot.origin`).
    pub render_origin: Point<i32, Logical>,
    pub output_origin: Point<i32, Logical>,
    pub scale: f64,
}

/// The four corners of the drawn quad: positions in output-physical pixels,
/// uvs, and frame-normalized positions (what the field is computed over).
#[derive(Clone, Debug, PartialEq)]
pub struct Quad {
    pub vertices: [f32; 24],
    pub bounds: Rectangle<i32, Physical>,
}

/// Floats per vertex: position (2), uv (2), frame position (2).
const STRIDE_FLOATS: usize = 6;
const INDICES: [u16; 6] = [0, 1, 2, 1, 3, 2];

/// The quad for a snapshot of `snap_size` pixels at `snap_origin` (relative to
/// the render origin, physical), a frame at `frame_px` (relative to the render
/// origin, physical), drawn under the physical pose `scale`/`origin`/`offset`
/// with the render origin at `render_origin` (output-physical).
pub fn quad(
    render_origin: (f64, f64),
    snap_origin: (f64, f64),
    snap_size: (f64, f64),
    frame_px: Rectangle<f64, Physical>,
    pose_origin: (f64, f64),
    pose_scale: (f64, f64),
    pose_offset: (f64, f64),
) -> Quad {
    let mut vertices = [0f32; 24];
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (i, (u, v)) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].into_iter().enumerate() {
        // The corner relative to the render origin, unposed.
        let lx = snap_origin.0 + u * snap_size.0;
        let ly = snap_origin.1 + v * snap_size.1;
        let ax = render_origin.0 + lx;
        let ay = render_origin.1 + ly;
        let x = pose_origin.0 + (ax - pose_origin.0) * pose_scale.0 + pose_offset.0;
        let y = pose_origin.1 + (ay - pose_origin.1) * pose_scale.1 + pose_offset.1;
        let fx = (lx - frame_px.loc.x) / frame_px.size.w.max(1.0);
        let fy = (ly - frame_px.loc.y) / frame_px.size.h.max(1.0);
        vertices[i * STRIDE_FLOATS..(i + 1) * STRIDE_FLOATS]
            .copy_from_slice(&[x as f32, y as f32, u as f32, v as f32, fx as f32, fy as f32]);
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    let (x0, y0) = (min_x.floor() as i32, min_y.floor() as i32);
    let bounds = Rectangle::new(
        Point::from((x0, y0)),
        Size::from(((max_x.ceil() as i32 - x0).max(1), (max_y.ceil() as i32 - y0).max(1))),
    );
    Quad { vertices, bounds }
}

impl Evaporate {
    /// `accent`: Bloom's for the app (`SetMinimizeTargets`), if it sent one;
    /// else the snapshot's own.
    pub fn new(snapshot: Snapshot, accent: Option<[f32; 3]>) -> Self {
        let accent = accent.unwrap_or_else(|| lumen::accent_of(&snapshot));
        Self {
            snapshot,
            accent,
            id: Id::new(),
            commit: Cell::new(CommitCounter::default()),
            texture: RefCell::new(None),
            warned: Cell::new(false),
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The snapshot uploaded into the GLES renderer, mipmapped where the
    /// context allows (the blur is a LOD bias; without mips there is no blur).
    fn upload<R: GlesAccess>(&self, renderer: &mut R) -> Option<Uploaded> {
        if let Some(up) = self.texture.borrow().as_ref() {
            return Some(up.clone());
        }
        let gles = renderer.gles_renderer()?;
        let size = Size::<i32, Buffer>::from((self.snapshot.size.w, self.snapshot.size.h));
        let texture = ImportMem::import_memory(gles, self.snapshot.pixels(), READBACK_FORMAT, size, false)
            .map_err(|e| {
                if !self.warned.replace(true) {
                    warn!("evaporate: the snapshot did not upload, drawing the plain fade: {e}");
                }
            })
            .ok()?;
        let id = texture.tex_id();
        let mipmapped = gles
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

    /// One frame at progress `p` under pose `t`, or `None` to draw the plain
    /// snapshot instead (no program, no upload).
    pub fn draw<R: GlesAccess>(
        &self,
        renderer: &mut R,
        place: &Placement,
        t: &WindowTransform,
        p: f64,
    ) -> Option<EvaporateElement> {
        let program = program(renderer)?;
        let up = self.upload(renderer)?;
        lumen::record_progress(Effect::Evaporate, p);

        let s = place.scale;
        let to_px = |v: f64, o: i32| (v - o as f64) * s;
        let render_origin = (to_px(place.render_origin.x as f64, place.output_origin.x),
            to_px(place.render_origin.y as f64, place.output_origin.y));
        let frame_px = Rectangle::<f64, Physical>::new(
            Point::from((
                (place.frame.loc.x - place.render_origin.x) as f64 * s,
                (place.frame.loc.y - place.render_origin.y) as f64 * s,
            )),
            Size::from((place.frame.size.w as f64 * s, place.frame.size.h as f64 * s)),
        );
        let q = quad(
            render_origin,
            (self.snapshot.origin.x as f64, self.snapshot.origin.y as f64),
            (self.snapshot.size.w as f64, self.snapshot.size.h as f64),
            frame_px,
            (to_px(t.origin.x, place.output_origin.x), to_px(t.origin.y, place.output_origin.y)),
            (t.scale.x, t.scale.y),
            (t.offset.x * s, t.offset.y * s),
        );
        // The field is over the frame's logical size (the button's place and
        // the distance's aspect are the same at any output scale).
        let size = (place.frame.size.w as f64, place.frame.size.h as f64);
        let btn = button(size);
        let mut commit = self.commit.get();
        commit.increment();
        self.commit.set(commit);
        Some(EvaporateElement {
            program,
            texture: up.texture,
            mipmapped: up.mipmapped,
            id: self.id.clone(),
            commit,
            quad: q,
            alpha: (t.alpha as f64 * fade(p)) as f32,
            threshold: threshold(p) as f32,
            lod: if up.mipmapped { lod_bias(p) as f32 } else { 0.0 },
            glow: glow_colour(self.accent),
            px: [size.0 as f32, size.1 as f32],
            btn: [btn.0 as f32, btn.1 as f32],
            dnorm: distance_norm(size, btn) as f32,
        })
    }
}

// === The element ===

/// One frame of an evaporating window. Its commit moves every frame, so the
/// damage tracker repaints its box (and the last frame's).
pub struct EvaporateElement {
    program: EvaporateProgram,
    texture: GlesTexture,
    mipmapped: bool,
    id: Id,
    commit: CommitCounter,
    quad: Quad,
    alpha: f32,
    threshold: f32,
    lod: f32,
    glow: [f32; 3],
    px: [f32; 2],
    btn: [f32; 2],
    dnorm: f32,
}

impl std::fmt::Debug for EvaporateElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvaporateElement")
            .field("bounds", &self.quad.bounds)
            .field("threshold", &self.threshold)
            .field("alpha", &self.alpha)
            .finish()
    }
}

impl Element for EvaporateElement {
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
        self.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl<R: GlesAccess> RenderElement<R> for EvaporateElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        if damage.is_empty() {
            return Ok(());
        }
        let mut result = Ok(());
        R::with_gles_frame(frame, &mut |gles| {
            let projection = *gles.projection();
            result = gles.with_context(|gl| unsafe { self.draw_gl(gl, &projection, dst, damage) });
        });
        result.map_err(R::gles_error)
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl EvaporateElement {
    /// # Safety
    /// Raw GL on the frame's context, as `GlesFrame::with_context` allows;
    /// everything changed is put back (scissor box, attributes, texture).
    unsafe fn draw_gl(
        &self,
        gl: &ffi::Gles2,
        projection: &[f32; 9],
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
    ) {
        let p = &self.program;
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
        gl.Uniform1i(p.u_tex, 0);
        gl.UniformMatrix3fv(p.u_proj, 1, ffi::FALSE, projection.as_ptr());
        gl.Uniform1f(p.u_alpha, self.alpha);
        gl.Uniform1f(p.u_th, self.threshold);
        gl.Uniform1f(p.u_lod, self.lod);
        gl.Uniform3f(p.u_glow, self.glow[0], self.glow[1], self.glow[2]);
        gl.Uniform2f(p.u_px, self.px[0], self.px[1]);
        gl.Uniform2f(p.u_btn, self.btn[0], self.btn[1]);
        gl.Uniform1f(p.u_dnorm, self.dnorm);

        gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
        gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, 0);
        let stride = (STRIDE_FLOATS * std::mem::size_of::<f32>()) as i32;
        let base = self.quad.vertices.as_ptr();
        // Smithay's instanced draws leave divisor 1 behind (see the warp).
        if gl.VertexAttribDivisor.is_loaded() {
            for loc in [p.a_pos, p.a_uv, p.a_fp] {
                gl.VertexAttribDivisor(loc, 0);
            }
        }
        gl.EnableVertexAttribArray(p.a_pos);
        gl.VertexAttribPointer(p.a_pos, 2, ffi::FLOAT, ffi::FALSE, stride, base as *const _);
        gl.EnableVertexAttribArray(p.a_uv);
        gl.VertexAttribPointer(p.a_uv, 2, ffi::FLOAT, ffi::FALSE, stride, base.add(2) as *const _);
        gl.EnableVertexAttribArray(p.a_fp);
        gl.VertexAttribPointer(p.a_fp, 2, ffi::FLOAT, ffi::FALSE, stride, base.add(4) as *const _);

        for rect in damage {
            let abs = Rectangle::new(rect.loc + dst.loc, rect.size);
            if let Some([x, y, w, h]) = scissor_box(projection, viewport, abs) {
                gl.Scissor(x, y, w, h);
                gl.DrawElements(ffi::TRIANGLES, INDICES.len() as i32, ffi::UNSIGNED_SHORT, INDICES.as_ptr() as *const _);
            }
        }

        gl.Scissor(0, 0, viewport[2], viewport[3]);
        gl.DisableVertexAttribArray(p.a_pos);
        gl.DisableVertexAttribArray(p.a_uv);
        gl.DisableVertexAttribArray(p.a_fp);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        lumen::count_frame(Effect::Evaporate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::settings::Mode;

    const SIZE: (f64, f64) = (800.0, 600.0);

    /// A grid over the frame (and a margin round it), frame-normalized.
    fn grid(margin: f64) -> impl Iterator<Item = (f64, f64)> {
        let n = 80;
        (0..=n).flat_map(move |i| {
            (0..=n).map(move |j| {
                let k = |v: usize| -margin + (1.0 + 2.0 * margin) * v as f64 / n as f64;
                (k(i), k(j))
            })
        })
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_field_stays_in_0_1() {
        for (x, y) in grid(0.1) {
            let f = field(x, y, SIZE);
            assert!((0.0..=1.0).contains(&f), "{f} at {x},{y}");
        }
    }

    #[test]
    fn everything_is_drawn_at_0_and_nothing_at_1() {
        for (x, y) in grid(0.1) {
            let f = field(x, y, SIZE);
            assert_eq!(coverage(f, threshold(0.0)), 1.0, "at {x},{y}");
            assert_eq!(glow_weight(f, threshold(0.0)), 0.0, "no glow before it starts");
            assert_eq!(coverage(f, threshold(1.0)), 0.0, "at {x},{y}");
        }
        assert_eq!(fade(0.0), 1.0);
        assert_eq!(fade(1.0), 0.0);
        assert_eq!(fade(FADE_FROM), 1.0, "full opacity until the last 20%");
    }

    #[test]
    fn the_dissolve_starts_at_the_close_button() {
        let btn = button(SIZE);
        // The first pixels to go: step p until something is gone, then check
        // every gone pixel is near the button.
        let mut p = 0.0;
        let gone = loop {
            p += 0.005;
            let th = threshold(p);
            let gone: Vec<_> = grid(0.0).filter(|&(x, y)| coverage(field(x, y, SIZE), th) < 0.5).collect();
            if !gone.is_empty() {
                break gone;
            }
        };
        assert!(p < 0.25, "the first pixel went at p={p}");
        let near = |&(x, y): &(f64, f64)| ((x - btn.0) * SIZE.0).hypot((y - btn.1) * SIZE.1) < 0.4 * SIZE.0.hypot(SIZE.1);
        assert!(gone.iter().all(near), "{gone:?}");
        // And half-way, the button's quarter has gone far more than the far one.
        let th = threshold(0.5);
        let frac = |xr: (f64, f64), yr: (f64, f64)| {
            let pts: Vec<_> = grid(0.0)
                .filter(|&(x, y)| x >= xr.0 && x <= xr.1 && y >= yr.0 && y <= yr.1)
                .collect();
            pts.iter().filter(|&&(x, y)| coverage(field(x, y, SIZE), th) < 0.5).count() as f64 / pts.len() as f64
        };
        let near_q = frac((0.5, 1.0), (0.0, 0.5));
        let far_q = frac((0.0, 0.5), (0.5, 1.0));
        assert!(near_q > 0.5 && far_q < 0.15 && near_q > 4.0 * far_q, "near {near_q}, far {far_q}");
    }

    #[test]
    fn the_band_is_six_percent_wide_and_brightest_at_the_edge() {
        let th = 0.4;
        assert_eq!(glow_weight(th, th), 1.0);
        assert_eq!(glow_weight(th + BAND, th), 0.0);
        assert!((glow_weight(th + BAND / 2.0, th) - 0.25).abs() < 1e-9, "k squared");
        assert_eq!(glow_weight(th + 0.2, th), 0.0);
        assert_eq!(BAND, 0.06);
        // Monotone toward the edge.
        let mut last = 0.0;
        for i in (0..=60).rev() {
            let w = glow_weight(th + i as f64 * 0.001, th);
            assert!(w >= last);
            last = w;
        }
    }

    #[test]
    fn the_noise_has_structure_at_the_cell_scale() {
        // Not a constant, not white noise: neighbours a pixel apart agree,
        // cells apart they don't.
        let a = vnoise(3.2, 4.7);
        assert!((vnoise(3.201, 4.7) - a).abs() < 0.01);
        let spread = (0..40).map(|i| vnoise(i as f64 * 1.7, 0.3)).fold((1.0f64, 0.0f64), |(lo, hi), v| (lo.min(v), hi.max(v)));
        assert!(spread.1 - spread.0 > 0.5, "{spread:?}");
        for i in 0..200 {
            let v = vnoise(i as f64 * 0.37 - 20.0, i as f64 * 0.11 - 7.0);
            assert!((0.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn the_pose_swells_and_blurs_with_a_cubic_out() {
        let a = transform(None, ms(0), &AnimationSettings::default()).unwrap();
        assert_eq!(a.scale.value(ms(0)), Vec2::splat(1.0));
        assert!(a.is_finished(DURATION));
        assert!((a.scale.value(DURATION).x - SCALE_TO).abs() < 1e-9);
        // Cubic out: more than half way by a third of the time.
        assert!(a.scale.value(DURATION / 3).x > 1.0 + (SCALE_TO - 1.0) * 0.5);
        assert_eq!(blur_px(0.0), 0.0);
        assert!((blur_px(1.0) - BLUR_PX).abs() < 1e-9);
        assert_eq!(lod_bias(0.0), 0.0);
        assert_eq!(shadow_keep(0.0), 1.0);
        assert_eq!(shadow_keep(SHADOW_GONE_AT), 0.0);
        assert!((progress(&a, DURATION / 2) - 0.5).abs() < 1e-6);
        assert_eq!(progress(&a, DURATION * 2), 1.0);
    }

    #[test]
    fn a_close_mid_open_starts_from_the_open_pose() {
        let open = TransformAnim::open(&AnimationSettings::default(), ms(0), None).unwrap();
        let at = ms(60);
        let c = transform(Some(&open), at, &AnimationSettings::default()).unwrap();
        assert_eq!(c.origin, open.origin);
        assert_eq!(c.scale.value(at), open.scale.value(at), "no jump in scale");
        assert_eq!(c.offset.value(at), open.offset.value(at), "or in place");
        assert!((c.alpha.value(at) - open.alpha.value(at)).abs() < 1e-9, "or opacity");
        let s = open.scale.value(at).x;
        assert!((c.scale.value(at + DURATION).x - s * SCALE_TO).abs() < 1e-9);
    }

    #[test]
    fn reduced_and_off_dont_evaporate() {
        let reduced = AnimationSettings { mode: Mode::Reduced, scale: 1.0 };
        let off = AnimationSettings { mode: Mode::Off, scale: 1.0 };
        let zero = AnimationSettings { mode: Mode::Full, scale: 0.0 };
        for s in [reduced, off, zero] {
            assert!(transform(None, ms(0), &s).is_none());
        }
        let slow = AnimationSettings { mode: Mode::Full, scale: 10.0 };
        assert!(!transform(None, ms(0), &slow).unwrap().is_finished(ms(3799)));
    }

    #[test]
    fn the_close_falls_back_to_the_plain_one() {
        let full = AnimationSettings::default();
        assert!(matches!(choose(None, ms(0), &full, true), Some(Close::Evaporate(_))));
        // Lumen off or the program failed: the foundation's close.
        match choose(None, ms(0), &full, false) {
            Some(Close::Plain(a)) => assert_eq!(a, TransformAnim::close(&full, ms(0)).unwrap()),
            other => panic!("{other:?}"),
        }
        // Mid-open, the plain close also starts from the open's pose.
        let open = TransformAnim::open(&full, ms(0), None).unwrap();
        match choose(Some(&open), ms(60), &full, false) {
            Some(Close::Plain(a)) => assert_eq!(a.scale.value(ms(60)), open.scale.value(ms(60))),
            other => panic!("{other:?}"),
        }
        // Reduced is a plain fade even with Lumen on; off is nothing.
        let reduced = AnimationSettings { mode: Mode::Reduced, scale: 1.0 };
        match choose(None, ms(0), &reduced, true) {
            Some(Close::Plain(a)) => {
                assert!(!a.sample(ms(50), Rectangle::from_size((800, 600).into())).moves());
                assert!(a.is_finished(ms(150)));
            }
            other => panic!("{other:?}"),
        }
        let off = AnimationSettings { mode: Mode::Off, scale: 1.0 };
        assert!(choose(None, ms(0), &off, true).is_none());
    }

    #[test]
    fn the_fallback_fades_evenly_to_nothing() {
        assert_eq!(fallback_alpha(0.0), 1.0);
        assert_eq!(fallback_alpha(0.5), 0.5);
        assert_eq!(fallback_alpha(1.0), 0.0);
    }

    #[test]
    fn the_quad_covers_the_snapshot_and_maps_the_frame_to_0_1() {
        // A 800x632 frame (SSD header included) at render origin (100, 50),
        // snapshot exactly the frame, no pose.
        let frame = Rectangle::<f64, Physical>::new((0.0, 0.0).into(), (800.0, 632.0).into());
        let q = quad((100.0, 50.0), (0.0, 0.0), (800.0, 632.0), frame, (500.0, 366.0), (1.0, 1.0), (0.0, 0.0));
        let v = |i: usize| &q.vertices[i * STRIDE_FLOATS..(i + 1) * STRIDE_FLOATS];
        assert_eq!(v(0), &[100.0, 50.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(v(3), &[900.0, 682.0, 1.0, 1.0, 1.0, 1.0]);
        assert_eq!(q.bounds, Rectangle::new((100, 50).into(), (800, 632).into()));
        // Scaled 1.04 about the centre: it grows evenly round it.
        let q = quad((100.0, 50.0), (0.0, 0.0), (800.0, 632.0), frame, (500.0, 366.0), (1.04, 1.04), (0.0, 0.0));
        let v0 = &q.vertices[0..2];
        assert!((v0[0] - 84.0).abs() < 1e-3 && (v0[1] - 37.36).abs() < 1e-3, "{v0:?}");
        // A snapshot with a 10 px margin: its corners lie outside 0..1.
        let q = quad((0.0, 0.0), (-10.0, -10.0), (820.0, 652.0), frame, (0.0, 0.0), (1.0, 1.0), (0.0, 0.0));
        assert!(q.vertices[4] < 0.0 && q.vertices[3 * STRIDE_FLOATS + 4] > 1.0);
    }

    #[test]
    fn the_button_is_the_top_right_corner() {
        let b = button(SIZE);
        assert!(b.0 > 0.97 && b.1 < 0.03, "{b:?}");
        // The far corner is distance 1.
        let d = ((0.0 - b.0) * SIZE.0).hypot((1.0 - b.1) * SIZE.1) / distance_norm(SIZE, b);
        assert!((d - 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_glow_is_mostly_white_tinted_by_the_accent() {
        let g = glow_colour([1.0, 0.0, 0.0]);
        assert!((g[0] - 1.0).abs() < 1e-6 && (g[1] - 0.55).abs() < 1e-6);
    }
}
