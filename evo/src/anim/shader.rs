//! GLES shaders for depth: the drop shadow (a pixel shader) and the rounded
//! clip on client content (a texture shader), and the plumbing to reach them
//! from Evo's generic renderer code.
//!
//! # Why a trait
//!
//! Smithay's `PixelShaderElement`/`TextureShaderElement` implement
//! `RenderElement<GlesRenderer>` only. The udev backend draws through a
//! `MultiRenderer` (its texture type cannot wrap a `GlesTexture`), so the
//! stock elements do not fit. `MultiRenderer` and its frame do hand out the
//! underlying `GlesRenderer`/`GlesFrame` through `AsMut`, which is the
//! supported route: [`GlesAccess`] names it, each backend implements it, and
//! [`ShadowElement`] and [`ClippedSurface`] are `RenderElement<R>` for every
//! `R: GlesAccess`. A backend with no GLES (pixman, say) implements it as
//! `None` and everything here quietly draws nothing.
//!
//! # Reuse (genie warp, and any later custom GLES element)
//!
//! `Programs` compiles once per `GlesRenderer`, kept in its EGL context's user
//! data (so once per GPU under `MultiRenderer`), and a failed compile is
//! remembered and logged once. A new shader adds a source, a field on
//! `Programs`, and an element that draws through `R::gles_frame`, exactly as
//! the two here do. A mesh warp needs a custom *vertex* stage, which smithay's
//! `compile_custom_*_shader` do not expose (they link the fixed vertex shader),
//! so it would draw through `GlesFrame::with_context` with its own program and
//! this same trait; see the notes in `docs/internal/evo-animation-plan.md`.

use smithay::{
    backend::renderer::{
        element::{
            memory::MemoryRenderBufferRenderElement,
            surface::{WaylandSurfaceRenderElement, WaylandSurfaceTexture},
            Element, Id, Kind, RenderElement, UnderlyingStorage,
        },
        gles::{
            GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
            UniformName, UniformType,
        },
        utils::{CommitCounter, DamageSet, OpaqueRegions},
        ImportAll, ImportMem, Renderer, Texture,
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform},
};

use crate::shell::identity::Radii;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use tracing::{info, warn};

/// How a renderer reaches GLES. See the module header.
pub trait GlesAccess: Renderer {
    /// The underlying GLES renderer, if there is one.
    fn gles_renderer(&mut self) -> Option<&mut GlesRenderer>;

    /// Run `f` on the underlying GLES frame; `false` (and `f` not run) if
    /// there is none. A closure rather than a returned reference because the
    /// frame's two lifetimes are tied up in `Self::Frame`.
    fn with_gles_frame(frame: &mut Self::Frame<'_, '_>, f: &mut dyn FnMut(&mut GlesFrame<'_, '_>)) -> bool;

    /// A GLES error as this renderer's own error type (`Renderer::Error` has
    /// no `From<GlesError>` bound a generic caller could use).
    fn gles_error(err: GlesError) -> Self::Error;
}

impl GlesAccess for GlesRenderer {
    fn gles_renderer(&mut self) -> Option<&mut GlesRenderer> {
        Some(self)
    }

    fn with_gles_frame(frame: &mut GlesFrame<'_, '_>, f: &mut dyn FnMut(&mut GlesFrame<'_, '_>)) -> bool {
        f(frame);
        true
    }

    fn gles_error(err: GlesError) -> GlesError {
        err
    }
}

