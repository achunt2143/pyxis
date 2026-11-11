//! Bloom, Lumen's open: a window arrives like light coming into focus.
//!
//! The `TransformAnim` (`TransformAnim::bloom`) carries scale, rise and alpha,
//! so counters, retargets and a close mid-open (`minimize::close_from`) work
//! as they do for any open. `BloomFx` carries the rest, all a function of the
//! open's time progress `p` (0..1, linear in time): the blur (the focus pull),
//! the corner radius, the accent tint and the glow behind the window. See
//! `docs/internal/evo-animation-plan.md`, "Lumen".
//!
//! An opening window has no snapshot, so the blur, tint and rounding run on
//! the *live* surface: `render::stack_elements` hands the window's elements to
//! [`apply`], which swaps each textured one for a `shader::BloomSurface` (a
//! texture shader, the way lane D's `ClippedSurface` rounds corners), and
//! draws [`glow`] beneath them. With no program (Lumen off, the shader failed
//! to compile, no GLES) the window is drawn through the plain transform: still
//! Bloom's scale, rise and fade.

use std::{
    sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
    time::Duration,
};

use smithay::{
    backend::renderer::{
        element::{Element, Id},
        ImportAll, ImportMem, Renderer, Texture,
    },
    utils::{Physical, Point, Rectangle, Scale, Size},
};

use super::{
    lumen::{self, Effect},
    settings::AnimationSettings,
    shader::{self, BloomInner, BloomParams, BloomSurface, GlesAccess, GlowElement},
    spec,
};
use crate::shell::{ssd::DecorationRenderElement, WindowRenderElement};

/// A neutral cool white, for an app whose accent Evo has not been told (a
/// window only now appearing has no picture to take an average from).
pub const NEUTRAL_ACCENT: [f32; 3] = [0.86, 0.90, 1.0];

/// Below this many pixels of blur the surface is sampled once (the blur's
/// taps would all land within a pixel of the centre).
pub const BLUR_MIN_PX: f64 = 0.3;

/// The per-window half of a Bloom: when it started, how long it runs, and
/// the colour it glows.
#[derive(Clone, Debug, PartialEq)]
pub struct BloomFx {
    start: Duration,
    duration: Duration,
    accent: [f32; 3],
    /// The corner radius the window lands on (its frame's, 0 when square).
    radius_to: f64,
}

/// Bloom's look at one instant, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BloomLook {
    pub progress: f64,
    pub blur: f64,
    pub radius: f64,
    pub tint: f64,
    pub glow: f64,
    pub accent: [f32; 3],
}

impl BloomFx {
    /// `None` unless Bloom draws in these settings: in reduced mode the open
    /// is opacity only, and off is nothing.
    pub fn new(settings: &AnimationSettings, now: Duration, accent: [f32; 3], radius_to: f64) -> Option<Self> {
        if !settings.motion() {
            return None;
        }
        let duration = settings.duration(spec::BLOOM)?;
        Some(Self { start: now, duration, accent, radius_to })
    }

    pub fn progress(&self, now: Duration) -> f64 {
        let d = self.duration.as_secs_f64();
        if d <= 0.0 {
            return 1.0;
        }
        (now.saturating_sub(self.start).as_secs_f64() / d).clamp(0.0, 1.0)
    }

    pub fn look(&self, now: Duration) -> BloomLook {
        look_at(self.progress(now), self.radius_to, self.accent)
    }
}

/// Bloom's look at progress `p`. Pure, so the spec's numbers are tested here.
pub fn look_at(p: f64, radius_to: f64, accent: [f32; 3]) -> BloomLook {
    let p = p.clamp(0.0, 1.0);
    let rest = 1.0 - p;
    // Cubic out: blur is gone fast, the last of it slowly (a lens settling).
    let pull = rest * rest * rest;
    let tint_rest = (1.0 - p / spec::BLOOM_TINT_FRACTION).max(0.0);
    BloomLook {
        progress: p,
        blur: spec::BLOOM_BLUR_PX * pull,
        radius: radius_to + (spec::BLOOM_RADIUS_FROM - radius_to) * pull,
        tint: spec::BLOOM_TINT * tint_rest * tint_rest,
        glow: spec::BLOOM_GLOW_ALPHA * (std::f64::consts::PI * p).sin().max(0.0),
        accent,
    }
}

