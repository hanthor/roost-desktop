//! The shell's live copy of GNOME's motion policy
//! ([`tuna_shell_control::MotionPolicy`]), shared with the compositor
//! through `InputSettings` and read by the shell's own animations.
use std::cell::Cell;

use gio::prelude::*;
use gtk4 as gtk;
use tuna_shell_control::{motion::SLOWDOWN_ENV, MotionPolicy};

thread_local! {
    static CURRENT: Cell<MotionPolicy> = Cell::new(MotionPolicy::default());
}

/// The policy most recently derived from GNOME's settings.
pub fn current() -> MotionPolicy {
    CURRENT.with(Cell::get)
}

/// Install a freshly derived policy; GTK's own transitions follow it.
/// GTK has no fade-only mode, so its transitions run only at full motion
/// (they slide as often as they fade).
pub fn set(policy: MotionPolicy) {
    CURRENT.with(|c| c.set(policy));
    if let Some(gtk_settings) = gtk::Settings::default() {
        if gtk_settings.is_gtk_enable_animations() != policy.allows_motion() {
            gtk_settings.set_gtk_enable_animations(policy.allows_motion());
        }
    }
}

/// Pure derivation from the raw keys; `None` is a key the installed
/// schemas lack (older GNOME), which keeps GNOME's default.
pub fn derive(
    enable_animations: Option<bool>,
    reduced_motion: Option<&str>,
    slowdown_env: Option<&str>,
) -> MotionPolicy {
    MotionPolicy::from_gnome(
        enable_animations.unwrap_or(true),
        reduced_motion == Some("reduce"),
        MotionPolicy::parse_slowdown(slowdown_env),
    )
}

/// Read the policy from GSettings and GNOME Shell's slow-down variable.
/// GNOME 51's Reduced Motion is an enum independent of enable-animations;
/// keys missing from older schema sets are never read.
pub fn from_settings(
    interface: Option<&gio::Settings>,
    a11y: Option<&gio::Settings>,
) -> MotionPolicy {
    let has = |s: &&gio::Settings, key: &str| {
        s.settings_schema()
            .is_some_and(|schema| schema.has_key(key))
    };
    let enable = interface
        .filter(|s| has(s, "enable-animations"))
        .map(|s| s.boolean("enable-animations"));
    let reduced = a11y
        .filter(|s| has(s, "reduced-motion"))
        .map(|s| s.string("reduced-motion"));
    let slowdown = std::env::var(SLOWDOWN_ENV).ok();
    derive(enable, reduced.as_deref(), slowdown.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::MotionLevel;

    #[test]
    fn gnome_keys_combine_into_three_levels() {
        let level = |enable, reduced| derive(enable, reduced, None).level;
        assert_eq!(level(Some(true), Some("no-preference")), MotionLevel::Full);
        assert_eq!(level(Some(true), Some("reduce")), MotionLevel::FadeOnly);
        assert_eq!(level(Some(false), Some("no-preference")), MotionLevel::Off);
        assert_eq!(level(Some(false), Some("reduce")), MotionLevel::Off);
        // Older schema sets: missing keys keep GNOME's defaults.
        assert_eq!(level(None, None), MotionLevel::Full);
        assert_eq!(level(None, Some("reduce")), MotionLevel::FadeOnly);
        assert_eq!(level(Some(false), None), MotionLevel::Off);
    }

    #[test]
    fn slowdown_variable_scales_shell_durations() {
        let policy = derive(Some(true), Some("reduce"), Some("2"));
        assert_eq!(policy.adjust_ms(150.0), 300.0);
        assert_eq!(derive(Some(true), None, Some("bogus")).slowdown(), 1.0);
        assert_eq!(derive(Some(false), None, Some("2")).adjust_ms(150.0), 0.0);
    }

    #[test]
    fn the_current_policy_updates_live() {
        assert_eq!(current(), MotionPolicy::default());
        let reduced = derive(Some(true), Some("reduce"), Some("3"));
        CURRENT.with(|c| c.set(reduced));
        assert_eq!(current(), reduced);
        assert!(!current().allows_motion() && current().allows_fades());
        CURRENT.with(|c| c.set(MotionPolicy::default()));
        assert!(current().allows_motion());
    }
}
