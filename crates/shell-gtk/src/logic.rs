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

/// Which parts the panel clock shows: GNOME's `clock-show-weekday`,
/// `clock-show-date` and `clock-show-seconds`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockParts {
    pub weekday: bool,
    pub date: bool,
    pub seconds: bool,
}

impl Default for ClockParts {
    /// What the panel shows without settings: weekday and date.
    fn default() -> Self {
        Self {
            weekday: true,
            date: true,
            seconds: false,
        }
    }
}

/// GNOME 51 panel clock text, built the way gnome-desktop's wall clock
/// builds it: optional weekday and date, then the time, separated by
/// two spaces (`Thu Oct 1  1:06 PM`).
pub fn clock_text(time: &jiff::civil::DateTime, format: ClockFormat, parts: ClockParts) -> String {
    let date = match (parts.weekday, parts.date) {
        (true, true) => time.strftime("%a %b %-d").to_string(),
        (false, true) => time.strftime("%b %-d").to_string(),
        (true, false) => time.strftime("%a").to_string(),
        (false, false) => String::new(),
    };
    let clock = match (format, parts.seconds) {
        (ClockFormat::TwelveHour, false) => time.strftime("%-I:%M %p").to_string(),
        (ClockFormat::TwelveHour, true) => time.strftime("%-I:%M:%S %p").to_string(),
        (ClockFormat::TwentyFourHour, false) => time.strftime("%H:%M").to_string(),
        (ClockFormat::TwentyFourHour, true) => time.strftime("%H:%M:%S").to_string(),
    };
    if date.is_empty() {
        clock
    } else {
        format!("{date}  {clock}")
    }
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
        let full = ClockParts::default();
        assert_eq!(
            clock_text(&at(13, 6), ClockFormat::TwelveHour, full),
            "Thu Oct 1  1:06 PM"
        );
        assert_eq!(
            clock_text(&at(13, 6), ClockFormat::TwentyFourHour, full),
            "Thu Oct 1  13:06"
        );
        assert_eq!(
            clock_text(&at(0, 5), ClockFormat::TwelveHour, full),
            "Thu Oct 1  12:05 AM"
        );
    }

    #[test]
    fn clock_parts_follow_the_gnome_keys() {
        let parts = |weekday, date, seconds| ClockParts {
            weekday,
            date,
            seconds,
        };
        let t = at(13, 6);
        let h24 = ClockFormat::TwentyFourHour;
        assert_eq!(
            clock_text(&t, h24, parts(false, true, false)),
            "Oct 1  13:06"
        );
        assert_eq!(clock_text(&t, h24, parts(true, false, false)), "Thu  13:06");
        assert_eq!(clock_text(&t, h24, parts(false, false, false)), "13:06");
        assert_eq!(clock_text(&t, h24, parts(false, false, true)), "13:06:00");
        assert_eq!(
            clock_text(&t, ClockFormat::TwelveHour, parts(false, false, true)),
            "1:06:00 PM"
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
            categories: Vec::new(),
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

/// power-profiles-daemon's neutral profile.
pub const BALANCED: &str = "balanced";

/// GNOME 51's Power Mode tile is checked whenever the profile is not
/// balanced.
pub fn power_mode_checked(profile: &str) -> bool {
    profile != BALANCED
}

/// Profile a Power Mode tile click switches to: back to balanced when
/// checked, else to power saver (GNOME's quick toggle shape).
pub fn power_mode_after_click(profile: &str) -> &'static str {
    if power_mode_checked(profile) {
        BALANCED
    } else {
        "power-saver"
    }
}

/// Tile subtitle for a power profile, in GNOME's words.
pub fn power_mode_label(profile: &str) -> &'static str {
    match profile {
        "power-saver" => "Power Saver",
        "performance" => "Performance",
        _ => "Balanced",
    }
}

/// Parse `wpctl get-volume` output (`Volume: 0.40 [MUTED]`) into a
/// 0..=100 percentage and the mute flag. `None` on anything else.
pub fn parse_wpctl_volume(out: &str) -> Option<(f64, bool)> {
    let rest = out.trim().strip_prefix("Volume:")?.trim();
    let value: f64 = rest.split_whitespace().next()?.parse().ok()?;
    Some(((value * 100.0).clamp(0.0, 150.0), rest.contains("[MUTED]")))
}

/// `wpctl set-volume` argument for a 0..=100 slider value.
pub fn wpctl_volume_arg(percent: f64) -> String {
    format!("{:.2}", percent.clamp(0.0, 100.0) / 100.0)
}

/// Slider percentage for a backlight reading.
pub fn brightness_percent(value: u32, max: u32) -> f64 {
    if max == 0 {
        return 0.0;
    }
    (value as f64 * 100.0 / max as f64).clamp(0.0, 100.0)
}

/// Backlight value for a slider percentage. Never zero: a black
/// screen is not a brightness level (GNOME keeps a floor too).
pub fn brightness_value(percent: f64, max: u32) -> u32 {
    let raw = (percent.clamp(0.0, 100.0) * max as f64 / 100.0).round() as u32;
    raw.clamp(1.min(max), max)
}

#[cfg(test)]
mod service_tests {
    use super::*;

    #[test]
    fn power_mode_follows_gnome_51() {
        assert!(!power_mode_checked("balanced"));
        assert!(power_mode_checked("power-saver"));
        assert!(power_mode_checked("performance"));
        assert_eq!(power_mode_after_click("balanced"), "power-saver");
        assert_eq!(power_mode_after_click("power-saver"), "balanced");
        assert_eq!(power_mode_after_click("performance"), "balanced");
        assert_eq!(power_mode_label("power-saver"), "Power Saver");
        assert_eq!(power_mode_label("unknown"), "Balanced");
    }