/// Drop shadow: Evan Wallace's closed-form blurred rounded rectangle (a
/// gaussian integrated over the box, erf approximated, the rounded corners
/// handled per row), masked to the outside of the window frame so a
/// translucent title bar is not darkened from behind.
///
/// Uniforms are in physical pixels, relative to the shadow area's top-left:
/// `rect` (x, y, w, h) is the window frame, `shadow_shift` moves the shadow
/// source relative to it (the light is above), `radii` the corner radii
/// (top-left, top-right, bottom-right, bottom-left: the waterdrop), `sigma`
/// the gaussian's deviation, `strength` its peak alpha.
///
/// The closed form integrates the blurred box row by row, so a corner per
/// quadrant is exact: each row's left and right edges take the radius of the
/// corner on their side, top or bottom by the row.
const SHADOW_SHADER: &str = concat!(r#"
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
varying vec2 v_coords;
uniform float alpha;
uniform vec2 area_px;
uniform vec4 rect;
uniform vec2 shadow_shift;
uniform vec4 radii;
uniform float sigma;
uniform float strength;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

float gaussian(float x, float s) {
    return exp(-(x * x) / (2.0 * s * s)) / (2.5066283 * s);
}

vec2 erf2(vec2 x) {
    vec2 s = sign(x);
    vec2 a = abs(x);
    x = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    x *= x;
    return s - s / (x * x);
}

// One row `y` (from the centre, y down) of the rounded box, blurred along x:
// the row's left edge takes its side's corner, the right edge its own.
float box_x(float x, float y, float s, vec4 r, vec2 half_size) {
    float cl = y < 0.0 ? r.x : r.w;
    float cr = y < 0.0 ? r.y : r.z;
    float dl = min(half_size.y - cl - abs(y), 0.0);
    float dr = min(half_size.y - cr - abs(y), 0.0);
    float left = half_size.x - cl + sqrt(max(0.0, cl * cl - dl * dl));
    float right = half_size.x - cr + sqrt(max(0.0, cr * cr - dr * dr));
    vec2 integral = 0.5 + 0.5 * erf2((x + vec2(-right, left)) * (0.70710678 / s));
    return integral.y - integral.x;
}

float box_shadow(vec2 half_size, vec2 point, float s, vec4 corner) {
    float low = point.y - half_size.y;
    float high = point.y + half_size.y;
    float start = clamp(-3.0 * s, low, high);
    float end = clamp(3.0 * s, low, high);
    float step = (end - start) / 4.0;
    float y = start + step * 0.5;
    float value = 0.0;
    for (int i = 0; i < 4; i++) {
        value += box_x(point.x, point.y - y, s, corner, half_size) * gaussian(y, s) * step;
        y += step;
    }
    return value;
}

#"#, crate::radii_glsl!(), r#"
void main() {
    vec2 p = v_coords * area_px;
    vec2 half_size = rect.zw * 0.5;
    vec2 center = rect.xy + half_size;
    float s = max(sigma, 0.5);
    vec4 r = min(radii, vec4(min(half_size.x, half_size.y)));

    // Nothing inside the frame, and nothing past the reach of the blur: skip
    // the integral for most of the area.
    float d = rbox4(p - center, half_size, r);
    if (d <= -0.5 || d - abs(shadow_shift.y) > 3.0 * s) {
        gl_FragColor = vec4(0.0);
        return;
    }
    float shadow = box_shadow(half_size, p - center - shadow_shift, s, r) * strength;
    float outside = smoothstep(-0.5, 0.5, d);

    vec4 color = vec4(0.0, 0.0, 0.0, shadow * outside) * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
"#);

/// Rounded bottom corners for client content. The texture program draws a
/// surface's texture exactly as the default one does, then multiplies it by
/// the coverage of a rounded rectangle whose *bottom* corners are round (the
/// top meets the title bar and stays square).
///
/// Position inside the element comes from `v_coords` (texture UV): with the
/// texture size and the source rectangle, UV maps back to the fraction of the
/// element, and so to physical pixels inside it. That assumes a buffer that is
/// not y-inverted and not rotated (what Gecko, and every toolkit Evo hosts,
/// attaches); the on-device shots would show the top corners rounded instead
/// if that stopped being true.
const CLIP_SHADER: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

uniform vec2 tex_size;
uniform vec4 src_rect;
uniform vec2 dst_size;
uniform vec4 clip_rect;
uniform float radius;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    vec2 p = ((v_coords * tex_size - src_rect.xy) / src_rect.zw) * dst_size;
    vec2 half_size = clip_rect.zw * 0.5;
    vec2 center = clip_rect.xy + half_size;
    vec2 d = p - center;
    float r = d.y > 0.0 ? radius : 0.0;
    vec2 q = abs(d) - half_size + r;
    float dist = min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
    color *= clamp(0.5 - dist, 0.0, 1.0);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
"#;

/// The compiled shaders of one `GlesRenderer`.
#[derive(Clone)]
pub struct Programs {
    pub shadow: GlesPixelProgram,
    pub clip: GlesTexProgram,
}

/// What an EGL context remembers: the programs, or that compiling them failed.
struct ProgramState(Option<Programs>);

fn compile(gles: &mut GlesRenderer) -> Result<Programs, GlesError> {
    let shadow = gles.compile_custom_pixel_shader(
        SHADOW_SHADER,
        &[
            UniformName::new("area_px", UniformType::_2f),
            UniformName::new("rect", UniformType::_4f),
            UniformName::new("shadow_shift", UniformType::_2f),
            UniformName::new("radii", UniformType::_4f),
            UniformName::new("sigma", UniformType::_1f),
            UniformName::new("strength", UniformType::_1f),
        ],
    )?;
    let clip = gles.compile_custom_texture_shader(
        CLIP_SHADER,
        &[
            UniformName::new("tex_size", UniformType::_2f),
            UniformName::new("src_rect", UniformType::_4f),
            UniformName::new("dst_size", UniformType::_2f),
            UniformName::new("clip_rect", UniformType::_4f),
            UniformName::new("radius", UniformType::_1f),
        ],
    )?;
    Ok(Programs { shadow, clip })
}

/// Depth (shadows and rounded corners) switched on or off at run time:
/// `EVO_DEPTH=off` at start, `evo.SetDepth {enabled}` after. Off draws what
/// Evo drew before depth existed, which is also the fallback when the shaders
/// do not compile. Process-wide, like the animation settings.
static ENABLED: AtomicBool = AtomicBool::new(true);
static ENV_READ: std::sync::Once = std::sync::Once::new();

pub fn depth_enabled() -> bool {
    ENV_READ.call_once(|| {
        if std::env::var("EVO_DEPTH").is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "0" | "none")) {
            ENABLED.store(false, Ordering::Relaxed);
        }
    });
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_depth_enabled(enabled: bool) {
    depth_enabled();
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// What compiling the shaders came to: 0 not tried, 1 compiled, 2 failed.
static COMPILED: AtomicU8 = AtomicU8::new(0);

/// `"untried"`, `"compiled"` or `"failed"`, for `GetDepthState`.
pub fn shader_status() -> &'static str {
    match COMPILED.load(Ordering::Relaxed) {
        1 => "compiled",
        2 => "failed",
        _ => "untried",
    }
}

/// The shaders for `renderer`, compiled on first use. `None` when depth is
/// off, the renderer has no GLES or compiling failed (which is logged once,
/// and not retried): the compositor then draws no shadow and no rounding, and
/// carries on.
pub fn programs<R: GlesAccess>(renderer: &mut R) -> Option<Programs> {
    if !depth_enabled() {
        return None;
    }
    let Some(gles) = renderer.gles_renderer() else {
        return None;
    };
    if let Some(state) = gles.egl_context().user_data().get::<ProgramState>() {
        return state.0.clone();
    }
    let result = match compile(gles) {
        Ok(programs) => {
            info!("depth shaders compiled (drop shadow, rounded corners)");
            COMPILED.store(1, Ordering::Relaxed);
            Some(programs)
        }
        Err(err) => {
            warn!("depth shaders failed to compile, drawing no shadows or rounded corners: {err}");
            COMPILED.store(2, Ordering::Relaxed);
            None
        }
    };
    gles.egl_context()
        .user_data()
        .insert_if_missing(|| ProgramState(result.clone()));
    result
}

// === The shadow ===

/// Everything that decides how a shadow looks, in physical pixels. Comparing
/// two of these is how a window's shadow knows whether to repaint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowParams {
    /// The region the shader draws into: the frame grown by the shadow's reach.
    pub area: Rectangle<i32, Physical>,
    /// The window frame, relative to `area`.
    pub rect: Rectangle<f64, Physical>,
    /// How far the shadow's source is moved down from the frame.
    pub shift: (f64, f64),
    pub radii: Radii,
    pub sigma: f64,
    pub strength: f64,
    pub alpha: f32,
}

