//! Easing curves, an analytic damped spring, and `Animation<T>`.
//!
//! Everything here is pure: time is a `Duration` the caller passes in (the
//! compositor's own clock in production, a number in tests), and nothing
//! touches a renderer.

use std::time::Duration;

// ---------------------------------------------------------------------------
// Cubic bezier easing
// ---------------------------------------------------------------------------

/// A CSS-style `cubic-bezier(x1, y1, x2, y2)` easing: the curve from (0, 0)
/// to (1, 1) with those two control points. `ease(x)` is the progress
/// (y) reached when the clock (x) has run `x` of its duration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CubicBezier {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl CubicBezier {
    pub const fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Self { x1, y1, x2, y2 }
    }

    fn is_linear(&self) -> bool {
        self.x1 == self.y1 && self.x2 == self.y2
    }

    fn sample(a: f64, b: f64, t: f64) -> f64 {
        // B(t) with control values a, b between fixed ends 0 and 1.
        let u = 1.0 - t;
        3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
    }

    fn sample_slope(a: f64, b: f64, t: f64) -> f64 {
        let u = 1.0 - t;
        3.0 * u * u * a + 6.0 * u * t * (b - a) + 3.0 * t * t * (1.0 - b)
    }

    /// The curve parameter whose x is `x`: Newton's method, then bisection if
    /// the slope is too flat for Newton to be trusted.
    fn solve_t(&self, x: f64) -> f64 {
        let mut t = x;
        for _ in 0..8 {
            let err = Self::sample(self.x1, self.x2, t) - x;
            if err.abs() < 1e-7 {
                return t;
            }
            let slope = Self::sample_slope(self.x1, self.x2, t);
            if slope.abs() < 1e-6 {
                break;
            }
            t -= err / slope;
        }
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        t = x;
        for _ in 0..40 {
            let v = Self::sample(self.x1, self.x2, t);
            if (v - x).abs() < 1e-7 {
                break;
            }
            if v < x {
                lo = t;
            } else {
                hi = t;
            }
            t = (lo + hi) / 2.0;
        }
        t
    }

    /// Progress for a clock position `x` in 0..=1 (clamped).
    pub fn ease(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        if self.is_linear() {
            return x;
        }
        if x == 0.0 || x == 1.0 {
            return x;
        }
        Self::sample(self.y1, self.y2, self.solve_t(x))
    }

    /// d(progress)/d(clock position) at `x`, for the velocity of an animation
    /// driven by this curve.
    pub fn slope(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        if self.is_linear() {
            return 1.0;
        }
        let t = self.solve_t(x);
        let dx = Self::sample_slope(self.x1, self.x2, t);
        if dx.abs() < 1e-9 {
            return 0.0;
        }
        Self::sample_slope(self.y1, self.y2, t) / dx
    }
}

pub const LINEAR: CubicBezier = CubicBezier::new(0.0, 0.0, 1.0, 1.0);
/// The house entrance curve (expo-out), shared with Bloom's `motion.ts`.
pub const EXPO_OUT: CubicBezier = CubicBezier::new(0.16, 1.0, 0.3, 1.0);
/// Exits: slow start, fast finish.
pub const ACCELERATE: CubicBezier = CubicBezier::new(0.3, 0.0, 0.8, 0.15);
/// Small state changes (focus tint, grab lift).
pub const STANDARD: CubicBezier = CubicBezier::new(0.2, 0.0, 0.0, 1.0);

// ---------------------------------------------------------------------------
// Spring
// ---------------------------------------------------------------------------

/// A damped harmonic oscillator with unit mass, solved analytically. `omega`
/// is the undamped angular frequency (rad/s), `zeta` the damping ratio
/// (< 1 overshoots, 1 is critical, > 1 creeps in).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub omega: f64,
    pub zeta: f64,
}

impl Spring {
    pub const fn new(omega: f64, zeta: f64) -> Self {
        Self { omega, zeta }
    }

    /// A spring whose envelope has decayed to 0.1% after `duration`, so a
    /// token can be written as "about 320 ms, ζ = 0.9" rather than as an ω.
    pub fn settling_in(duration: Duration, zeta: f64) -> Self {
        let secs = duration.as_secs_f64().max(1e-3);
        let zeta = zeta.max(0.05);
        // The slowest decaying root sets the envelope. For ζ <= 1 that rate is
        // ζω; for ζ > 1 it is ω(ζ - √(ζ²-1)).
        let rate = if zeta <= 1.0 {
            zeta
        } else {
            zeta - (zeta * zeta - 1.0).sqrt()
        };
        Self {
            omega: (1000.0_f64).ln() / (rate * secs),
            zeta,
        }
    }