    #[test]
    fn wpctl_volume_parses_level_and_mute() {
        assert_eq!(parse_wpctl_volume("Volume: 0.40\n"), Some((40.0, false)));
        assert_eq!(
            parse_wpctl_volume("Volume: 1.00 [MUTED]"),
            Some((100.0, true))
        );
        assert_eq!(parse_wpctl_volume("Error: no default sink"), None);
        assert_eq!(wpctl_volume_arg(40.0), "0.40");
        assert_eq!(wpctl_volume_arg(250.0), "1.00");
    }

    #[test]
    fn brightness_round_trips_and_never_goes_black() {
        assert_eq!(brightness_percent(512, 1024), 50.0);
        assert_eq!(brightness_percent(5, 0), 0.0);
        assert_eq!(brightness_value(50.0, 1024), 512);
        assert_eq!(brightness_value(0.0, 1024), 1);
        assert_eq!(brightness_value(100.0, 1024), 1024);
        assert_eq!(brightness_value(30.0, 0), 0);
    }
}

/// Idle-lock timeout from GNOME's keys (#63), `0` for never: the screen
/// blanks after `idle-delay` seconds (`0` never) and, with
/// `lock-enabled`, locks `lock-delay` seconds later. Roost locks at that
/// point (it has no separate blank stage yet).
pub fn idle_lock_ms(idle_delay_s: u32, lock_enabled: bool, lock_delay_s: u32) -> u64 {
    if idle_delay_s == 0 || !lock_enabled {
        return 0;
    }
    (u64::from(idle_delay_s) + u64::from(lock_delay_s)) * 1000
}

#[cfg(test)]
mod idle_tests {
    use super::*;

    #[test]
    fn idle_lock_follows_gnome_keys() {
        assert_eq!(idle_lock_ms(300, true, 0), 300_000, "GNOME 51 default");
        assert_eq!(idle_lock_ms(300, true, 30), 330_000);
        assert_eq!(idle_lock_ms(0, true, 0), 0, "idle-delay 0 is never");
        assert_eq!(idle_lock_ms(300, false, 0), 0, "lock disabled");
    }
}

/// GNOME's input sources (`[('xkb', 'us'), ('xkb', 'de+nodeadkeys')]`)
/// as one xkb keymap: comma-separated layouts and variants in order.
/// Non-xkb sources (IBus engines) are skipped; none at all means `us`.
pub fn xkb_from_sources(sources: &[(String, String)]) -> (String, String) {
    let parts: Vec<(&str, &str)> = sources
        .iter()
        .filter(|(kind, _)| kind == "xkb")
        .map(|(_, id)| id.split_once('+').unwrap_or((id.as_str(), "")))
        .collect();
    if parts.is_empty() {
        return ("us".to_owned(), String::new());
    }
    let layouts: Vec<&str> = parts.iter().map(|(l, _)| *l).collect();
    let variants: Vec<&str> = parts.iter().map(|(_, v)| *v).collect();
    let variants = if variants.iter().all(|v| v.is_empty()) {
        String::new()
    } else {
        variants.join(",")
    };
    (layouts.join(","), variants)
}

#[cfg(test)]
mod input_tests {
    use super::*;

    fn src(kind: &str, id: &str) -> (String, String) {
        (kind.to_owned(), id.to_owned())
    }

    #[test]
    fn input_sources_become_one_keymap() {
        assert_eq!(xkb_from_sources(&[]), ("us".into(), String::new()));
        assert_eq!(
            xkb_from_sources(&[src("xkb", "us"), src("xkb", "de")]),
            ("us,de".into(), String::new())
        );
        assert_eq!(
            xkb_from_sources(&[src("xkb", "us"), src("xkb", "de+nodeadkeys")]),
            ("us,de".into(), ",nodeadkeys".into())
        );
        assert_eq!(
            xkb_from_sources(&[src("ibus", "anthy"), src("xkb", "fr+bepo")]),
            ("fr".into(), "bepo".into())
        );
    }
}

/// The compositor's wallpaper drop file, from GNOME's background keys:
/// the picture URI on the first line (the dark variant when the dark
/// style is on and one is set; none when `picture-options` is "none"),
/// GNOME's `primary-color` on the second.
pub fn wallpaper_drop(
    uri: &str,
    uri_dark: &str,
    prefer_dark: bool,
    options: &str,
    primary: &str,
) -> String {
    let picture = if options == "none" {
        ""
    } else if prefer_dark && !uri_dark.is_empty() {
        uri_dark
    } else {
        uri
    };
    format!("{picture}\n{primary}\n")
}

#[cfg(test)]
mod wallpaper_tests {
    use super::wallpaper_drop;

    #[test]
    fn wallpaper_follows_gnome_background_keys() {
        let l = "file:///usr/share/backgrounds/gnome/adwaita-l.jxl";
        let d = "file:///usr/share/backgrounds/gnome/adwaita-d.jxl";
        assert_eq!(
            wallpaper_drop(l, d, false, "zoom", "#023c88"),
            format!("{l}\n#023c88\n")
        );
        assert_eq!(
            wallpaper_drop(l, d, true, "zoom", "#023c88"),
            format!("{d}\n#023c88\n")
        );
        assert_eq!(
            wallpaper_drop(l, "", true, "zoom", "#023c88"),
            format!("{l}\n#023c88\n")
        );
        assert_eq!(
            wallpaper_drop(l, d, false, "none", "#023c88"),
            "\n#023c88\n"
        );
    }
}
