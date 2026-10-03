//! Critically damped spring for view motion (scrollable-tiling spec).
//!
//! niri animates horizontal view movement with a spring; its default
//! `view-movement` animation is `spring damping-ratio=1.0
//! stiffness=800 epsilon=0.0001`. With a damping ratio of exactly 1.0
//! (unit mass) the spring is critically damped and has the closed form
//!
//! ```text
//! x(t) = to + (x0 + (v0 + w * x0) * t) * e^(-w t),  x0 = from - to,
//! w = sqrt(stiffness)
//! ```
//!
//! which is niri's own critically damped branch (its `beta` equals
//! `omega0` there). Positions are in whatever unit the caller uses
//! (logical pixels for the strip view); time is in seconds, so the
//! spring can be stepped and tested without a wall clock.

/// niri's default `view-movement` stiffness.
pub const VIEW_STIFFNESS: f64 = 800.0;
/// niri's default `view-movement` epsilon, relative to the distance
/// travelled.
pub const VIEW_EPSILON: f64 = 0.0001;
/// Hard stop: a spring that has not settled by now snaps (guards
/// against a non-finite input ever keeping frames alive).
const MAX_SECONDS: f64 = 2.0;

/// One critically damped spring from `from` toward `to`, starting with
/// `velocity` (units per second).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    /// Value at `t = 0`.
    pub from: f64,
    /// Value it settles at.
    pub to: f64,
    /// Velocity at `t = 0`, in units per second.
    pub velocity: f64,
    /// Natural frequency `sqrt(stiffness / mass)`, mass 1.
    omega: f64,
    /// Settle threshold relative to the motion's scale.
    epsilon: f64,
}

impl Spring {
    /// A spring with niri's default `view-movement` parameters.
    pub fn view_movement(from: f64, to: f64, velocity: f64) -> Self {
        Self::new(from, to, velocity, VIEW_STIFFNESS, VIEW_EPSILON)
    }

    /// A critically damped (damping ratio 1.0) spring of `stiffness`.
    pub fn new(from: f64, to: f64, velocity: f64, stiffness: f64, epsilon: f64) -> Self {
        Self {
            from,
            to,
            velocity,
            omega: stiffness.max(f64::EPSILON).sqrt(),
            epsilon,
        }
    }

    /// Value at `t` seconds.
    pub fn value_at(&self, t: f64) -> f64 {
        let x0 = self.from - self.to;
        let w = self.omega;
        self.to + (x0 + (self.velocity + w * x0) * t) * (-w * t).exp()
    }

    /// Velocity (units per second) at `t` seconds: the derivative of
    /// [`value_at`](Self::value_at), carried into a retargeted spring
    /// so motion stays smooth when the target moves mid-flight.
    pub fn velocity_at(&self, t: f64) -> f64 {
        let x0 = self.from - self.to;
        let w = self.omega;
        (self.velocity - w * (self.velocity + w * x0) * t) * (-w * t).exp()
    }

    /// The scale the epsilon is relative to: the distance to travel,
    /// or the distance the initial velocity would carry it, at least
    /// one unit (one logical pixel for the view).
    fn scale(&self) -> f64 {
        (self.from - self.to)
            .abs()
            .max(self.velocity.abs() / self.omega)
            .max(1.0)
    }

    /// Whether the spring has settled at `t`: both its displacement
    /// and its velocity are within epsilon of rest.
    pub fn done_at(&self, t: f64) -> bool {
        let finite = t.is_finite() && self.from.is_finite() && self.to.is_finite();
        if !finite || t >= MAX_SECONDS {
            return true;
        }
        let scale = self.scale();
        (self.value_at(t) - self.to).abs() <= self.epsilon * scale
            && self.velocity_at(t).abs() <= self.epsilon * scale * self.omega
    }

    /// First time (seconds, to the millisecond) the spring has settled.
    pub fn settle_time(&self) -> f64 {
        let mut ms = 0u32;
        while !self.done_at(f64::from(ms) / 1000.0) {
            ms += 1;
        }
        f64::from(ms) / 1000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_from_and_settles_monotonically_on_target() {
        let spring = Spring::view_movement(0.0, 632.0, 0.0);
        assert_eq!(spring.value_at(0.0), 0.0);
        let mut last = 0.0;
        for ms in 1..=1000 {
            let x = spring.value_at(f64::from(ms) / 1000.0);
            // Critically damped from rest: never overshoots, never
            // turns back.
            assert!(x >= last, "not monotone at {ms} ms: {x} < {last}");
            assert!(x <= 632.0, "overshot at {ms} ms: {x}");
            last = x;
        }
        let settle = spring.settle_time();
        assert!(spring.done_at(settle));
        assert!((spring.value_at(settle) - 632.0).abs() <= VIEW_EPSILON * 632.0);
    }

    #[test]
    fn settles_in_niri_time() {
        // niri estimates a critically damped spring's duration as the
        // time its envelope e^(-w t) falls below epsilon: -ln(eps) / w
        // = 9.21 / 28.28 = 0.326 s for the view-movement defaults. The
        // exact settle (displacement within epsilon, including the
        // (1 + w t) factor) lands a little later, still under half a
        // second, and does not depend on the distance.
        let estimate = -VIEW_EPSILON.ln() / VIEW_STIFFNESS.sqrt();
        assert!((estimate - 0.326).abs() < 0.001);
        for distance in [100.0, 632.0, 2000.0] {
            let spring = Spring::view_movement(0.0, distance, 0.0);
            let settle = spring.settle_time();
            assert!(
                (estimate..=0.45).contains(&settle),
                "{distance}: settled at {settle}"
            );
            // About a thousandth of the way short by niri's estimate.
            assert!((spring.value_at(estimate) - distance).abs() < 0.0011 * distance);
        }
    }

    #[test]
    fn velocity_is_the_derivative_and_carries_over() {
        let spring = Spring::view_movement(500.0, 0.0, -300.0);
        for ms in [0u32, 20, 80, 200] {
            let t = f64::from(ms) / 1000.0;
            let h = 1e-6;
            let numeric = (spring.value_at(t + h) - spring.value_at(t - h)) / (2.0 * h);
            assert!((numeric - spring.velocity_at(t)).abs() < 1e-3 * numeric.abs().max(1.0));
        }
        // Retargeting mid-flight keeps position and velocity continuous.
        let t = 0.05;
        let next = Spring::view_movement(spring.value_at(t), 200.0, spring.velocity_at(t));
        assert_eq!(next.value_at(0.0), spring.value_at(t));
        assert!((next.velocity_at(0.0) - spring.velocity_at(t)).abs() < 1e-9);
    }

    #[test]
    fn at_rest_is_already_done() {
        assert!(Spring::view_movement(10.0, 10.0, 0.0).done_at(0.0));
        assert_eq!(Spring::view_movement(10.0, 10.0, 0.0).settle_time(), 0.0);
    }
}