/// A drop shadow, drawn with the pixel shader. Cheap to clone (a program
/// handle and some numbers). Its `id` and `commit` come from the window's
/// `ShadowTrack` so the damage tracker sees a still shadow as unchanged.
#[derive(Clone)]
pub struct ShadowElement {
    program: GlesPixelProgram,
    id: Id,
    commit: CommitCounter,
    params: ShadowParams,
}

impl ShadowElement {
    pub fn new(program: GlesPixelProgram, id: Id, commit: CommitCounter, params: ShadowParams) -> Self {
        Self { program, id, commit, params }
    }

    pub fn params(&self) -> &ShadowParams {
        &self.params
    }
}

impl std::fmt::Debug for ShadowElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadowElement").field("params", &self.params).finish()
    }
}

impl Element for ShadowElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(Size::from((self.params.area.size.w as f64, self.params.area.size.h as f64)))
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.params.area
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

impl<R: GlesAccess> RenderElement<R> for ShadowElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        let p = &self.params;
        let uniforms = [
            Uniform::new("area_px", (dst.size.w as f32, dst.size.h as f32)),
            Uniform::new(
                "rect",
                (p.rect.loc.x as f32, p.rect.loc.y as f32, p.rect.size.w as f32, p.rect.size.h as f32),
            ),
            Uniform::new("shadow_shift", (p.shift.0 as f32, p.shift.1 as f32)),
            Uniform::new("radii", p.radii.as_f32()),
            Uniform::new("sigma", p.sigma as f32),
            Uniform::new("strength", p.strength as f32),
        ];
        // The shader draws nothing inside the frame, so don't ask it to: take
        // the part of each damage rect that lies inside the rounded frame (the
        // two rectangles that fit in it, which leave the four corner squares,
        // where the mask does the rest) out of the list. This is most of the
        // fill: a shadow is a thin ring round a big window. The largest
        // corner decides the squares left out, so every corner is in them.
        let r = p.radii.max().ceil() as i32;
        let (fx, fy) = (p.rect.loc.x as i32, p.rect.loc.y as i32);
        let (fw, fh) = (p.rect.size.w as i32, p.rect.size.h as i32);
        let inside = [
            Rectangle::new(Point::from((fx + r, fy)), Size::from(((fw - 2 * r).max(0), fh))),
            Rectangle::new(Point::from((fx, fy + r)), Size::from((fw, (fh - 2 * r).max(0)))),
        ];
        let damage: Vec<Rectangle<i32, Physical>> =
            Rectangle::subtract_rects_many(damage.iter().copied(), inside.into_iter().filter(|r| !r.is_empty()));
        if damage.is_empty() {
            return Ok(());
        }
        let mut result = Ok(());
        R::with_gles_frame(frame, &mut |gles| {
            result = gles.render_pixel_shader_to(
                &self.program,
                src,
                dst,
                Size::from((dst.size.w, dst.size.h)),
                Some(&damage),
                p.alpha,
                &uniforms,
            );
        });
        result.map_err(R::gles_error)
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

// === The rounded clip ===

/// A surface element drawn through the clip shader. Wraps the stock element,
/// so its texture, damage and id are the surface's own.
pub struct ClippedSurface<R: Renderer> {
    inner: WaylandSurfaceRenderElement<R>,
    program: GlesTexProgram,
    tex_size: Size<i32, Buffer>,
    /// The rounded rectangle, relative to the element's top-left, in physical
    /// pixels at the element's own (unscaled) geometry.
    clip: Rectangle<f64, Physical>,
    /// The element's size when `clip` was made, so a `Rescale` wrapper's
    /// scaled `dst` gives the factor to apply.
    base: Size<i32, Physical>,
    radius: f64,
}

