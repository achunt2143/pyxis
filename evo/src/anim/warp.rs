//! Droplet and Rise as something Evo can draw: the window's picture (a
//! [`lumen::Source`]), the icon it is heading for, and one frame of the Lumen
//! program per draw, posed by [`droplet::pose`].
//!
//! The genie funnel this module used to draw is retired (Lumen replaced it,
//! the user's call 2026-11-08); its GL plumbing lives on in [`super::lumen`].
//!
//! # Why a snapshot
//!
//! A live surface can't be sampled by a program of ours: on udev its texture
//! is a `MultiTexture`. So the window is read back once when the minimize or
//! restore begins (`Backend::snapshot_window`) and drawn from that; the
//! picture is frozen for the animation's ~half second. It lives in the
//! animation (`WindowAnim::warp`), so it follows the animation from a minimize
//! ghost into the restore that interrupts it, and the Rise turns round on the
//! same pixels.
//!
//! # Fallback
//!
//! `draw` answers `None` when Lumen can't draw (switched off, no GLES, the
//! program failed, the upload failed); the caller then draws lane B's
//! genie-lite transform of the live window, driven by the very same spring.

use std::{cell::Cell, time::Duration};

use smithay::utils::{Logical, Physical, Point, Rectangle};

use super::{
    droplet::{self, Rect},
    lumen::{self, Effect, Source},
    shader::GlesAccess,
    snapshot::Snapshot,
    transform::WindowTransform,
};
use crate::shell::identity::Radii;

/// The element a Droplet frame is: the Lumen program's.
pub type WarpElement = lumen::LumenElement;

/// What a minimize or restore carries to be drawn as a Droplet or Rise.
pub struct Warp {
    source: Source,
    /// The icon, output-logical.
    pub target: Rectangle<i32, Logical>,
    accent: [f32; 3],
    /// The window's corners when it left, logical: where the shape starts.
    corners: Radii,
    /// The last progress drawn and when, and the progress rate measured from
    /// them (per second): the speed the stretch follows.
    last: Cell<Option<(Duration, f64)>>,
    rate: Cell<f64>,
}

impl std::fmt::Debug for Warp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Warp").field("source", &self.source).field("target", &self.target).finish()
    }
}

/// Where a frame is drawn.
pub struct Placement {
    /// The window's frame, output-logical (what the spring's plan is against).
    pub frame: Rectangle<i32, Logical>,
    /// The window's render origin, output-physical (the snapshot's pixels sit
    /// at `render_origin + snapshot.origin`).
    pub render_origin: Point<i32, Physical>,
    pub output_origin: Point<i32, Logical>,
    pub scale: f64,
}

/// One frame, and how far along it is.
pub struct Drawn {
    pub element: WarpElement,
    /// 0 at the frame, 1 at the icon.
    pub progress: f64,
}

/// Rate samples closer together than this are the same frame drawn for
/// another output (or a repaint): they don't move the measured speed.
const MIN_SAMPLE: Duration = Duration::from_millis(4);

/// Fold a new rate sample into the running one: light smoothing, since the
/// spring is smooth and frame times jitter.
pub fn smooth_rate(old: f64, sample: f64) -> f64 {
    0.4 * old + 0.6 * sample
}

impl Warp {
    /// `accent` is the app's own (Bloom's, from `SetMinimizeTargets`);
    /// without one the picture's average colour is made into one. `corners`
    /// are the frame's (`WindowElement::picture_radii`).
    pub fn new(snapshot: Snapshot, target: Rectangle<i32, Logical>, accent: Option<[f32; 3]>, corners: Radii) -> Self {
        let source = Source::new(snapshot);
        let accent = accent.unwrap_or_else(|| source.average_accent());
        Self { source, target, accent, corners, last: Cell::new(None), rate: Cell::new(0.0) }
    }

    pub fn snapshot(&self) -> &Snapshot {
        self.source.snapshot()
    }

