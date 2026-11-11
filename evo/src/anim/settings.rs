//! Animation mode and speed: `EVO_ANIMATIONS` / `EVO_ANIMATION_SCALE` at
//! start, `evo.SetAnimationSettings` after.
//!
//! The settings are process-wide (one compositor, one user preference), so
//! like the overlay's region they are a global the Echo thread writes and the
//! compositor thread reads.

use std::{
    sync::{OnceLock, RwLock},
    time::Duration,
};

/// Reduced motion's ceiling, by accessibility convention.
pub const REDUCED_MAX: Duration = Duration::from_millis(150);

/// The largest duration scale accepted. 10 is slow motion for filmstrips;
/// the on-device suite uses 60 to catch an expo-out open before it has done
/// most of its moving (a 260 ms open is 95% done a quarter of the way in).
pub const MAX_SCALE: f64 = 100.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Every token as specified.
    Full,
    /// Opacity only: no scale or translate, at most 150 ms.
    Reduced,
    /// Everything instant.
    Off,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "full" | "on" | "1" => Some(Mode::Full),
            "reduced" => Some(Mode::Reduced),
            "off" | "none" | "0" => Some(Mode::Off),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Full => "full",
            Mode::Reduced => "reduced",
            Mode::Off => "off",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnimationSettings {
    pub mode: Mode,
    /// Multiplies every duration. 0 turns animation off, 10 is slow motion.
    pub scale: f64,
}

impl Default for AnimationSettings {
    fn default() -> Self {
        Self { mode: Mode::Full, scale: 1.0 }
    }
}

impl AnimationSettings {
    /// From the two environment values. Anything unparseable keeps the
    /// default for that field.
    pub fn parse(mode: Option<&str>, scale: Option<&str>) -> Self {
        let mut s = Self::default();
        if let Some(mode) = mode.and_then(Mode::parse) {
            s.mode = mode;
        }
        if let Some(scale) = scale.and_then(|v| v.trim().parse::<f64>().ok()) {
            s.scale = clamp_scale(scale).unwrap_or(s.scale);
        }
        s
    }

    pub fn from_env() -> Self {
        Self::parse(
            std::env::var("EVO_ANIMATIONS").ok().as_deref(),
            std::env::var("EVO_ANIMATION_SCALE").ok().as_deref(),
        )
    }

    /// Whether anything animates at all.
    pub fn enabled(&self) -> bool {
        self.mode != Mode::Off && self.scale > 0.0
    }

    /// Whether scale and translation are allowed (Full), as opposed to
    /// opacity alone (Reduced).
    pub fn motion(&self) -> bool {
        self.mode == Mode::Full && self.scale > 0.0
    }

    /// `base` as this mode and scale run it, or `None` when it should not
    /// animate. Reduced clamps to 150 ms *before* the scale, so slow motion
    /// still slows a reduced animation down.
    pub fn duration(&self, base: Duration) -> Option<Duration> {
        if !self.enabled() {
            return None;
        }
        let base = if self.mode == Mode::Reduced { base.min(REDUCED_MAX) } else { base };
        Some(base.mul_f64(self.scale))
    }
}

/// `Some(scale)` if it is a usable scale, clamped to 0..=MAX_SCALE.
pub fn clamp_scale(scale: f64) -> Option<f64> {
    scale.is_finite().then(|| scale.clamp(0.0, MAX_SCALE))
}

fn cell() -> &'static RwLock<AnimationSettings> {
    static SETTINGS: OnceLock<RwLock<AnimationSettings>> = OnceLock::new();
    SETTINGS.get_or_init(|| RwLock::new(AnimationSettings::from_env()))
}

/// The settings in force.
pub fn settings() -> AnimationSettings {
    cell().read().map(|s| *s).unwrap_or_default()
}

/// Change either field; `None` keeps it. Returns what is now in force.
/// Animations already running finish as they were started.
pub fn update(mode: Option<Mode>, scale: Option<f64>) -> AnimationSettings {
    let mut guard = match cell().write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(mode) = mode {
        guard.mode = mode;
    }
    if let Some(scale) = scale.and_then(clamp_scale) {
        guard.scale = scale;
    }
    *guard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_full_at_1x() {
        let s = AnimationSettings::parse(None, None);
        assert_eq!(s, AnimationSettings { mode: Mode::Full, scale: 1.0 });
        assert!(s.enabled() && s.motion());
    }

    #[test]
    fn modes_parse_case_insensitively() {
        assert_eq!(AnimationSettings::parse(Some("OFF"), None).mode, Mode::Off);
        assert_eq!(AnimationSettings::parse(Some(" reduced "), None).mode, Mode::Reduced);
        assert_eq!(AnimationSettings::parse(Some("full"), None).mode, Mode::Full);
    }

    #[test]
    fn garbage_keeps_the_default() {
        let s = AnimationSettings::parse(Some("wobbly"), Some("fast"));
        assert_eq!(s, AnimationSettings::default());
    }

    #[test]
    fn scale_is_parsed_and_clamped() {
        assert_eq!(AnimationSettings::parse(None, Some("10")).scale, 10.0);
        assert_eq!(AnimationSettings::parse(None, Some("0.5")).scale, 0.5);
        assert_eq!(AnimationSettings::parse(None, Some("1000")).scale, MAX_SCALE);
        assert_eq!(AnimationSettings::parse(None, Some("-3")).scale, 0.0);
        assert_eq!(AnimationSettings::parse(None, Some("nan")).scale, 1.0);
    }

    #[test]
    fn off_and_zero_scale_disable_everything() {
        let base = Duration::from_millis(260);
        assert_eq!(AnimationSettings::parse(Some("off"), None).duration(base), None);
        let zero = AnimationSettings::parse(None, Some("0"));
        assert!(!zero.enabled());
        assert_eq!(zero.duration(base), None);
    }

    #[test]
    fn durations_scale() {
        let s = AnimationSettings::parse(None, Some("10"));
        assert_eq!(s.duration(Duration::from_millis(260)), Some(Duration::from_millis(2600)));
    }

    #[test]
    fn reduced_is_capped_before_scaling_and_has_no_motion() {
        let s = AnimationSettings::parse(Some("reduced"), None);
        assert!(s.enabled() && !s.motion());
        assert_eq!(s.duration(Duration::from_millis(320)), Some(REDUCED_MAX));
        assert_eq!(s.duration(Duration::from_millis(80)), Some(Duration::from_millis(80)));
        let slow = AnimationSettings::parse(Some("reduced"), Some("10"));
        assert_eq!(slow.duration(Duration::from_millis(320)), Some(REDUCED_MAX * 10));
    }
}