impl<R: Renderer + ImportAll> ClippedSurface<R> {
    /// `clip` is the rounded rectangle in the same (output-physical)
    /// coordinates as the element's geometry. `None` if the element is a solid
    /// colour (no texture to sample) or has no area.
    pub fn new(
        inner: WaylandSurfaceRenderElement<R>,
        program: GlesTexProgram,
        scale: Scale<f64>,
        clip: Rectangle<i32, Physical>,
        radius: f64,
    ) -> Result<Self, WaylandSurfaceRenderElement<R>>
    where
        R::TextureId: Texture,
    {
        let tex_size = match inner.texture() {
            WaylandSurfaceTexture::Texture(texture) => texture.size(),
            _ => return Err(inner),
        };
        let geo = inner.geometry(scale);
        if geo.size.w <= 0 || geo.size.h <= 0 {
            return Err(inner);
        }
        let clip = Rectangle::new(
            Point::from(((clip.loc.x - geo.loc.x) as f64, (clip.loc.y - geo.loc.y) as f64)),
            Size::from((clip.size.w as f64, clip.size.h as f64)),
        );
        Ok(Self { inner, program, tex_size, clip, base: geo.size, radius })
    }

    /// Whether the element reaches a bottom corner of the clip, i.e. whether
    /// clipping changes anything on it.
    pub fn touches_corner(
        element: &WaylandSurfaceRenderElement<R>,
        scale: Scale<f64>,
        clip: Rectangle<i32, Physical>,
        radius: i32,
    ) -> bool {
        let geo = element.geometry(scale);
        corner_squares(clip, radius).iter().any(|c| c.overlaps(geo))
    }
}

/// The two squares at a clip rectangle's bottom corners that rounding may cut.
pub fn corner_squares(clip: Rectangle<i32, Physical>, radius: i32) -> [Rectangle<i32, Physical>; 2] {
    let r = radius.max(0);
    let bottom = clip.loc.y + clip.size.h - r;
    [
        Rectangle::new(Point::from((clip.loc.x, bottom)), Size::from((r, r))),
        Rectangle::new(Point::from((clip.loc.x + clip.size.w - r, bottom)), Size::from((r, r))),
    ]
}

impl<R: Renderer> std::fmt::Debug for ClippedSurface<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClippedSurface").field("clip", &self.clip).finish()
    }
}

impl<R: Renderer + ImportAll> Element for ClippedSurface<R> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    /// The surface's opaque regions less the two corner squares: rounding
    /// makes those pixels (partly) transparent, so what is behind must be
    /// drawn there rather than culled.
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let clip = Rectangle::new(
            Point::from((self.clip.loc.x.round() as i32, self.clip.loc.y.round() as i32)),
            Size::from((self.clip.size.w.round() as i32, self.clip.size.h.round() as i32)),
        );
        // Opaque regions are relative to the element, and `corner_squares`
        // takes the clip in the same space.
        let corners = corner_squares(clip, self.radius.ceil() as i32 + 1);
        self.inner
            .opaque_regions(scale)
            .iter()
            .flat_map(|region| region.subtract_rects(corners))
            .collect()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<R> RenderElement<R> for ClippedSurface<R>
where
    R: GlesAccess + ImportAll,
    R::TextureId: Texture + 'static,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        let sx = dst.size.w as f64 / self.base.w.max(1) as f64;
        let sy = dst.size.h as f64 / self.base.h.max(1) as f64;
        let uniforms = vec![
            Uniform::new("tex_size", (self.tex_size.w as f32, self.tex_size.h as f32)),
            Uniform::new(
                "src_rect",
                (src.loc.x as f32, src.loc.y as f32, src.size.w as f32, src.size.h as f32),
            ),
            Uniform::new("dst_size", (dst.size.w as f32, dst.size.h as f32)),
            Uniform::new(
                "clip_rect",
                (
                    (self.clip.loc.x * sx) as f32,
                    (self.clip.loc.y * sy) as f32,
                    (self.clip.size.w * sx) as f32,
                    (self.clip.size.h * sy) as f32,
                ),
            ),
            Uniform::new("radius", (self.radius * (sx + sy) / 2.0) as f32),
        ];
        R::with_gles_frame(frame, &mut |gles| {
            gles.override_default_tex_program(self.program.clone(), uniforms.clone());
        });
        let result = RenderElement::<R>::draw(&self.inner, frame, src, dst, damage, opaque_regions);
        R::with_gles_frame(frame, &mut |gles| gles.clear_tex_program_override());
        result
    }

    /// Never scanned out: the corners have to be composited.
    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

// === Bloom (Lumen's open, `anim::bloom`) ===
//
// Its own programs and status, so Bloom works with depth off and depth with
// Lumen off. Same shape as the depth programs above.