    pub fn accent(&self) -> [f32; 3] {
        self.accent
    }

    /// Measure the progress rate at `now`.
    fn measure(&self, now: Duration, p: f64) -> f64 {
        match self.last.get() {
            Some((t, p0)) if now >= t + MIN_SAMPLE => {
                let sample = (p - p0) / (now - t).as_secs_f64();
                let rate = smooth_rate(self.rate.get(), sample);
                self.rate.set(rate);
                self.last.set(Some((now, p)));
                rate
            }
            Some(_) => self.rate.get(),
            None => {
                self.last.set(Some((now, p)));
                0.0
            }
        }
    }

    /// One Droplet/Rise frame for the pose `t` (lane B's spring, sampled
    /// now), or `None` to fall back to the genie-lite draw of the live window.
    pub fn draw<R: GlesAccess>(&self, renderer: &mut R, place: &Placement, t: &WindowTransform) -> Option<Drawn> {
        if !lumen::enabled() {
            return None;
        }
        let progress = droplet::progress_for(t.scale.y, place.frame, self.target);
        let rate = self.measure(super::now(), progress);
        let s = place.scale;
        let to_px = |r: Rectangle<i32, Logical>| {
            Rect::new(
                (r.loc.x - place.output_origin.x) as f64 * s,
                (r.loc.y - place.output_origin.y) as f64 * s,
                r.size.w as f64 * s,
                r.size.h as f64 * s,
            )
        };
        let frame = to_px(place.frame);
        let pose = droplet::pose(frame, to_px(self.target), progress, rate, s, self.corners);
        let snap = self.source.snapshot();
        let top_left = (place.render_origin + snap.origin).to_f64();
        let params = lumen::Params {
            center: pose.center,
            size: pose.size,
            radii: pose.radii,
            blur: pose.blur,
            tint: pose.tint,
            glow: pose.glow,
            glow_radius: pose.glow_radius,
            accent: self.accent,
            alpha: pose.alpha as f32,
            stretch: pose.stretch,
            angle: pose.angle,
            // Only the frame: a client's shadow margin stays out of the orb.
            uv: self.source.uv_of(top_left, [frame.x, frame.y, frame.w, frame.h]),
        };
        let element = self.source.element(renderer, &params, Effect::Droplet)?;
        lumen::record_progress(Effect::Droplet, progress);
        Some(Drawn { element, progress })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_is_measured_across_frames_not_outputs() {
        let w = Warp::new(
            Snapshot::from_pixels(vec![255; 16], (2, 2).into(), (0, 0).into(), 1.0),
            Rectangle::new((0, 0).into(), (48, 48).into()),
            None,
            Radii::FRAME,
        );
        let ms = Duration::from_millis;
        assert_eq!(w.measure(ms(1000), 0.0), 0.0, "first sample: at rest");
        let r = w.measure(ms(1016), 0.032);
        assert!((r - 1.2).abs() < 1e-9, "2/s smoothed: {r}");
        // The same frame on a second output: the rate holds.
        assert_eq!(w.measure(ms(1017), 0.034), r);
        // Turning round (a Rise interrupting) brings the rate through zero, no jump.
        let r2 = w.measure(ms(1032), 0.032);
        assert!(r2 < r);
    }

    #[test]
    fn an_apps_accent_wins_over_the_picture() {
        let snap = || Snapshot::from_pixels(vec![255; 16], (2, 2).into(), (0, 0).into(), 1.0);
        let target = Rectangle::new((0, 0).into(), (48, 48).into());
        assert_eq!(Warp::new(snap(), target, Some([1.0, 0.0, 0.0]), Radii::FRAME).accent(), [1.0, 0.0, 0.0]);
        let own = Warp::new(snap(), target, None, Radii::FRAME).accent();
        assert_ne!(own, [1.0, 1.0, 1.0], "a white picture is not a white orb");
    }
}
