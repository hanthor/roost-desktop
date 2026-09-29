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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSettings {
    /// Bar clock format.
    pub clock_format: ClockFormat,
    /// Desktop background URI, when a key is present. Unused until the
    /// compositor background hook lands; carried now so the read path
    /// is proven for more than one key.
    pub wallpaper_uri: Option<String>,
    /// Icon theme name. Defaults to the GNOME default; unknown or
    /// absent values keep the current theme — never reset.
    pub icon_theme: String,
}

/// Default icon theme when the settings key is absent.
pub const DEFAULT_ICON_THEME: &str = "Adwaita";

impl Default for ShellSettings {
    /// Clock and wallpaper as before; icon theme falls back to the
    /// GNOME default rather than an empty name no theme matches.
    fn default() -> Self {
        Self {
            clock_format: ClockFormat::default(),
            wallpaper_uri: None,
            icon_theme: DEFAULT_ICON_THEME.to_owned(),
        }
    }
}

/// String-valued settings source. The production backend reads the
/// platform settings service; tests substitute [`MapBackend`].
pub trait SettingsBackend {
    /// Read `key` from `schema`, or `None` when absent/unreadable.
    /// Must never block the caller for long and never panic.
    fn string(&self, schema: &str, key: &str) -> Option<String>;
    /// Write `value` to `key` in `schema`: `false` when the schema,
    /// key, or bus is unavailable. The caller keeps its snapshot on
    /// `false` — never reset, never panic.
    fn set_string(&self, schema: &str, key: &str, value: &str) -> bool;
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

    fn set_string(&self, schema: &str, key: &str, value: &str) -> bool {
        let Some(settings) = Self::settings_for(schema) else {
            return false;
        };
        // Same key guard as the read: a missing key must read as a
        // refused write, never a critical log.
        let known = settings
            .settings_schema()
            .is_some_and(|schema| schema.has_key(key));
        if !known {
            return false;
        }
        settings.set_string(key, value).is_ok()
    }
}

/// In-memory backend for tests. Interior mutability keeps the
/// [`SettingsBackend`] write behind `&self`, like the production
/// backend's bus handle.
#[derive(Debug, Default)]
pub struct MapBackend {
    values: std::cell::RefCell<HashMap<(String, String), String>>,
}

impl MapBackend {
    /// Backend holding the given `(schema, key, value)` triples.
    pub fn with_values(values: &[(&str, &str, &str)]) -> Self {
        let backend = Self::default();
        for (schema, key, value) in values {
            backend
                .values
                .borrow_mut()
                .insert((schema.to_string(), key.to_string()), value.to_string());
        }
        backend
    }
}

impl SettingsBackend for MapBackend {
    fn string(&self, schema: &str, key: &str) -> Option<String> {
        self.values
            .borrow()
            .get(&(schema.to_string(), key.to_string()))
            .cloned()
    }

    fn set_string(&self, schema: &str, key: &str, value: &str) -> bool {
        self.values
            .borrow_mut()
            .insert((schema.to_string(), key.to_string()), value.to_string());
        true
    }
}

/// Schema and keys honored from the shared desktop settings.
pub const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
/// `12h` or `24h`.
pub const CLOCK_FORMAT_KEY: &str = "clock-format";
/// Background picture URI (published to the compositor drop file).
pub const BACKGROUND_SCHEMA: &str = "org.gnome.desktop.background";
/// Picture URI key.
pub const PICTURE_URI_KEY: &str = "picture-uri";
/// Icon theme name key.
pub const ICON_THEME_KEY: &str = "icon-theme";

/// Key value for a clock format (`12h`/`24h`): the inverse of
/// [`parse_clock_format`].
pub fn clock_format_value(format: ClockFormat) -> &'static str {
    match format {
        ClockFormat::Twelve => "12h",
        ClockFormat::TwentyFour => "24h",
    }
}

/// Write the clock format against the shared desktop schema.
/// `false` keeps the caller's snapshot — never reset, never panic.
pub fn write_clock_format(backend: &dyn SettingsBackend, format: ClockFormat) -> bool {
    backend.set_string(
        INTERFACE_SCHEMA,
        CLOCK_FORMAT_KEY,
        clock_format_value(format),
    )
}

