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

/// GNOME-style app search ranking (#57): names starting with the query
/// first, then names with a word starting with it, then any other match
/// on name, generic name, keywords, or id; ties keep discovery order.
/// Case-insensitive; a blank query matches nothing.
pub fn rank_apps<'a>(
    apps: &'a [roost_shell_host::apps::AppEntry],
    query: &str,
    limit: usize,
) -> Vec<&'a roost_shell_host::apps::AppEntry> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(u8, usize, &roost_shell_host::apps::AppEntry)> = apps
        .iter()
        .enumerate()
        .filter_map(|(i, app)| {
            let name = app.name.to_lowercase();
            let score = if name.starts_with(&needle) {
                0
            } else if name
                .split(|c: char| !c.is_alphanumeric())
                .any(|word| word.starts_with(&needle))
            {
                1
            } else {
                let mut hay = format!("{name} {}", app.app_id.to_lowercase());
                if let Some(g) = &app.generic_name {
                    hay.push(' ');
                    hay.push_str(&g.to_lowercase());
                }
                for k in &app.keywords {
                    hay.push(' ');
                    hay.push_str(&k.to_lowercase());
                }
                if hay.contains(&needle) {
                    2
                } else {
                    return None;
                }
            };
            Some((score, i, app))
        })
        .collect();
    scored.sort_by_key(|(score, i, _)| (*score, *i));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, app)| app)
        .collect()
}

/// GNOME's default dash favorites, used when the user pinned none;
/// only those installed are shown.
pub const DEFAULT_FAVORITES: &[&str] = &[
    "org.gnome.Nautilus.desktop",
    "firefox.desktop",
    "org.mozilla.firefox.desktop",
    "org.gnome.Software.desktop",
    "org.gnome.Console.desktop",
    "org.gnome.Terminal.desktop",
    "org.gnome.Settings.desktop",
];

#[cfg(test)]
mod search_tests {
    use super::*;
    use roost_shell_host::apps::AppEntry;

    fn app(id: &str, name: &str, generic: Option<&str>, keywords: &[&str]) -> AppEntry {
        AppEntry {
            app_id: id.to_owned(),
            name: name.to_owned(),
            generic_name: generic.map(str::to_owned),
            keywords: keywords.iter().map(|k| (*k).to_owned()).collect(),
            argv: vec!["true".into()],
            icon: None,
        }
    }

    #[test]
    fn prefix_beats_word_prefix_beats_substring() {
        let apps = vec![
            app("a.desktop", "Text Editor", None, &[]),
            app("b.desktop", "Edit Tool", None, &[]),
            app("c.desktop", "Credits", None, &["editing"]),
        ];
        let names: Vec<&str> = rank_apps(&apps, "edit", 10)
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, vec!["Edit Tool", "Text Editor", "Credits"]);
    }

    #[test]
    fn generic_names_and_keywords_match() {
        let apps = vec![app(
            "org.gnome.Nautilus.desktop",
            "Files",
            Some("File Manager"),
            &["folder"],
        )];
        assert_eq!(rank_apps(&apps, "folder", 5).len(), 1);
        assert_eq!(rank_apps(&apps, "manager", 5).len(), 1);
        assert_eq!(rank_apps(&apps, "nautilus", 5).len(), 1);
    }

    #[test]
    fn blank_queries_match_nothing_and_limits_apply() {
        let apps: Vec<AppEntry> = (0..20)
            .map(|i| app(&format!("{i}.desktop"), &format!("App {i}"), None, &[]))
            .collect();
        assert!(rank_apps(&apps, "   ", 5).is_empty());
        assert_eq!(rank_apps(&apps, "app", 6).len(), 6);
    }
}