/// Bloom's live-surface pass: the surface's texture, blurred by a 13-tap
/// two-ring disc of radius `blur_uv` (in UV units; 0 samples once), tinted
/// toward the accent, then clipped to the frame's rounded rectangle (each
/// corner at its own of `radii`) with an edge softened by `soft` pixels, so the
/// silhouette goes out of focus with the content.
///
/// The disc is turned by a per-pixel angle (a hash of the fragment's
/// position): the taps' ghost copies become a fine grain, which reads as
/// frosted glass, not as a doubled image. Position inside the element comes
/// from `v_coords`, as in `CLIP_SHADER` (the same buffer assumption holds).
/// `accent` is not called `tint`: smithay's debug build adds a `tint` uniform.
const BLOOM_SHADER: &str = concat!(r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

uniform vec2 tex_size;
uniform vec4 src_rect;
uniform vec2 dst_size;
uniform vec4 clip_rect;
uniform vec4 radii;
uniform vec2 blur_uv;
uniform float soft;
uniform vec4 accent;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

vec4 tap(vec2 uv) {
    vec4 c = texture2D(tex, uv);
#if defined(NO_ALPHA)
    c.a = 1.0;
#endif
    return c;
}
"#, crate::radii_glsl!(), r#"

void main() {
    vec4 color;
    if (blur_uv.x > 0.0) {
        float a = fract(sin(dot(gl_FragCoord.xy, vec2(12.9898, 78.233))) * 43758.5453) * 6.2831853;
        vec2 cs = vec2(cos(a), sin(a));
        mat2 rot = mat2(cs.x, cs.y, -cs.y, cs.x);
        vec2 i1 = rot * vec2(0.45, 0.0) * blur_uv;
        vec2 i2 = rot * vec2(0.0, 0.45) * blur_uv;
        vec2 o1 = rot * vec2(0.878, 0.364) * blur_uv;
        vec2 o2 = rot * vec2(0.364, 0.878) * blur_uv;
        vec2 o3 = rot * vec2(-0.364, 0.878) * blur_uv;
        vec2 o4 = rot * vec2(-0.878, 0.364) * blur_uv;
        color = tap(v_coords) * 0.16;
        color += (tap(v_coords + i1) + tap(v_coords - i1) + tap(v_coords + i2) + tap(v_coords - i2)) * 0.09;
        color += (tap(v_coords + o1) + tap(v_coords - o1) + tap(v_coords + o2) + tap(v_coords - o2)
                + tap(v_coords + o3) + tap(v_coords - o3) + tap(v_coords + o4) + tap(v_coords - o4)) * 0.06;
    } else {
        color = tap(v_coords);
    }
    color = color * alpha;
    color.rgb = mix(color.rgb, accent.rgb * color.a, accent.a);

    vec2 p = ((v_coords * tex_size - src_rect.xy) / src_rect.zw) * dst_size;
    vec2 half_size = clip_rect.zw * 0.5;
    vec4 r = min(radii, vec4(min(half_size.x, half_size.y)));
    float dist = rbox4(p - clip_rect.xy - half_size, half_size, r);
    color *= clamp((0.5 - dist) / (1.0 + soft), 0.0, 1.0);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
"#);

/// Bloom's glow: a soft radial falloff of the accent, `(1 - r²)²`, filling the
/// square it is drawn into. Premultiplied, so it lightens what it covers.
const GLOW_SHADER: &str = r#"
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
varying vec2 v_coords;
uniform float alpha;
uniform vec3 accent;
uniform float strength;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec2 d = v_coords * 2.0 - 1.0;
    float f = max(1.0 - dot(d, d), 0.0);
    float a = strength * f * f * alpha;
    gl_FragColor = vec4(accent * a, a);
}
"#;

/// Bloom's compiled programs for one `GlesRenderer`.
#[derive(Clone)]
pub struct BloomPrograms {
    pub surface: GlesTexProgram,
    pub glow: GlesPixelProgram,
}

struct BloomProgramState(Option<BloomPrograms>);

fn compile_bloom(gles: &mut GlesRenderer) -> Result<BloomPrograms, GlesError> {
    let surface = gles.compile_custom_texture_shader(
        BLOOM_SHADER,
        &[
            UniformName::new("tex_size", UniformType::_2f),
            UniformName::new("src_rect", UniformType::_4f),
            UniformName::new("dst_size", UniformType::_2f),
            UniformName::new("clip_rect", UniformType::_4f),
            UniformName::new("radii", UniformType::_4f),
            UniformName::new("blur_uv", UniformType::_2f),
            UniformName::new("soft", UniformType::_1f),
            UniformName::new("accent", UniformType::_4f),
        ],
    )?;
    let glow = gles.compile_custom_pixel_shader(
        GLOW_SHADER,
        &[
            UniformName::new("accent", UniformType::_3f),
            UniformName::new("strength", UniformType::_1f),
        ],
    )?;
    Ok(BloomPrograms { surface, glow })
}

/// Bloom's programs for `renderer`, compiled on first use. `None` when Bloom
/// is unavailable (Lumen off), the renderer has no GLES or compiling failed
/// (logged once, not retried): the opening window is then drawn through the
/// plain transform.
pub fn bloom_programs<R: GlesAccess>(renderer: &mut R) -> Option<BloomPrograms> {
    use super::bloom::PROGRAM;
    if !super::bloom::available() {
        return None;
    }
    let Some(gles) = renderer.gles_renderer() else {
        PROGRAM.store(2, Ordering::Relaxed);
        return None;
    };
    if let Some(state) = gles.egl_context().user_data().get::<BloomProgramState>() {
        return state.0.clone();
    }
    let result = match compile_bloom(gles) {
        Ok(programs) => {
            info!("bloom shaders compiled (live blur, tint and corners; glow)");
            PROGRAM.store(1, Ordering::Relaxed);
            Some(programs)
        }
        Err(err) => {
            warn!("bloom shaders failed to compile, opening windows with the plain transform: {err}");
            PROGRAM.store(2, Ordering::Relaxed);
            None
        }
    };
    gles.egl_context()
        .user_data()
        .insert_if_missing(|| BloomProgramState(result.clone()));
    result
}

impl<R: Renderer> ClippedSurface<R> {
    /// The surface element without the clip (Bloom draws it its own way).
    pub fn into_inner(self) -> WaylandSurfaceRenderElement<R> {
        self.inner
    }
}

/// Bloom's look for one frame, in output-physical pixels at the window's full
/// (unposed) size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BloomParams {
    pub radii: Radii,
    /// Blur radius; 0 samples once.
    pub blur: f64,
    pub tint: f32,
    pub accent: [f32; 3],
}

