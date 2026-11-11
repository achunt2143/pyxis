//! Lumen's Settle glint: at the end of a geometry transition (maximize,
//! unmaximize, a fit) a soft diagonal band of white light crosses the window,
//! clipped to its frame. Spec: `docs/internal/evo-animation-plan.md`, Lumen,
//! "Settle".
//!
//! The band starts at [`START`] of the transition's nominal duration and
//! crosses the frame by its end; its peak alpha is `PEAK × sin(π g)` with `g`
//! the glint's own progress (0..1), so it brightens in and fades out as it
//! goes and never pops. Full motion only: reduced and off have no glint.
//!
//! A pixel shader drawn over the window, with its own program kept in the EGL
//! context's user data beside lane D's (`anim::shader`), reached through the
//! same [`GlesAccess`] route. If it fails to compile (logged once) the
//! transition is simply drawn without it; with Lumen off (`EVO_LUMEN=off`,
//! `SetLumen`) it is not drawn either. Frames count under `GetLumenState`'s
//! `frames.glint`, and `glint_program` there is [`program_status`].

use std::sync::atomic::{AtomicU8, Ordering};

use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{GlesPixelProgram, GlesRenderer, Uniform, UniformName, UniformType},
        utils::{CommitCounter, OpaqueRegions},
    },
    utils::{Buffer, Physical, Rectangle, Scale, Size},
};
use tracing::{info, warn};

use super::{
    lumen::{self, Effect},
    shader::GlesAccess,
};

/// Where in the transition (fraction of its nominal duration) the glint
/// starts; it ends with the transition.
pub const START: f64 = 0.42;
/// Peak alpha of the band, at the middle of its crossing.
pub const PEAK: f64 = 0.32;
/// Half-width of the band's soft profile, as a fraction of the window's
/// diagonal extent, clamped to [`BAND_MIN_PX`, `BAND_MAX_PX`] logical px.
const BAND_FRACTION: f64 = 0.11;
const BAND_MIN_PX: f64 = 36.0;
const BAND_MAX_PX: f64 = 180.0;

/// The glint's own progress for a transition `p` of the way through (by its
/// nominal duration): 0 before [`START`], 1 at the end. `None` outside the
/// glint, so a caller draws nothing.
pub fn progress(p: f64) -> Option<f64> {
    if !(START..1.0).contains(&p) {
        return None;
    }
    Some(((p - START) / (1.0 - START)).clamp(0.0, 1.0))
}

/// The band's peak alpha at glint progress `g`.
pub fn intensity(g: f64) -> f64 {
    PEAK * (std::f64::consts::PI * g.clamp(0.0, 1.0)).sin()
}

/// The band's half-width in logical px for a `w`×`h` frame.
pub fn band_px(w: f64, h: f64) -> f64 {
    (BAND_FRACTION * (w + SLOPE * h)).clamp(BAND_MIN_PX, BAND_MAX_PX)
}

/// The band runs along `x + SLOPE·y = c`: a shallow diagonal, top left to
/// bottom right, as light off a pane tilted towards the viewer.
const SLOPE: f64 = 0.6;

/// Uniforms in physical pixels relative to the frame's top-left: `size` the
/// frame, `radius` its corner radius (the frame's corners at this moment),
/// `centre` the band's position along `x + slope·y`, `band` its half-width,
/// `peak` its alpha. Premultiplied white.
const GLINT_SHADER: &str = r#"
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
varying vec2 v_coords;
uniform float alpha;
uniform vec2 size;
uniform float radius;
uniform float slope;
uniform float centre;
uniform float band;
uniform float peak;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec2 p = v_coords * size;
    vec2 half_size = size * 0.5;
    float r = min(radius, min(half_size.x, half_size.y));
    vec2 q = abs(p - half_size) - half_size + r;
    float dist = min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
    float inside = clamp(0.5 - dist, 0.0, 1.0);

    float s = (p.x + slope * p.y - centre) / max(band, 1.0);
    // A soft core with a long, faint tail on each side.
    float a = peak * (0.75 * exp(-s * s * 2.2) + 0.25 * exp(-s * s * 0.35)) * inside * alpha;

    vec4 color = vec4(a, a, a, a);
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
"#;

/// The band's centre along `x + SLOPE·y`, physical px, at glint progress `g`
/// for a frame `w`×`h` with half-width `band`: from fully off the top-left
/// corner (g = 0) to fully off the bottom-right one (g = 1).
pub fn centre(g: f64, w: f64, h: f64, band: f64) -> f64 {
    let reach = 2.2 * band;
    let from = -reach;
    let to = w + SLOPE * h + reach;
    from + (to - from) * g.clamp(0.0, 1.0)
}

struct GlintProgram(Option<GlesPixelProgram>);

static COMPILED: AtomicU8 = AtomicU8::new(0);

/// `"untried"`, `"compiled"` or `"failed"`.
pub fn program_status() -> &'static str {
    match COMPILED.load(Ordering::Relaxed) {
        1 => "compiled",
        2 => "failed",
        _ => "untried",
    }
}

fn compile(gles: &mut GlesRenderer) -> Option<GlesPixelProgram> {
    match gles.compile_custom_pixel_shader(
        GLINT_SHADER,
        &[
            UniformName::new("size", UniformType::_2f),
            UniformName::new("radius", UniformType::_1f),
            UniformName::new("slope", UniformType::_1f),
            UniformName::new("centre", UniformType::_1f),
            UniformName::new("band", UniformType::_1f),
            UniformName::new("peak", UniformType::_1f),
        ],
    ) {
        Ok(program) => {
            info!("glint shader compiled");
            COMPILED.store(1, Ordering::Relaxed);
            Some(program)
        }
        Err(err) => {
            warn!("glint shader failed to compile, settling without a glint: {err}");
            COMPILED.store(2, Ordering::Relaxed);
            None
        }
    }
}

