//! Shell settings snapshot: shared GNOME keys, paint-safe reads.
//!
//! The shell honors the desktop settings it shares with GNOME apps
//! instead of owning a parallel settings universe. Reads go through a
//! [`SettingsBackend`] behind a plain [`ShellSettings`] snapshot that the
//! paint code takes by value: a missing bus, missing schema, or slow
//! daemon degrades to defaults exactly like an unreachable service, and
//! unit tests run against [`MapBackend`] with no D-Bus at all.

use std::collections::HashMap;

use gio::prelude::SettingsExt;

/// Clock rendering selected by the desktop `clock-format` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClockFormat {
    /// Twenty-four hour `HH:MM` (also the fallback for unknown values).
    #[default]
    TwentyFour,
    /// Twelve hour `h:MM` without suffix (the painted strip has no
    /// meridiem glyphs; wall-clock hour only).
    Twelve,
}

/// Parse a `clock-format` key value (`12h`/`24h`).
pub fn parse_clock_format(value: &str) -> ClockFormat {
    match value.trim() {
        "12h" => ClockFormat::Twelve,
        _ => ClockFormat::TwentyFour,
    }
}

/// Plain snapshot the paint code reads. Defaults render exactly as the
/// shell did before settings existed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShellSettings {
    /// Bar clock format.
    pub clock_format: ClockFormat,
    /// Desktop background URI, when a key is present. Unused until the
    /// compositor background hook lands; carried now so the read path
    /// is proven for more than one key.
    pub wallpaper_uri: Option<String>,
}

/// String-valued settings source. The production backend reads the
/// platform settings service; tests substitute [`MapBackend`].
pub trait SettingsBackend {
    /// Read `key` from `schema`, or `None` when absent/unreadable.
    /// Must never block the caller for long and never panic.
    fn string(&self, schema: &str, key: &str) -> Option<String>;
}

/// Platform settings backend over `gio::Settings`.
pub struct GioBackend;

impl GioBackend {
    /// Settings object when the schema is installed, `None` otherwise.
    /// The schema lookup happens up front so a missing schema never
    /// touches the bus.
    fn settings_for(schema: &str) -> Option<gio::Settings> {
        let source = gio::SettingsSchemaSource::default()?;
        source.lookup(schema, true)?;
        Some(gio::Settings::new(schema))
    }
}

impl SettingsBackend for GioBackend {
    fn string(&self, schema: &str, key: &str) -> Option<String> {
        let settings = Self::settings_for(schema)?;
        // Guard the key up front: `value()` on a missing key only logs
        // a critical, and the shell must stay quiet and functional.
        let known = settings.settings_schema()?.has_key(key);
        if !known {
            return None;
        }
        settings.value(key).str().map(str::to_owned)
    }
}

/// In-memory backend for tests.
#[derive(Debug, Default)]
pub struct MapBackend {
    values: HashMap<(String, String), String>,
}

impl MapBackend {
    /// Backend holding the given `(schema, key, value)` triples.
    pub fn with_values(values: &[(&str, &str, &str)]) -> Self {
        let mut backend = Self::default();
        for (schema, key, value) in values {
            backend
                .values
                .insert((schema.to_string(), key.to_string()), value.to_string());
        }
        backend
    }
}

impl SettingsBackend for MapBackend {
    fn string(&self, schema: &str, key: &str) -> Option<String> {
        self.values
            .get(&(schema.to_string(), key.to_string()))
            .cloned()
    }
}

/// Schema and keys honored from the shared desktop settings.
pub const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
/// `12h` or `24h`.
pub const CLOCK_FORMAT_KEY: &str = "clock-format";
/// Background picture URI (carried, not yet applied).
pub const BACKGROUND_SCHEMA: &str = "org.gnome.desktop.background";
/// Picture URI key.
pub const PICTURE_URI_KEY: &str = "picture-uri";

/// Refresh a snapshot from a backend. Unknown or absent values keep
/// their current (default) settings — never reset, never panic.
pub fn refresh(settings: &mut ShellSettings, backend: &dyn SettingsBackend) {
    if let Some(value) = backend.string(INTERFACE_SCHEMA, CLOCK_FORMAT_KEY) {
        settings.clock_format = parse_clock_format(&value);
    }
    if let Some(uri) = backend.string(BACKGROUND_SCHEMA, PICTURE_URI_KEY) {
        if !uri.trim().is_empty() {
            settings.wallpaper_uri = Some(uri);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_format_parses_both_values_with_safe_default() {
        assert_eq!(parse_clock_format("12h"), ClockFormat::Twelve);
        assert_eq!(parse_clock_format("24h"), ClockFormat::TwentyFour);
        assert_eq!(parse_clock_format(""), ClockFormat::TwentyFour);
        assert_eq!(parse_clock_format("bogus"), ClockFormat::TwentyFour);
        assert_eq!(parse_clock_format(" 12h "), ClockFormat::Twelve);
    }

    #[test]
    fn defaults_render_as_before() {
        let settings = ShellSettings::default();
        assert_eq!(settings.clock_format, ClockFormat::TwentyFour);
        assert_eq!(settings.wallpaper_uri, None);
    }

    #[test]
    fn refresh_applies_known_keys_and_keeps_unknown() {
        let backend = MapBackend::with_values(&[
            (INTERFACE_SCHEMA, CLOCK_FORMAT_KEY, "12h"),
            (BACKGROUND_SCHEMA, PICTURE_URI_KEY, "file:///pic.png"),
        ]);
        let mut settings = ShellSettings::default();
        refresh(&mut settings, &backend);
        assert_eq!(settings.clock_format, ClockFormat::Twelve);
        assert_eq!(settings.wallpaper_uri.as_deref(), Some("file:///pic.png"));

        let stale = MapBackend::with_values(&[(INTERFACE_SCHEMA, CLOCK_FORMAT_KEY, "bogus")]);
        refresh(&mut settings, &stale);
        assert_eq!(settings.clock_format, ClockFormat::TwentyFour);
        // Absent wallpaper key keeps the previously read URI.
        assert_eq!(settings.wallpaper_uri.as_deref(), Some("file:///pic.png"));
    }

    #[test]
    fn empty_backend_leaves_defaults() {
        let backend = MapBackend::default();
        let mut settings = ShellSettings::default();
        refresh(&mut settings, &backend);
        assert_eq!(settings, ShellSettings::default());
    }

    #[test]
    fn blank_wallpaper_uri_ignored() {
        let backend = MapBackend::with_values(&[(BACKGROUND_SCHEMA, PICTURE_URI_KEY, "   ")]);
        let mut settings = ShellSettings::default();
        refresh(&mut settings, &backend);
        assert_eq!(settings.wallpaper_uri, None);
    }
}
