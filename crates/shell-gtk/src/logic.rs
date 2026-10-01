//! Toolkit-free pieces of the GTK shell, unit-tested without a display.

/// Clock format, from `org.gnome.desktop.interface clock-format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockFormat {
    /// `1:06 PM`
    TwelveHour,
    /// `13:06`
    TwentyFourHour,
}

impl ClockFormat {
    /// Parse the GSettings value; anything unknown reads as 24h, the
    /// GNOME schema default.
    pub fn from_setting(raw: &str) -> Self {
        if raw == "12h" {
            Self::TwelveHour
        } else {
            Self::TwentyFourHour
        }
    }
}

/// GNOME 51 panel clock text: weekday, month, day, then the time,
/// separated by two spaces (`Thu Oct 1  1:06 PM`).
pub fn clock_text(time: &jiff::civil::DateTime, format: ClockFormat) -> String {
    let date = time.strftime("%a %b %-d").to_string();
    let clock = match format {
        ClockFormat::TwelveHour => time.strftime("%-I:%M %p").to_string(),
        ClockFormat::TwentyFourHour => time.strftime("%H:%M").to_string(),
    };
    format!("{date}  {clock}")
}

/// Calendar popover heading: weekday line and full date line
/// (`Thursday` / `October 1 2026`).
pub fn calendar_heading(time: &jiff::civil::DateTime) -> (String, String) {
    (
        time.strftime("%A").to_string(),
        time.strftime("%B %-d %Y").to_string(),
    )
}

/// Workspace indicator shape: one entry per workspace, `true` for the
/// active one (drawn as the wide pill, the rest as dots). GNOME always
/// shows at least one workspace.
pub fn workspace_pills(workspaces: &[u32], active: u32) -> Vec<bool> {
    if workspaces.is_empty() {
        return vec![true];
    }
    workspaces.iter().map(|ws| *ws == active).collect()
}

/// `org.gnome.desktop.interface color-scheme` value for the Dark Style
/// quick toggle.
pub fn color_scheme_for(dark: bool) -> &'static str {
    if dark {
        "prefer-dark"
    } else {
        "default"
    }
}

/// Whether a `color-scheme` value means Dark Style is on.
pub fn is_dark(color_scheme: &str) -> bool {
    color_scheme == "prefer-dark"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(h: i8, m: i8) -> jiff::civil::DateTime {
        jiff::civil::date(2026, 10, 1).at(h, m, 0, 0)
    }

    #[test]
    fn clock_matches_gnome_51_panel_text() {
        assert_eq!(
            clock_text(&at(13, 6), ClockFormat::TwelveHour),
            "Thu Oct 1  1:06 PM"
        );
        assert_eq!(
            clock_text(&at(13, 6), ClockFormat::TwentyFourHour),
            "Thu Oct 1  13:06"
        );
        assert_eq!(
            clock_text(&at(0, 5), ClockFormat::TwelveHour),
            "Thu Oct 1  12:05 AM"
        );
    }

    #[test]
    fn clock_format_parses_the_gsettings_value() {
        assert_eq!(ClockFormat::from_setting("12h"), ClockFormat::TwelveHour);
        assert_eq!(
            ClockFormat::from_setting("24h"),
            ClockFormat::TwentyFourHour
        );
        assert_eq!(
            ClockFormat::from_setting("bogus"),
            ClockFormat::TwentyFourHour
        );
    }

    #[test]
    fn calendar_heading_matches_gnome() {
        assert_eq!(
            calendar_heading(&at(13, 6)),
            ("Thursday".to_owned(), "October 1 2026".to_owned())
        );
    }

    #[test]
    fn pills_mark_the_active_workspace_and_never_vanish() {
        assert_eq!(workspace_pills(&[0, 1, 2], 1), vec![false, true, false]);
        assert_eq!(workspace_pills(&[], 0), vec![true]);
    }

    #[test]
    fn dark_style_round_trips_through_color_scheme() {
        assert!(is_dark(color_scheme_for(true)));
        assert!(!is_dark(color_scheme_for(false)));
        assert_eq!(color_scheme_for(false), "default");
    }
}