/// What a `BloomSurface` draws: a client surface or a piece of the title bar.
pub enum BloomInner<R: Renderer> {
    Surface(WaylandSurfaceRenderElement<R>),
    Memory(MemoryRenderBufferRenderElement<R>),
}

impl<R: Renderer + ImportAll> BloomInner<R> {
    fn element(&self) -> &dyn Element {
        match self {
            BloomInner::Surface(e) => e,
            BloomInner::Memory(e) => e,
        }
    }
}

/// A textured element drawn through Bloom's shader. It carries a fresh `Id`
/// every frame: its look changes every frame though the surface's commit
/// does not, and a new id is how the damage tracker is told to repaint it
/// (and, when Bloom ends, to repaint the plain element that replaces it).
pub struct BloomSurface<R: Renderer> {
    inner: BloomInner<R>,
    id: Id,
    program: GlesTexProgram,
    tex_size: Size<i32, Buffer>,
    /// The frame's rectangle relative to the element's top-left, physical
    /// pixels at the element's own (unscaled) geometry.
    clip: Rectangle<f64, Physical>,
    base: Size<i32, Physical>,
    params: BloomParams,
}

impl<R: Renderer + ImportAll> BloomSurface<R>
where
    R::TextureId: Texture,
{
    /// `frame` is the window's visible frame in the element's (output
    /// physical) coordinates. Gives the element back if it has no texture to
    /// sample (a solid colour) or no area.
    pub fn new(
        inner: BloomInner<R>,
        program: GlesTexProgram,
        scale: Scale<f64>,
        frame: Rectangle<i32, Physical>,
        params: BloomParams,
    ) -> Result<Self, BloomInner<R>> {
        let tex_size = match &inner {
            BloomInner::Surface(e) => match e.texture() {
                WaylandSurfaceTexture::Texture(texture) => texture.size(),
                _ => return Err(inner),
            },
            // A title-bar piece is one whole memory buffer at scale 1, so its
            // texture is the size of its source.
            BloomInner::Memory(e) => {
                let src = e.src();
                Size::from((src.size.w.round() as i32, src.size.h.round() as i32))
            }
        };
        let geo = inner.element().geometry(scale);
        if geo.size.w <= 0 || geo.size.h <= 0 || tex_size.w <= 0 || tex_size.h <= 0 {
            return Err(inner);
        }
        let clip = Rectangle::new(
            Point::from(((frame.loc.x - geo.loc.x) as f64, (frame.loc.y - geo.loc.y) as f64)),
            Size::from((frame.size.w as f64, frame.size.h as f64)),
        );
        Ok(Self { inner, id: Id::new(), program, tex_size, clip, base: geo.size, params })
    }
}

impl<R: Renderer> std::fmt::Debug for BloomSurface<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BloomSurface").field("clip", &self.clip).field("params", &self.params).finish()
    }
}

impl<R: Renderer + ImportAll> Element for BloomSurface<R> {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.element().current_commit()
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.element().location(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.element().src()
    }

    fn transform(&self) -> Transform {
        self.inner.element().transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.element().geometry(scale)
    }

    /// Nothing is opaque: the corners, the soft edge and the fade all let
    /// what is behind show.
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.inner.element().alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.element().kind()
    }
}

impl<R> RenderElement<R> for BloomSurface<R>
where
    R: GlesAccess + ImportAll + ImportMem,
    R::TextureId: Texture + 'static,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        let p = &self.params;
        let sx = dst.size.w as f64 / self.base.w.max(1) as f64;
        let sy = dst.size.h as f64 / self.base.h.max(1) as f64;
        let k = (sx + sy) / 2.0;
        // Blur in the drawn pixels, then in texels: a texel spans
        // src/dst pixels of the drawn element.
        let blur = p.blur * k;
        let blur_uv = if blur > 0.0 {
            (
                (blur * src.size.w / dst.size.w.max(1) as f64 / self.tex_size.w as f64) as f32,
                (blur * src.size.h / dst.size.h.max(1) as f64 / self.tex_size.h as f64) as f32,
            )
        } else {
            (0.0, 0.0)
        };
        let uniforms = vec![
            Uniform::new("tex_size", (self.tex_size.w as f32, self.tex_size.h as f32)),
            Uniform::new(
                "src_rect",
                (src.loc.x as f32, src.loc.y as f32, src.size.w as f32, src.size.h as f32),
            ),
            Uniform::new("dst_size", (dst.size.w as f32, dst.size.h as f32)),
            Uniform::new(
                "clip_rect",
                (
                    (self.clip.loc.x * sx) as f32,
                    (self.clip.loc.y * sy) as f32,
                    (self.clip.size.w * sx) as f32,
                    (self.clip.size.h * sy) as f32,
                ),
            ),
            Uniform::new("radii", p.radii.scale(k).as_f32()),
            Uniform::new("blur_uv", blur_uv),
            Uniform::new("soft", (blur * 0.8) as f32),
            Uniform::new("accent", (p.accent[0], p.accent[1], p.accent[2], p.tint)),
        ];
        R::with_gles_frame(frame, &mut |gles| {
            gles.override_default_tex_program(self.program.clone(), uniforms.clone());
        });
        let result = match &self.inner {
            BloomInner::Surface(e) => RenderElement::<R>::draw(e, frame, src, dst, damage, opaque_regions),
            BloomInner::Memory(e) => RenderElement::<R>::draw(e, frame, src, dst, damage, opaque_regions),
        };
        R::with_gles_frame(frame, &mut |gles| gles.clear_tex_program_override());
        result
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