/// The glint program for `renderer`, compiled on first use; `None` with no
/// GLES or after a failed compile (not retried).
pub fn program<R: GlesAccess>(renderer: &mut R) -> Option<GlesPixelProgram> {
    let gles = renderer.gles_renderer()?;
    if let Some(state) = gles.egl_context().user_data().get::<GlintProgram>() {
        return state.0.clone();
    }
    let program = compile(gles);
    gles.egl_context()
        .user_data()
        .insert_if_missing(|| GlintProgram(program.clone()));
    program
}

/// Per-transition identity: the element keeps one id for its life, and its
/// commit moves every frame it is drawn (the band moves every frame).
#[derive(Debug, Default)]
pub struct GlintTrack {
    id: Option<Id>,
    commit: CommitCounter,
}

impl GlintTrack {
    /// The element for this frame. `frame` is where the window is drawn
    /// (output-physical), `radius` its corners now (physical px), `g` the
    /// glint's progress, `px` physical px per logical px (for the band width).
    pub fn element(
        &mut self,
        program: GlesPixelProgram,
        frame: Rectangle<i32, Physical>,
        radius: f64,
        g: f64,
        px: f64,
    ) -> Option<GlintElement> {
        if frame.size.w <= 0 || frame.size.h <= 0 {
            return None;
        }
        let peak = intensity(g);
        if peak <= 0.0 {
            return None;
        }
        let (w, h) = (frame.size.w as f64, frame.size.h as f64);
        let band = band_px(w / px.max(1e-6), h / px.max(1e-6)) * px;
        self.commit.increment();
        // `GetLumenState`'s `frames.glint` and `last_progress.glint`.
        lumen::count_frame(Effect::Glint);
        lumen::record_progress(Effect::Glint, g);
        Some(GlintElement {
            program,
            id: self.id.get_or_insert_with(Id::new).clone(),
            commit: self.commit,
            area: frame,
            radius: radius as f32,
            centre: centre(g, w, h, band) as f32,
            band: band as f32,
            peak: peak as f32,
        })
    }
}

/// The band, drawn over the window with the pixel shader.
#[derive(Clone)]
pub struct GlintElement {
    program: GlesPixelProgram,
    id: Id,
    commit: CommitCounter,
    area: Rectangle<i32, Physical>,
    radius: f32,
    centre: f32,
    band: f32,
    peak: f32,
}

impl std::fmt::Debug for GlintElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlintElement")
            .field("area", &self.area)
            .field("centre", &self.centre)
            .field("peak", &self.peak)
            .finish()
    }
}

impl Element for GlintElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
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

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl<R: GlesAccess> RenderElement<R> for GlintElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        // `dst` can differ from `area` only by rounding; scale the band with it.
        let k = dst.size.w as f32 / self.area.size.w.max(1) as f32;
        let uniforms = [
            Uniform::new("size", (dst.size.w as f32, dst.size.h as f32)),
            Uniform::new("radius", self.radius * k),
            Uniform::new("slope", SLOPE as f32),
            Uniform::new("centre", self.centre * k),
            Uniform::new("band", self.band * k),
            Uniform::new("peak", self.peak),
        ];
        let mut result = Ok(());
        R::with_gles_frame(frame, &mut |gles| {
            result = gles.render_pixel_shader_to(
                &self.program,
                src,
                dst,
                Size::from((dst.size.w, dst.size.h)),
                Some(damage),
                1.0,
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
    use super::*;

    #[test]
    fn the_glint_runs_from_forty_two_percent_to_the_end() {
        assert_eq!(progress(0.0), None);
        assert_eq!(progress(0.41), None);
        assert_eq!(progress(START), Some(0.0));
        assert!((progress(0.71).unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(progress(1.0), None, "over with the transition");
    }

    #[test]
    fn it_fades_in_and_out_peaking_at_point_three_two() {
        assert_eq!(intensity(0.0), 0.0);
        assert!((intensity(0.5) - 0.32).abs() < 1e-12);
        assert!(intensity(1.0).abs() < 1e-12);
        let mut last = 0.0;
        for k in 0..=50 {
            let a = intensity(k as f64 / 100.0);
            assert!(a >= last, "rises to the middle");
            last = a;
        }
    }

    #[test]
    fn the_band_crosses_the_whole_frame_from_corner_to_corner() {
        let (w, h) = (1280.0, 700.0);
        let band = band_px(w, h);
        assert!((BAND_MIN_PX..=BAND_MAX_PX).contains(&band));
        // Off the top-left corner (x + slope·y = 0) at the start, off the
        // bottom-right one at the end, with the soft profile clear of both.
        assert!(centre(0.0, w, h, band) <= -2.0 * band);
        assert!(centre(1.0, w, h, band) >= w + SLOPE * h + 2.0 * band);
        let mut last = f64::MIN;
        for k in 0..=20 {
            let c = centre(k as f64 / 20.0, w, h, band);
            assert!(c > last, "moves one way");
            last = c;
        }
    }

    #[test]
    fn a_small_window_gets_a_narrower_band_within_bounds() {
        assert_eq!(band_px(100.0, 80.0), BAND_MIN_PX);
        assert_eq!(band_px(4000.0, 3000.0), BAND_MAX_PX);
        assert!(band_px(800.0, 600.0) < band_px(1280.0, 700.0));
    }
}
