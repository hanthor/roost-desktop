//! GNOME's motion policy, shared by the compositor and the shell.
//!
//! GNOME 51 derives it from three inputs (St settings): the
//! `org.gnome.desktop.interface enable-animations` switch, the
//! `org.gnome.desktop.a11y.interface reduced-motion` enum, and a
//! slow-down factor that GNOME Shell only takes from the
//! `GNOME_SHELL_SLOWDOWN_FACTOR` environment variable (or Looking
//! Glass). Durations go through [`MotionPolicy::adjust_ms`], GNOME's
//! `adjustAnimationTime`. Under reduced motion GNOME keeps opacity
//! changes and drops translation and scale, so an animation asks
//! [`MotionPolicy::allows_motion`] before moving or scaling anything.

use serde::{Deserialize, Serialize};

/// The environment variable GNOME Shell reads its slow-down factor from.
pub const SLOWDOWN_ENV: &str = "GNOME_SHELL_SLOWDOWN_FACTOR";

/// How much animation the session allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum MotionLevel {
    /// Every animation runs.
    #[default]
    Full,
    /// Reduced motion: fades run, translation and scale finish instantly.
    FadeOnly,
    /// Animations are off: everything finishes instantly.
    Off,
}

impl MotionLevel {
    /// Stable name for logs and the compositor state file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::FadeOnly => "fade-only",
            Self::Off => "off",
        }
    }
}

/// The effective motion policy: a [`MotionLevel`] plus GNOME's slow-down
/// factor, kept in thousandths so the policy stays `Eq` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MotionPolicy {
    pub level: MotionLevel,
    /// Slow-down factor in thousandths (1000 = GNOME's default 1.0).
    pub slowdown_milli: u32,
}

impl Default for MotionPolicy {
    /// GNOME 51's defaults: animations on, no reduced motion, factor 1.
    fn default() -> Self {
        Self::new(MotionLevel::Full, 1.0)
    }
}

impl MotionPolicy {
    /// A policy with `slowdown` clamped to a positive thousandth.
    pub fn new(level: MotionLevel, slowdown: f64) -> Self {
        let milli = if slowdown.is_finite() && slowdown > 0.0 {
            (slowdown * 1000.0).round().clamp(1.0, f64::from(u32::MAX)) as u32
        } else {
            1000
        };
        Self {
            level,
            slowdown_milli: milli,
        }
    }

    /// Derive the policy from GNOME's settings. `enable-animations` off
    /// wins; otherwise `reduced-motion = reduce` keeps fades only.
    pub fn from_gnome(enable_animations: bool, reduce_motion: bool, slowdown: f64) -> Self {
        let level = match (enable_animations, reduce_motion) {
            (false, _) => MotionLevel::Off,
            (true, true) => MotionLevel::FadeOnly,
            (true, false) => MotionLevel::Full,
        };
        Self::new(level, slowdown)
    }

    /// Parse `GNOME_SHELL_SLOWDOWN_FACTOR` as GNOME Shell does: a positive
    /// number is used, anything else keeps the factor at 1.
    pub fn parse_slowdown(value: Option<&str>) -> f64 {
        value
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|f| f.is_finite() && *f > 0.0)
            .unwrap_or(1.0)
    }

    /// The slow-down factor (1.0 = normal speed).
    pub fn slowdown(&self) -> f64 {
        f64::from(self.slowdown_milli) / 1000.0
    }

    /// GNOME's `enable-animations` as St reports it: anything but off.
    /// This is what `org.gnome.Shell.Introspect AnimationsEnabled` shows.
    pub fn animations_enabled(&self) -> bool {
        self.level != MotionLevel::Off
    }

    /// Whether opacity changes animate (full or fade-only).
    pub fn allows_fades(&self) -> bool {
        self.animations_enabled()
    }

    /// Whether translation and scale animate (full only).
    pub fn allows_motion(&self) -> bool {
        self.level == MotionLevel::Full
    }

    /// GNOME's `adjustAnimationTime`: 0 when animations are off,
    /// otherwise `ms` times the slow-down factor. Motion animations that
    /// fade-only drops should also check [`Self::allows_motion`].
    pub fn adjust_ms(&self, ms: f64) -> f64 {
        if self.animations_enabled() {
            ms * self.slowdown()
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_combination_maps_to_gnome_levels() {
        for enable in [false, true] {
            for reduce in [false, true] {
                let policy = MotionPolicy::from_gnome(enable, reduce, 1.0);
                let expected = match (enable, reduce) {
                    (false, _) => MotionLevel::Off,
                    (true, true) => MotionLevel::FadeOnly,
                    (true, false) => MotionLevel::Full,
                };
                assert_eq!(policy.level, expected, "enable={enable} reduce={reduce}");
                assert_eq!(policy.animations_enabled(), enable);
                assert_eq!(policy.allows_fades(), enable);
                assert_eq!(policy.allows_motion(), enable && !reduce);
            }
        }
        assert_eq!(
            MotionPolicy::default(),
            MotionPolicy::from_gnome(true, false, 1.0)
        );
    }

    #[test]
    fn durations_follow_adjust_animation_time() {
        let full = MotionPolicy::from_gnome(true, false, 1.0);
        assert_eq!(full.adjust_ms(250.0), 250.0);
        let slow = MotionPolicy::from_gnome(true, false, 4.0);
        assert_eq!(slow.adjust_ms(250.0), 1000.0);
        // Reduced motion keeps fades at their scaled duration.
        let fade = MotionPolicy::from_gnome(true, true, 2.0);
        assert_eq!(fade.adjust_ms(10_000.0), 20_000.0);
        // Off is instant whatever the factor.
        assert_eq!(
            MotionPolicy::from_gnome(false, false, 4.0).adjust_ms(250.0),
            0.0
        );
        assert_eq!(
            MotionPolicy::from_gnome(false, true, 1.0).adjust_ms(250.0),
            0.0
        );
        let fast = MotionPolicy::from_gnome(true, false, 0.5);
        assert_eq!(fast.adjust_ms(200.0), 100.0);
    }

    #[test]
    fn slowdown_parses_like_gnome_shell() {
        assert_eq!(MotionPolicy::parse_slowdown(None), 1.0);
        assert_eq!(MotionPolicy::parse_slowdown(Some("3")), 3.0);
        assert_eq!(MotionPolicy::parse_slowdown(Some(" 0.25 ")), 0.25);
        for bad in ["", "0", "-2", "slow", "NaN", "inf"] {
            assert_eq!(MotionPolicy::parse_slowdown(Some(bad)), 1.0, "{bad}");
        }
        // Tiny factors stay positive; invalid ones fall back to 1.
        assert_eq!(MotionPolicy::new(MotionLevel::Full, 1e-9).slowdown_milli, 1);
        assert_eq!(MotionPolicy::new(MotionLevel::Full, -1.0).slowdown(), 1.0);
        assert_eq!(MotionPolicy::new(MotionLevel::Full, 2.5).slowdown(), 2.5);
    }

    #[test]
    fn level_names_are_stable() {
        assert_eq!(MotionLevel::Full.as_str(), "full");
        assert_eq!(MotionLevel::FadeOnly.as_str(), "fade-only");
        assert_eq!(MotionLevel::Off.as_str(), "off");
    }
}