/// Bloom's glow behind an opening window, drawn with the pixel shader.
#[derive(Clone)]
pub struct GlowElement {
    program: GlesPixelProgram,
    id: Id,
    area: Rectangle<i32, Physical>,
    accent: [f32; 3],
    strength: f32,
    alpha: f32,
}

impl GlowElement {
    pub fn new(
        program: GlesPixelProgram,
        id: Id,
        area: Rectangle<i32, Physical>,
        accent: [f32; 3],
        strength: f32,
        alpha: f32,
    ) -> Self {
        Self { program, id, area, accent, strength, alpha }
    }
}

impl std::fmt::Debug for GlowElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlowElement")
            .field("area", &self.area)
            .field("strength", &self.strength)
            .finish()
    }
}

impl Element for GlowElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        CommitCounter::default()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(Size::from((self.area.size.w as f64, self.area.size.h as f64)))
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.area
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

impl<R: GlesAccess> RenderElement<R> for GlowElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        let uniforms = [
            Uniform::new("accent", (self.accent[0], self.accent[1], self.accent[2])),
            Uniform::new("strength", self.strength),
        ];
        let mut result = Ok(());
        R::with_gles_frame(frame, &mut |gles| {
            result = gles.render_pixel_shader_to(
                &self.program,
                src,
                dst,
                Size::from((dst.size.w, dst.size.h)),
                Some(damage),
                self.alpha,
                &uniforms,
            );
        });
        result.map_err(R::gles_error)
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

// === Frost (the title bar's glass, `shell/frost.rs`) ===
//
// Its own program and status, like Bloom's: a window frame still draws its
// shadow with frost off, and frost falls back to the flat bar when this does
// not compile.

/// The frosted bar: a crop of the veiled wallpaper (`tex`, sampled as the
/// element's `src` says), with grain from a hash (an overlay, `grain` strong),
/// a rim of white along the top (`rim` alpha), a seam of the app's accent
/// along the bottom (`seam` rgb and alpha), both `px` thick, all clipped to the
/// bar's rounded top corners at the drawn resolution, so the corners stay
/// crisp though the frost is a quarter-size texture. Position inside the
/// element comes from `v_coords`, as in `CLIP_SHADER`. The texture is opaque
/// and straight; the output is premultiplied.
pub(crate) const FROST_SHADER: &str = concat!(r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

uniform vec2 tex_size;
uniform vec4 src_rect;
uniform vec2 dst_size;
uniform vec4 radii;
uniform float px;
uniform float grain;
uniform float rim;
uniform vec4 seam;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// A hash of a pixel's place: static noise, so the grain neither swims nor
// shimmers (Dave Hoskins' hash12, no sin).
float hash12(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

float overlay1(float base, float n) {
    return base < 0.5 ? 2.0 * base * n : 1.0 - 2.0 * (1.0 - base) * (1.0 - n);
}
"#, crate::radii_glsl!(), r#"
void main() {
    vec3 rgb = texture2D(tex, v_coords).rgb;
    vec2 p = ((v_coords * tex_size - src_rect.xy) / src_rect.zw) * dst_size;

    float n = hash12(floor(p));
    vec3 noisy = vec3(overlay1(rgb.r, n), overlay1(rgb.g, n), overlay1(rgb.b, n));
    rgb = mix(rgb, noisy, grain);

    float top = clamp(px - p.y, 0.0, 1.0);
    rgb = mix(rgb, vec3(1.0), rim * top);
    float bottom = clamp(px - (dst_size.y - p.y), 0.0, 1.0);
    rgb = mix(rgb, seam.rgb, seam.a * bottom);

    vec2 half_size = dst_size * 0.5;
    vec4 r = min(radii, vec4(min(half_size.x, half_size.y)));
    float cover = clamp(0.5 - rbox4(p - half_size, half_size, r), 0.0, 1.0);

    vec4 color = vec4(rgb, 1.0) * (cover * alpha);
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
"#);

struct FrostProgramState(Option<GlesTexProgram>);

/// What compiling the frost shader came to: 0 not tried, 1 compiled, 2 failed.
static FROST_COMPILED: AtomicU8 = AtomicU8::new(0);

/// `"untried"`, `"compiled"` or `"failed"`, for `GetFrostState`.
pub fn frost_status() -> &'static str {
    match FROST_COMPILED.load(Ordering::Relaxed) {
        1 => "compiled",
        2 => "failed",
        _ => "untried",
    }
}

fn compile_frost(gles: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    gles.compile_custom_texture_shader(
        FROST_SHADER,
        &[
            UniformName::new("tex_size", UniformType::_2f),
            UniformName::new("src_rect", UniformType::_4f),
            UniformName::new("dst_size", UniformType::_2f),
            UniformName::new("radii", UniformType::_4f),
            UniformName::new("px", UniformType::_1f),
            UniformName::new("grain", UniformType::_1f),
            UniformName::new("rim", UniformType::_1f),
            UniformName::new("seam", UniformType::_4f),
        ],
    )
}

/// The frost program for `renderer`, compiled on first use. `None` with no
/// GLES or after a failed compile (logged once, not retried): bars then draw
/// the flat fill.
pub fn frost_program<R: GlesAccess>(renderer: &mut R) -> Option<GlesTexProgram> {
    let gles = renderer.gles_renderer()?;
    if let Some(state) = gles.egl_context().user_data().get::<FrostProgramState>() {
        return state.0.clone();
    }
    let result = match compile_frost(gles) {
        Ok(program) => {
            info!("frost shader compiled");
            FROST_COMPILED.store(1, Ordering::Relaxed);
            Some(program)
        }
        Err(err) => {
            warn!("frost shader failed to compile, title bars draw flat: {err}");
            FROST_COMPILED.store(2, Ordering::Relaxed);
            None
        }
    };
    gles.egl_context().user_data().insert_if_missing(|| FrostProgramState(result.clone()));
    result
}

/// How one frost layer is drawn, physical px at the element's own size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrostParams {
    /// The bar's corners: top-left and top-right round, the bottom square.
    pub radii: Radii,
    /// Physical px per logical px: the thickness of the rim and the seam.
    pub px: f32,
    pub grain: f32,
    pub rim: f32,
    /// The seam's colour and alpha.
    pub seam: [f32; 4],
    pub alpha: f32,
}

/// A title bar's frost, drawn with the frost program from a crop of a shared
/// texture. Cheap to clone (a program and a texture handle). A window's bar
/// keeps one `Id` and a commit counter that moves only when what it draws
/// does, so a bar at rest costs no damage; it is a `WindowRenderElement`, so
/// it follows its window through every pose and every snapshot.
#[derive(Clone)]
pub struct FrostElement {
    program: GlesTexProgram,
    texture: GlesTexture,
    id: Id,
    commit: CommitCounter,
    geo: Rectangle<i32, Physical>,
    src: Rectangle<f64, Buffer>,
    params: FrostParams,
}

impl FrostElement {
    pub fn new(
        program: GlesTexProgram,
        texture: GlesTexture,
        id: Id,
        commit: CommitCounter,
        geo: Rectangle<i32, Physical>,
        src: Rectangle<f64, Buffer>,
        params: FrostParams,
    ) -> Self {
        Self { program, texture, id, commit, geo, src, params }
    }

    pub fn params(&self) -> &FrostParams {
        &self.params
    }

    /// The same bar with other corners (Bloom's open lands them), and a fresh
    /// `Id`: the corners change every frame though the counter does not, and a
    /// new id is how the damage tracker is told to repaint.
    pub fn with_radii(mut self, radii: Radii) -> Self {
        self.params.radii = radii;
        self.id = Id::new();
        self
    }
}

impl std::fmt::Debug for FrostElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrostElement").field("geo", &self.geo).field("params", &self.params).finish()
    }
}