    /// Displacement from rest and velocity `t` seconds after starting at
    /// displacement `x0` with velocity `v0`. Both are linear in `(x0, v0)`,
    /// which is what lets `Animation<T>` run vectors through it per axis.
    pub fn step(&self, x0: f64, v0: f64, t: f64) -> (f64, f64) {
        let w = self.omega;
        let z = self.zeta;
        if (z - 1.0).abs() < 1e-9 {
            let e = (-w * t).exp();
            let b = v0 + w * x0;
            let x = e * (x0 + b * t);
            let v = e * (b - w * (x0 + b * t));
            (x, v)
        } else if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let e = (-z * w * t).exp();
            let (s, c) = (wd * t).sin_cos();
            let b = (v0 + z * w * x0) / wd;
            let x = e * (x0 * c + b * s);
            let v = e * (-z * w * (x0 * c + b * s) + (-x0 * wd * s + b * wd * c));
            (x, v)
        } else {
            let r = w * (z * z - 1.0).sqrt();
            let s1 = -w * z + r;
            let s2 = -w * z - r;
            let a = (v0 - s2 * x0) / (s1 - s2);
            let b = x0 - a;
            let (e1, e2) = ((s1 * t).exp(), (s2 * t).exp());
            (a * e1 + b * e2, a * s1 * e1 + b * s2 * e2)
        }
    }
}

// ---------------------------------------------------------------------------
// Animatable values
// ---------------------------------------------------------------------------

/// What an `Animation` can animate: a small vector space.
pub trait Animatable: Copy + PartialEq {
    fn zero() -> Self;
    fn add(self, other: Self) -> Self;
    fn sub(self, other: Self) -> Self;
    fn scale(self, k: f64) -> Self;
    /// Largest absolute component, to test "close enough to rest".
    fn magnitude(self) -> f64;
}

impl Animatable for f64 {
    fn zero() -> Self {
        0.0
    }
    fn add(self, o: Self) -> Self {
        self + o
    }
    fn sub(self, o: Self) -> Self {
        self - o
    }
    fn scale(self, k: f64) -> Self {
        self * k
    }
    fn magnitude(self) -> f64 {
        self.abs()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    pub const fn splat(v: f64) -> Self {
        Self { x: v, y: v }
    }
}

impl Animatable for Vec2 {
    fn zero() -> Self {
        Self::default()
    }
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y)
    }
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y)
    }
    fn scale(self, k: f64) -> Self {
        Self::new(self.x * k, self.y * k)
    }
    fn magnitude(self) -> f64 {
        self.x.abs().max(self.y.abs())
    }
}

// ---------------------------------------------------------------------------
// Animation<T>
// ---------------------------------------------------------------------------

/// How an animation spends its time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Timing {
    /// A fixed duration shaped by a bezier. A curve starts at rest, so an
    /// entering velocity is dropped, not carried.
    Curve { duration: Duration, curve: CubicBezier },
    /// A spring toward the target. Carries the entering velocity exactly, and
    /// is finished once position and velocity are both within `rest`.
    Spring { spring: Spring, rest: f64 },
}

impl Timing {
    pub fn curve(duration: Duration, curve: CubicBezier) -> Self {
        Timing::Curve { duration, curve }
    }
    pub fn spring(spring: Spring, rest: f64) -> Self {
        Timing::Spring { spring, rest }
    }
}

/// A value moving from `from` to `to`, with the clock injected on every call.
///
/// ```ignore
/// let mut a = Animation::new(0.0, 1.0, now, Timing::spring(Spring::settling_in(ms(300), 0.9), 0.001));
/// a.value(now + ms(100));
/// a.retarget(now + ms(100), 0.0); // turns around without jumping or stalling
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Animation<T: Animatable> {
    from: T,
    to: T,
    /// Velocity at `start`, in value units per second.
    velocity: T,
    start: Duration,
    timing: Timing,
    /// How long it runs in all (a spring's is found at construction).
    run: Duration,
}