// === Blur on/off, and what was drawn ===

/// Whether Bloom blurs (`EVO_BLOOM_BLUR=off` at start, `evo.SetLumen
/// {bloom_blur}` after). Without it Bloom keeps its rise, tint, corners and
/// glow: the cheaper look, for a GPU that can't afford the taps.
static BLUR: AtomicBool = AtomicBool::new(true);
static BLUR_ENV: std::sync::Once = std::sync::Once::new();

pub fn blur_enabled() -> bool {
    BLUR_ENV.call_once(|| {
        if std::env::var("EVO_BLOOM_BLUR")
            .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "0" | "none"))
        {
            BLUR.store(false, Ordering::Relaxed);
        }
    });
    BLUR.load(Ordering::Relaxed)
}

pub fn set_blur_enabled(enabled: bool) {
    blur_enabled();
    BLUR.store(enabled, Ordering::Relaxed);
}

/// Of Bloom's frames (`GetLumenState` `frames.bloom`, counted in
/// `anim::lumen`), the ones that blurred.
static BLURRED_FRAMES: AtomicU64 = AtomicU64::new(0);

pub fn blurred_frames() -> u64 {
    BLURRED_FRAMES.load(Ordering::Relaxed)
}

/// One window drawn through Bloom's shader this frame.
fn note_frame(look: &BloomLook, blurred: bool) {
    lumen::count_frame(Effect::Bloom);
    lumen::record_progress(Effect::Bloom, look.progress);
    if blurred {
        BLURRED_FRAMES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Bloom's own fields for `GetLumenState` (its program is a smithay texture
/// shader, not the Lumen program): `bloom_program`, `bloom_blur` and
/// `bloom_blurred_frames`.
pub fn state_json() -> serde_json::Value {
    serde_json::json!({
        "bloom_program": program_status(),
        "bloom_blur": blur_enabled(),
        "bloom_blurred_frames": blurred_frames(),
    })
}

/// What compiling Bloom's programs came to: 0 not tried, 1 compiled, 2 failed.
pub(super) static PROGRAM: AtomicU8 = AtomicU8::new(0);

/// `"untried"`, `"compiled"` or `"failed"`.
pub fn program_status() -> &'static str {
    match PROGRAM.load(Ordering::Relaxed) {
        1 => "compiled",
        2 => "failed",
        _ => "untried",
    }
}

/// Whether a new open should be drawn as Bloom: Lumen on, and the program not
/// known to have failed (a window opening before the first frame has compiled
/// it still blooms; if compiling then fails it is drawn as the plain
/// transform).
pub fn available() -> bool {
    blooms(super::lumen::enabled(), PROGRAM.load(Ordering::Relaxed))
}

/// `available`'s rule, on plain values: Lumen on, and the program untried
/// (0) or compiled (1), not failed (2).
fn blooms(lumen: bool, program: u8) -> bool {
    lumen && program != 2
}


// === Drawing ===

/// The window's elements with each textured one drawn through Bloom's shader.
/// `frame` is the window's whole visible frame (title bar included) in output
/// physical pixels, unposed: the same space as the elements, before the pose's
/// `Rescale`/`Relocate` wrap. Returns `elements` unchanged when there is no
/// program, which is the plain-transform fallback.
pub fn apply<R>(
    renderer: &mut R,
    elements: Vec<WindowRenderElement<R>>,
    look: &BloomLook,
    frame: Rectangle<i32, Physical>,
    scale: Scale<f64>,
) -> Vec<WindowRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem + GlesAccess,
    R::TextureId: Clone + Texture + Send + 'static,
{
    let Some(programs) = shader::bloom_programs(renderer) else {
        return elements;
    };
    let px = (scale.x + scale.y) / 2.0;
    let blur = if blur_enabled() && look.blur * px > BLUR_MIN_PX { look.blur * px } else { 0.0 };
    let params = BloomParams {
        radius: look.radius * px,
        blur,
        tint: look.tint as f32,
        accent: look.accent,
    };
    note_frame(look, blur > 0.0);
    // The border's straight edges are solid colour, which no texture shader
    // reaches: drawn while the corners are still wider than the border's arcs
    // they would stick out past the rounding, so they fade in as the radius
    // lands.
    let border = border_alpha(look);
    elements
        .into_iter()
        .filter_map(|element| {
            let inner = match element {
                WindowRenderElement::Window(e) => BloomInner::Surface(e),
                WindowRenderElement::Clipped(c) => BloomInner::Surface(c.into_inner()),
                WindowRenderElement::Decoration(DecorationRenderElement::Memory(m)) => BloomInner::Memory(m),
                WindowRenderElement::Decoration(DecorationRenderElement::Solid(s)) => {
                    if border <= 0.0 {
                        return None;
                    }
                    let solid = smithay::backend::renderer::element::solid::SolidColorRenderElement::new(
                        s.id().clone(),
                        s.geometry(scale),
                        s.current_commit(),
                        s.color() * border,
                        s.kind(),
                    );
                    return Some(WindowRenderElement::Decoration(DecorationRenderElement::Solid(solid)));
                }
                other => return Some(other),
            };
            Some(match BloomSurface::new(inner, programs.surface.clone(), scale, frame, params) {
                Ok(bloom) => WindowRenderElement::Bloom(bloom),
                Err(BloomInner::Surface(e)) => WindowRenderElement::Window(e),
                Err(BloomInner::Memory(m)) => WindowRenderElement::Decoration(DecorationRenderElement::Memory(m)),
            })
        })
        .collect()
}

/// How visible the border's solid edges are at `look`: hidden until the
/// corners are within a pixel or two of the frame's own, then faded in.
pub fn border_alpha(look: &BloomLook) -> f32 {
    // The share of the radius change still to go (the corners' cubic out):
    // the edges show from 8% left (about 2 px over the arcs) and are whole by
    // 2% (half a pixel).
    let left = (1.0 - look.progress).powi(3);
    ((0.08 - left) / 0.06).clamp(0.0, 1.0) as f32
}

/// The glow behind the window, centred on its posed frame (output physical
/// pixels), or `None` when there is nothing to draw.
pub fn glow<R: GlesAccess>(renderer: &mut R, look: &BloomLook, posed: Rectangle<i32, Physical>, alpha: f32) -> Option<GlowElement> {
    if look.glow <= 0.0 || posed.size.w <= 0 || posed.size.h <= 0 {
        return None;
    }
    let programs = shader::bloom_programs(renderer)?;
    let reach = (posed.size.w.max(posed.size.h) as f64 * spec::BLOOM_GLOW_REACH).round() as i32;
    let cx = posed.loc.x + posed.size.w / 2;
    let cy = posed.loc.y + posed.size.h / 2;
    let area = Rectangle::new(Point::from((cx - reach, cy - reach)), Size::from((2 * reach, 2 * reach)));
    // A new id each frame: the glow changes every frame it is drawn, and the
    // damage tracker then repaints its area, and the area it left, as a matter
    // of course.
    Some(GlowElement::new(programs.glow, Id::new(), area, look.accent, look.glow as f32, alpha))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::settings::Mode;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn starts_soft_wide_tinted_and_dark_and_lands_plain() {
        let start = look_at(0.0, 16.0, NEUTRAL_ACCENT);
        assert_eq!(start.blur, spec::BLOOM_BLUR_PX);
        assert_eq!(start.radius, spec::BLOOM_RADIUS_FROM);
        assert_eq!(start.tint, spec::BLOOM_TINT);
        assert_eq!(start.glow, 0.0);
        let end = look_at(1.0, 16.0, NEUTRAL_ACCENT);
        assert_eq!((end.blur, end.radius, end.tint), (0.0, 16.0, 0.0));
        assert!(end.glow.abs() < 1e-9);
    }

    #[test]
    fn blur_is_a_cubic_out_focus_pull() {
        // Most of the blur goes early: under half is left at a fifth of the way.
        assert!(look_at(0.2, 16.0, NEUTRAL_ACCENT).blur < spec::BLOOM_BLUR_PX * 0.52);
        let mut last = f64::INFINITY;
        for i in 0..=20 {
            let b = look_at(i as f64 / 20.0, 16.0, NEUTRAL_ACCENT).blur;
            assert!(b <= last, "blur never comes back");
            last = b;
        }
        assert!(look_at(0.75, 16.0, NEUTRAL_ACCENT).blur < BLUR_MIN_PX, "sharp by three quarters");
    }

    #[test]
    fn tint_is_gone_by_half_way_and_glow_peaks_there() {
        assert_eq!(look_at(0.5, 16.0, NEUTRAL_ACCENT).tint, 0.0);
        assert!(look_at(0.25, 16.0, NEUTRAL_ACCENT).tint > 0.0);
        let peak = look_at(0.5, 16.0, NEUTRAL_ACCENT).glow;
        assert!((peak - spec::BLOOM_GLOW_ALPHA).abs() < 1e-9);
        for p in [0.1, 0.3, 0.7, 0.9] {
            assert!(look_at(p, 16.0, NEUTRAL_ACCENT).glow < peak);
        }
    }

    #[test]
    fn corners_land_on_the_frames_radius_even_square() {
        assert_eq!(look_at(1.0, 0.0, NEUTRAL_ACCENT).radius, 0.0);
        let mid = look_at(0.3, 0.0, NEUTRAL_ACCENT).radius;
        assert!(mid > 0.0 && mid < spec::BLOOM_RADIUS_FROM);
    }

    #[test]
    fn border_fades_in_only_once_the_corners_have_landed() {
        assert_eq!(border_alpha(&look_at(0.0, 16.0, NEUTRAL_ACCENT)), 0.0);
        assert_eq!(border_alpha(&look_at(0.3, 16.0, NEUTRAL_ACCENT)), 0.0);
        assert_eq!(border_alpha(&look_at(1.0, 16.0, NEUTRAL_ACCENT)), 1.0);
        let late = border_alpha(&look_at(0.8, 16.0, NEUTRAL_ACCENT));
        assert!(late > 0.0 && late <= 1.0);
    }

    #[test]
    fn progress_follows_the_scaled_clock() {
        let slow = AnimationSettings { mode: Mode::Full, scale: 10.0 };
        let fx = BloomFx::new(&slow, ms(1000), NEUTRAL_ACCENT, 16.0).unwrap();
        assert_eq!(fx.progress(ms(1000)), 0.0);
        assert!((fx.progress(ms(1000) + spec::BLOOM * 5) - 0.5).abs() < 1e-9);
        assert_eq!(fx.progress(ms(1000) + spec::BLOOM * 20), 1.0);
    }

    #[test]
    fn lumen_off_or_a_failed_shader_falls_back_to_the_plain_open() {
        assert!(blooms(true, 0), "a window opening before the first frame still blooms");
        assert!(blooms(true, 1));
        assert!(!blooms(true, 2), "failed to compile: the plain open");
        assert!(!blooms(false, 1), "Lumen off: the plain open");
    }

    #[test]
    fn reduced_and_off_have_no_bloom() {
        let reduced = AnimationSettings { mode: Mode::Reduced, scale: 1.0 };
        let off = AnimationSettings { mode: Mode::Off, scale: 1.0 };
        let zero = AnimationSettings { mode: Mode::Full, scale: 0.0 };
        for s in [reduced, off, zero] {
            assert!(BloomFx::new(&s, ms(0), NEUTRAL_ACCENT, 16.0).is_none());
        }
        assert!(BloomFx::new(&AnimationSettings::default(), ms(0), NEUTRAL_ACCENT, 16.0).is_some());
    }
}