impl Element for FrostElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.src
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geo
    }

    /// Opaque below the corner rows when the bar is at full alpha, so what is
    /// beneath (the shadow, the desktop) isn't drawn under it.
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let r = self.params.radii.max().ceil() as i32;
        if self.params.alpha < 1.0 || self.geo.size.h <= r {
            return OpaqueRegions::default();
        }
        OpaqueRegions::from_slice(&[Rectangle::new(Point::from((0, r)), Size::from((self.geo.size.w, self.geo.size.h - r)))])
    }

    fn alpha(&self) -> f32 {
        self.params.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl<R: GlesAccess> RenderElement<R> for FrostElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        let p = &self.params;
        // A pose may have scaled the drawn rect (`Rescale`): the corners, the
        // rim and the seam scale with it.
        let k = dst.size.w as f64 / self.geo.size.w.max(1) as f64;
        let tex = self.texture.size();
        let uniforms = [
            Uniform::new("tex_size", (tex.w as f32, tex.h as f32)),
            Uniform::new("src_rect", (src.loc.x as f32, src.loc.y as f32, src.size.w as f32, src.size.h as f32)),
            Uniform::new("dst_size", (dst.size.w as f32, dst.size.h as f32)),
            Uniform::new("radii", p.radii.scale(k).as_f32()),
            Uniform::new("px", p.px * k as f32),
            Uniform::new("grain", p.grain),
            Uniform::new("rim", p.rim),
            Uniform::new("seam", (p.seam[0], p.seam[1], p.seam[2], p.seam[3])),
        ];
        let mut result = Ok(());
        R::with_gles_frame(frame, &mut |gles| {
            result = gles.render_texture_from_to(
                &self.texture,
                src,
                dst,
                damage,
                &[],
                Transform::Normal,
                p.alpha,
                Some(&self.program),
                &uniforms,
            );
        });
        result.map_err(R::gles_error)
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    /// Writes every shader the frame's corners reach into `$EVO_SHADER_DUMP`,
    /// for a GLES compile on the host (`cargo test dump_shaders -- --ignored`
    /// with the variable set). The unit tests can't compile GLSL; a typo here
    /// would otherwise show only on device, as a logged fallback.
    #[test]
    #[ignore]
    fn dump_shaders() {
        let Ok(dir) = std::env::var("EVO_SHADER_DUMP") else { return };
        let dir = std::path::Path::new(&dir);
        for (name, src) in [
            ("pixel-shadow.frag", super::SHADOW_SHADER),
            ("pixel-glow.frag", super::GLOW_SHADER),
            ("pixel-glint.frag", super::super::glint::GLINT_SHADER),
            ("tex-clip.frag", super::CLIP_SHADER),
            ("tex-bloom.frag", super::BLOOM_SHADER),
            ("tex-frost.frag", super::FROST_SHADER),
            ("lumen.vert", super::super::lumen::VERTEX),
            ("lumen.frag", super::super::lumen::FRAGMENT),
        ] {
            std::fs::write(dir.join(name), src).unwrap();
        }
    }
}