/// Step used to find when a spring comes to rest.
const REST_SCAN_STEP: f64 = 1.0 / 480.0;
const REST_SCAN_LIMIT: f64 = 10.0;

impl<T: Animatable> Animation<T> {
    pub fn new(from: T, to: T, start: Duration, timing: Timing) -> Self {
        Self::with_velocity(from, to, T::zero(), start, timing)
    }

    pub fn with_velocity(from: T, to: T, velocity: T, start: Duration, timing: Timing) -> Self {
        let run = match timing {
            Timing::Curve { duration, .. } => duration,
            Timing::Spring { spring, rest } => Self::rest_time(from, to, velocity, spring, rest),
        };
        Self { from, to, velocity, start, timing, run }
    }

    /// An animation that is already at `value` and stays there.
    pub fn fixed(value: T) -> Self {
        Self {
            from: value,
            to: value,
            velocity: T::zero(),
            start: Duration::ZERO,
            timing: Timing::Curve { duration: Duration::ZERO, curve: LINEAR },
            run: Duration::ZERO,
        }
    }

    fn rest_time(from: T, to: T, velocity: T, spring: Spring, rest: f64) -> Duration {
        let x0 = from.sub(to);
        let mut t = 0.0;
        while t < REST_SCAN_LIMIT {
            // Components are independent, so the vector rests when each does;
            // checking the combined magnitudes is the conservative bound.
            let (x, v) = Self::spring_at(spring, x0, velocity, t);
            if x.magnitude() <= rest && v.magnitude() <= rest * 10.0 {
                return Duration::from_secs_f64(t);
            }
            t += REST_SCAN_STEP;
        }
        Duration::from_secs_f64(REST_SCAN_LIMIT)
    }

    /// Run the scalar spring per component. `Animatable` has no components,
    /// but the solution is linear in `(x0, v0)`, so it can be evaluated as
    /// `x(t) = x0 * a(t) + v0 * b(t)` with scalar coefficients taken from
    /// two unit solves.
    fn spring_at(spring: Spring, x0: T, v0: T, t: f64) -> (T, T) {
        let (xa, va) = spring.step(1.0, 0.0, t); // response to unit displacement
        let (xb, vb) = spring.step(0.0, 1.0, t); // response to unit velocity
        (
            x0.scale(xa).add(v0.scale(xb)),
            x0.scale(va).add(v0.scale(vb)),
        )
    }

    pub fn from(&self) -> T {
        self.from
    }
    pub fn to(&self) -> T {
        self.to
    }
    pub fn start(&self) -> Duration {
        self.start
    }
    pub fn timing(&self) -> Timing {
        self.timing
    }
    /// How long the animation runs from its start.
    pub fn duration(&self) -> Duration {
        self.run
    }

    pub fn is_finished(&self, now: Duration) -> bool {
        now.saturating_sub(self.start) >= self.run
    }

    /// The value at `now`. Before `start` it is `from`; once finished it is
    /// exactly `to`.
    pub fn value(&self, now: Duration) -> T {
        if self.is_finished(now) {
            return self.to;
        }
        let elapsed = now.saturating_sub(self.start).as_secs_f64();
        match self.timing {
            Timing::Curve { duration, curve } => {
                let u = elapsed / duration.as_secs_f64();
                self.from.add(self.to.sub(self.from).scale(curve.ease(u)))
            }
            Timing::Spring { spring, .. } => {
                let (x, _) = Self::spring_at(spring, self.from.sub(self.to), self.velocity, elapsed);
                self.to.add(x)
            }
        }
    }

    /// The velocity at `now`, in value units per second.
    pub fn velocity(&self, now: Duration) -> T {
        if self.is_finished(now) {
            return T::zero();
        }
        let elapsed = now.saturating_sub(self.start).as_secs_f64();
        match self.timing {
            Timing::Curve { duration, curve } => {
                let secs = duration.as_secs_f64();
                self.to
                    .sub(self.from)
                    .scale(curve.slope(elapsed / secs) / secs)
            }
            Timing::Spring { spring, .. } => {
                Self::spring_at(spring, self.from.sub(self.to), self.velocity, elapsed).1
            }
        }
    }

    /// Head for `to` from wherever the value is now, with the timing it had.
    /// The value is continuous across the call, and so is the velocity for a
    /// spring (a curve restarts at rest). Nothing is queued: the old target
    /// is simply dropped.
    pub fn retarget(&mut self, now: Duration, to: T) {
        self.retarget_with(now, to, self.timing);
    }