/// Write the wallpaper URI against the shared desktop schema.
/// Blank URIs are refused (`false`) so the caller keeps its last
/// good snapshot — never reset, never panic.
pub fn write_wallpaper_uri(backend: &dyn SettingsBackend, uri: &str) -> bool {
    let uri = uri.trim();
    if uri.is_empty() {
        return false;
    }
    backend.set_string(BACKGROUND_SCHEMA, PICTURE_URI_KEY, uri)
}

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
    if let Some(theme) = backend.string(INTERFACE_SCHEMA, ICON_THEME_KEY) {
        if !theme.trim().is_empty() {
            settings.icon_theme = theme;
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

    #[test]
    fn default_icon_theme_is_gnome_default() {
        assert_eq!(ShellSettings::default().icon_theme, DEFAULT_ICON_THEME);
        assert_eq!(DEFAULT_ICON_THEME, "Adwaita");
    }

    #[test]
    fn clock_format_value_round_trips_through_parse() {
        assert_eq!(clock_format_value(ClockFormat::Twelve), "12h");
        assert_eq!(clock_format_value(ClockFormat::TwentyFour), "24h");
        assert_eq!(
            parse_clock_format(clock_format_value(ClockFormat::Twelve)),
            ClockFormat::Twelve
        );
        assert_eq!(
            parse_clock_format(clock_format_value(ClockFormat::TwentyFour)),
            ClockFormat::TwentyFour
        );
    }

    #[test]
    fn write_clock_format_flips_the_shared_key() {
        let backend = MapBackend::with_values(&[(INTERFACE_SCHEMA, CLOCK_FORMAT_KEY, "24h")]);
        assert!(write_clock_format(&backend, ClockFormat::Twelve));
        assert_eq!(
            backend
                .string(INTERFACE_SCHEMA, CLOCK_FORMAT_KEY)
                .as_deref(),
            Some("12h")
        );
        assert!(write_clock_format(&backend, ClockFormat::TwentyFour));
        assert_eq!(
            backend
                .string(INTERFACE_SCHEMA, CLOCK_FORMAT_KEY)
                .as_deref(),
            Some("24h")
        );
    }

    #[test]
    fn write_wallpaper_uri_round_trips_and_refuses_blank() {
        let backend = MapBackend::default();
        assert!(write_wallpaper_uri(&backend, "file:///wall.png"));
        assert_eq!(
            backend
                .string(BACKGROUND_SCHEMA, PICTURE_URI_KEY)
                .as_deref(),
            Some("file:///wall.png")
        );
        // Surrounding whitespace is trimmed before the write.
        assert!(write_wallpaper_uri(&backend, "  file:///next.png\n"));
        assert_eq!(
            backend
                .string(BACKGROUND_SCHEMA, PICTURE_URI_KEY)
                .as_deref(),
            Some("file:///next.png")
        );
        // Blank URIs never touch the backend: the stored value stays.
        assert!(!write_wallpaper_uri(&backend, "   "));
        assert_eq!(
            backend
                .string(BACKGROUND_SCHEMA, PICTURE_URI_KEY)
                .as_deref(),
            Some("file:///next.png")
        );
    }

    #[test]
    fn refresh_applies_icon_theme_and_keeps_on_absent_or_blank() {
        let backend =
            MapBackend::with_values(&[(INTERFACE_SCHEMA, ICON_THEME_KEY, "HighContrast")]);
        let mut settings = ShellSettings::default();
        refresh(&mut settings, &backend);
        assert_eq!(settings.icon_theme, "HighContrast");

        // Absent key keeps the previously read theme.
        refresh(&mut settings, &MapBackend::default());
        assert_eq!(settings.icon_theme, "HighContrast");

        // Blank value is ignored, like the wallpaper URI.
        let blank = MapBackend::with_values(&[(INTERFACE_SCHEMA, ICON_THEME_KEY, "   ")]);
        refresh(&mut settings, &blank);
        assert_eq!(settings.icon_theme, "HighContrast");
    }
}