    /// `retarget`, switching to a different timing.
    pub fn retarget_with(&mut self, now: Duration, to: T, timing: Timing) {
        let from = self.value(now);
        let velocity = self.velocity(now);
        *self = Self::with_velocity(from, to, velocity, now, timing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn bezier_hits_its_endpoints_and_is_monotonic() {
        for curve in [LINEAR, EXPO_OUT, ACCELERATE, STANDARD] {
            assert_eq!(curve.ease(0.0), 0.0);
            assert_eq!(curve.ease(1.0), 1.0);
            let mut last = 0.0;
            for i in 1..=100 {
                let y = curve.ease(i as f64 / 100.0);
                assert!(y >= last - 1e-9, "{curve:?} not monotonic at {i}");
                last = y;
            }
        }
    }

    #[test]
    fn linear_is_identity() {
        assert!(close(LINEAR.ease(0.37), 0.37, 1e-12));
        assert!(close(LINEAR.slope(0.37), 1.0, 1e-12));
    }

    #[test]
    fn expo_out_front_loads_motion_and_accelerate_back_loads_it() {
        assert!(EXPO_OUT.ease(0.25) > 0.6, "expo-out should be mostly done by 25%");
        assert!(ACCELERATE.ease(0.5) < 0.3, "accelerate should be less than a third done at 50%: {}", ACCELERATE.ease(0.5));
        assert!(ACCELERATE.ease(0.9) > ACCELERATE.ease(0.5) * 2.0);
    }

    #[test]
    fn bezier_matches_a_known_css_value() {
        // cubic-bezier(0.25, 0.1, 0.25, 1) ("ease") at x = 0.5 is ~0.8024.
        let ease = CubicBezier::new(0.25, 0.1, 0.25, 1.0);
        assert!(close(ease.ease(0.5), 0.8024, 1e-3), "{}", ease.ease(0.5));
    }

    #[test]
    fn bezier_slope_agrees_with_a_finite_difference() {
        let h = 1e-4;
        for x in [0.1, 0.3, 0.6, 0.9] {
            let numeric = (EXPO_OUT.ease(x + h) - EXPO_OUT.ease(x - h)) / (2.0 * h);
            assert!(close(EXPO_OUT.slope(x), numeric, 1e-2), "x={x}");
        }
    }

    #[test]
    fn spring_starts_at_its_initial_state_and_settles_at_zero() {
        for zeta in [0.5, 0.9, 1.0, 1.7] {
            let s = Spring::settling_in(ms(300), zeta);
            let (x, v) = s.step(1.0, 2.0, 0.0);
            assert!(close(x, 1.0, 1e-9) && close(v, 2.0, 1e-9), "zeta {zeta}");
            let (x, v) = s.step(1.0, 2.0, 5.0);
            assert!(x.abs() < 1e-6 && v.abs() < 1e-4, "zeta {zeta}: {x} {v}");
        }
    }

    #[test]
    fn spring_velocity_is_the_derivative_of_position() {
        let h = 1e-6;
        for zeta in [0.5, 1.0, 1.7] {
            let s = Spring::settling_in(ms(300), zeta);
            for t in [0.02, 0.1, 0.2] {
                let (_, v) = s.step(1.0, 0.5, t);
                let numeric = (s.step(1.0, 0.5, t + h).0 - s.step(1.0, 0.5, t - h).0) / (2.0 * h);
                assert!(close(v, numeric, 1e-3), "zeta {zeta} t {t}: {v} vs {numeric}");
            }
        }
    }

    #[test]
    fn underdamped_spring_overshoots_and_critical_does_not() {
        let under = Spring::settling_in(ms(300), 0.5);
        let crit = Spring::settling_in(ms(300), 1.0);
        let mut under_crossed = false;
        let mut crit_crossed = false;
        for i in 0..400 {
            let t = i as f64 / 1000.0;
            under_crossed |= under.step(1.0, 0.0, t).0 < -1e-3;
            crit_crossed |= crit.step(1.0, 0.0, t).0 < -1e-6;
        }
        assert!(under_crossed);
        assert!(!crit_crossed);
    }

    #[test]
    fn curve_animation_runs_its_duration_and_lands_exactly() {
        let a = Animation::new(0.0, 10.0, ms(100), Timing::curve(ms(200), LINEAR));
        assert_eq!(a.value(ms(0)), 0.0, "before start it holds `from`");
        assert!(close(a.value(ms(200)), 5.0, 1e-9));
        assert!(!a.is_finished(ms(299)));
        assert!(a.is_finished(ms(300)));
        assert_eq!(a.value(ms(300)), 10.0);
        assert_eq!(a.value(ms(9999)), 10.0);
        assert_eq!(a.velocity(ms(9999)), 0.0);
    }

    #[test]
    fn spring_animation_settles_and_is_finished_there() {
        let a = Animation::new(
            0.0,
            100.0,
            ms(0),
            Timing::spring(Spring::settling_in(ms(300), 0.9), 0.25),
        );
        assert!(a.duration() > ms(100) && a.duration() < ms(600), "{:?}", a.duration());
        assert!(!a.is_finished(ms(50)));
        assert!(a.is_finished(a.duration()));
        assert_eq!(a.value(a.duration()), 100.0);
    }

    #[test]
    fn vectors_animate_per_axis() {
        let a = Animation::new(
            Vec2::new(0.0, 10.0),
            Vec2::new(10.0, 10.0),
            ms(0),
            Timing::curve(ms(100), LINEAR),
        );
        let v = a.value(ms(50));
        assert!(close(v.x, 5.0, 1e-9) && close(v.y, 10.0, 1e-9));
    }

    #[test]
    fn retargeting_a_spring_keeps_value_and_velocity() {
        let timing = Timing::spring(Spring::settling_in(ms(300), 0.9), 0.001);
        let mut a = Animation::new(0.0, 1.0, ms(0), timing);
        let now = ms(80);
        let (value, velocity) = (a.value(now), a.velocity(now));
        assert!(velocity > 0.5, "it should be moving: {velocity}");
        a.retarget(now, 0.0);
        assert!(close(a.value(now), value, 1e-9), "value jumped");
        assert!(close(a.velocity(now), velocity, 1e-9), "velocity was lost");
        // Momentum carries it past where it was before turning round.
        assert!(a.value(now + ms(10)) > value - 1e-3 || a.velocity(now) > 0.0);
        let end = now + a.duration();
        assert_eq!(a.value(end), 0.0);
    }

    #[test]
    fn retargeting_midway_has_no_visible_step_at_any_sample() {
        // Sample every millisecond across a retarget: the largest step
        // between neighbours must not exceed what the motion itself does.
        let timing = Timing::spring(Spring::settling_in(ms(320), 0.9), 0.001);
        let mut a = Animation::new(0.0, 1.0, ms(0), timing);
        let mut last = a.value(ms(0));
        let mut biggest = 0.0_f64;
        for t in 1..=200u64 {
            if t == 90 {
                a.retarget(ms(t), 0.0);
            }
            let v = a.value(ms(t));
            biggest = biggest.max((v - last).abs());
            last = v;
        }
        assert!(biggest < 0.05, "a 1 ms step of {biggest} is a jump");
    }

    #[test]
    fn retargeting_a_curve_is_continuous_in_value() {
        let mut a = Animation::new(0.0, 1.0, ms(0), Timing::curve(ms(200), EXPO_OUT));
        let v = a.value(ms(50));
        a.retarget(ms(50), 0.0);
        assert!(close(a.value(ms(50)), v, 1e-9));
        assert_eq!(a.value(ms(250)), 0.0);
    }

    #[test]
    fn retarget_after_finishing_starts_from_the_target() {
        let mut a = Animation::new(0.0, 1.0, ms(0), Timing::curve(ms(100), LINEAR));
        a.retarget(ms(500), 2.0);
        assert_eq!(a.value(ms(500)), 1.0, "starts where the finished one ended");
        assert_eq!(a.value(ms(600)), 2.0);
    }

    #[test]
    fn a_spring_retargeted_after_finishing_carries_no_velocity() {
        let timing = Timing::spring(Spring::settling_in(ms(300), 0.9), 0.001);
        let mut a = Animation::new(0.0, 1.0, ms(0), timing);
        a.retarget(ms(5000), 0.0);
        assert_eq!(a.value(ms(5000)), 1.0);
        assert!(a.velocity(ms(5000)).abs() < 1e-9);
    }
}
