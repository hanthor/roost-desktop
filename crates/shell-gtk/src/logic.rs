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

/// GNOME 51 panel clock text, exactly as gnome-desktop's wall clock
/// builds it: optional weekday and date, an EN SPACE (U+2002), then the
/// time. In 12-hour mode a FIGURE SPACE (U+2007) pads single-digit
/// hours (`%l`), so the width holds steady. A weekday alone takes a
/// plain space. All checked against GnomeWallClock in GNOME 51 (`Oct 1\u{2002}\u{2007}1:06 PM`).
pub fn clock_text(time: &jiff::civil::DateTime, format: ClockFormat, parts: ClockParts) -> String {
    let date = match (parts.weekday, parts.date) {
        (true, true) => time.strftime("%a %b %-d").to_string(),
        (false, true) => time.strftime("%b %-d").to_string(),
        (true, false) => time.strftime("%a").to_string(),
        (false, false) => String::new(),
    };
    let clock = match (format, parts.seconds) {
        (ClockFormat::TwelveHour, false) => time.strftime("%l:%M %p").to_string(),
        (ClockFormat::TwelveHour, true) => time.strftime("%l:%M:%S %p").to_string(),
        (ClockFormat::TwentyFourHour, false) => time.strftime("%H:%M").to_string(),
        (ClockFormat::TwentyFourHour, true) => time.strftime("%H:%M:%S").to_string(),
    };
    // `%l` pads with an ASCII space; GNOME swaps it for a figure space.
    let clock = match clock.strip_prefix(' ') {
        Some(rest) => format!("\u{2007}{rest}"),
        None => clock,
    };
    match (parts.weekday, parts.date) {
        (_, true) => format!("{date}\u{2002}{clock}"),
        (true, false) => format!("{date} {clock}"),
        (false, false) => clock,
    }
}

/// Today button labels: weekday line and full date line (`Thursday` /
/// `October 1 2026`, dateMenu.js's `%A` and `%B %-d %Y`).
pub fn calendar_heading(time: &jiff::civil::DateTime) -> (String, String) {
    (
        time.strftime("%A").to_string(),
        time.strftime("%B %-d %Y").to_string(),
    )
}

/// The 42 days (six weeks) GNOME's calendar grid shows for `month`,
/// starting on the locale's first weekday (`week_start`, 0 = Sunday),
/// with the first of the month in the first row (calendar.js
/// `_buildMonth`).
pub fn month_grid(year: i16, month: i8, week_start: i8) -> Vec<jiff::civil::Date> {
    let first = jiff::civil::date(year, month, 1);
    let weekday = first.weekday().to_sunday_zero_offset();
    let back = (weekday - week_start).rem_euclid(7);
    let start = first
        .checked_sub(jiff::Span::new().days(i64::from(back)))
        .unwrap_or(first);
    (0..42)
        .map(|i| {
            start
                .checked_add(jiff::Span::new().days(i))
                .unwrap_or(start)
        })
        .collect()
}

/// One-letter weekday headings from `week_start` on, GNOME's English
/// (`C_('grid sunday', 'S')`...).
pub fn weekday_initials(week_start: i8) -> Vec<&'static str> {
    const DAYS: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];
    (0..7)
        .map(|i| DAYS[((i + week_start).rem_euclid(7)) as usize])
        .collect()
}

/// GNOME's work-free days (`C_('calendar-no-work', '06')`): Sunday and
/// Saturday.
pub fn is_weekend(day: jiff::civil::Date) -> bool {
    matches!(day.weekday().to_sunday_zero_offset(), 0 | 6)
}

/// The calendar header: the month, plus the year when it is not this
/// year (calendar.js `_updateMonthLabel`).
/// GNOME's lock-screen clock (`formatTime(.., {timeOnly: true})`):
/// "22:38", or "10:38 PM" under the 12-hour format.
pub fn lock_clock_text(time: &jiff::civil::DateTime, format: ClockFormat) -> String {
    let parts = ClockParts {
        weekday: false,
        date: false,
        seconds: false,
    };
    clock_text(time, format, parts)
}

/// GNOME's lock-screen date line: "Thursday October 1".
pub fn lock_date_text(date: jiff::civil::Date) -> String {
    date.strftime("%A %B %-d").to_string()
}

/// Top margins of the lock screen's clock and prompt for an output
/// `height` pixels tall: GNOME's stack sits a third of the way down,
/// which puts the clock at y 275 and the prompt at y 205 on 800 rows.
pub fn lock_offsets(height: i32) -> (i32, i32) {
    let third = height.max(0) / 3;
    ((third + 9).max(0), (third - 61).max(0))
}

/// The user's display name for the unlock prompt: the GECOS real name,
/// else the login name (GNOME's user widget, without AccountsService).
pub fn real_name() -> String {
    // SAFETY: getpwuid returns a pointer into static storage, read at once.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return String::new();
        }
        let gecos = if (*pw).pw_gecos.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr((*pw).pw_gecos)
                .to_string_lossy()
                .into_owned()
        };
        let login = std::ffi::CStr::from_ptr((*pw).pw_name)
            .to_string_lossy()
            .into_owned();
        display_name(&gecos, &login)
    }
}

/// GECOS's first field when it has one, else the login name.
pub fn display_name(gecos: &str, login: &str) -> String {
    match gecos.split(',').next().map(str::trim) {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => login.to_owned(),
    }
}

pub fn month_label(shown: jiff::civil::Date, today: jiff::civil::Date) -> String {
    if shown.year() == today.year() {
        shown.strftime("%B").to_string()
    } else {
        shown.strftime("%B %Y").to_string()
    }
}

/// Workspace indicator shape: one entry per workspace, `true` for the
/// active one (drawn as the wide pill, the rest as dots). GNOME always
/// shows at least one workspace.
///
/// GNOME's dynamic workspaces (`windowManager.js` `_checkWorkspaces`)
/// always keep one empty workspace at the end and never fewer than two
/// (`MIN_NUM_WORKSPACES`), and the indicator shows them: when the last
/// workspace holds a window (`occupied`) a trailing dot follows, and a
/// lone workspace gets one too.
pub fn workspace_pills(workspaces: &[u32], active: u32, occupied: &[u32]) -> Vec<bool> {
    let mut pills: Vec<bool> = workspaces.iter().map(|ws| *ws == active).collect();
    if pills.is_empty() {
        pills.push(true);
    }
    if workspaces
        .last()
        .is_some_and(|last| occupied.contains(last))
    {
        pills.push(false);
    }
    while pills.len() < 2 {
        pills.push(false);
    }
    pills
}

/// Width of the active workspace pill, GNOME 51's `WorkspaceDot`: the
/// 8px dot times 3.625 with up to two indicators, 3.25 up to five, 2.75
/// beyond (`panel.js` `_updateExpansion`), rounded.
pub fn active_pill_width(indicators: usize) -> i32 {
    let multiplier = match indicators {
        0..=2 => 3.625,
        3..=5 => 3.25,
        _ => 2.75,
    };
    (8.0_f64 * multiplier).round() as i32
}

/// GTK's font settings (`gtk-xft-hinting`, `-hintstyle`, `-antialias`,
/// `-rgba`) for GNOME's `font-hinting` and `font-antialiasing` keys, the
/// mapping gnome-settings-daemon's xsettings plugin uses.
pub fn font_rendering(hinting: &str, antialiasing: &str) -> (i32, &'static str, i32, &'static str) {
    let style = match hinting {
        "none" => "hintnone",
        "medium" => "hintmedium",
        "full" => "hintfull",
        _ => "hintslight",
    };
    let hint = i32::from(hinting != "none");
    let (aa, rgba) = match antialiasing {
        "none" => (0, "none"),
        "rgba" => (1, "rgb"),
        _ => (1, "none"),
    };
    (hint, style, aa, rgba)
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
    fn lock_screen_text_matches_gnome_51() {
        let t = jiff::civil::date(2026, 10, 1).at(22, 38, 0, 0);
        assert_eq!(lock_clock_text(&t, ClockFormat::TwentyFourHour), "22:38");
        assert_eq!(lock_clock_text(&t, ClockFormat::TwelveHour), "10:38 PM");
        assert_eq!(lock_date_text(t.date()), "Thursday October 1");
        assert_eq!(lock_offsets(800), (275, 205));
        assert_eq!(display_name("Ada Lovelace,,,", "ada"), "Ada Lovelace");
        assert_eq!(display_name("", "ada"), "ada");
        assert_eq!(display_name(",,,", "ada"), "ada");
    }

    #[test]
    fn clock_matches_gnome_51_panel_text() {
        let full = ClockParts::default();
        assert_eq!(
            clock_text(&at(13, 6), ClockFormat::TwelveHour, full),
            "Thu Oct 1\u{2002}\u{2007}1:06 PM"
        );
        assert_eq!(
            clock_text(&at(13, 6), ClockFormat::TwentyFourHour, full),
            "Thu Oct 1\u{2002}13:06"
        );
        assert_eq!(
            clock_text(&at(0, 5), ClockFormat::TwelveHour, full),
            "Thu Oct 1\u{2002}12:05 AM"
        );
        // GNOME 51's defaults (no weekday), from gnome-desktop's own
        // GnomeWallClock in the reference session.
        let gnome = ClockParts {
            weekday: false,
            ..ClockParts::default()
        };
        assert_eq!(
            clock_text(&at(20, 4), ClockFormat::TwentyFourHour, gnome),
            "Oct 1\u{2002}20:04"
        );
        assert_eq!(
            clock_text(&at(20, 4), ClockFormat::TwelveHour, gnome),
            "Oct 1\u{2002}\u{2007}8:04 PM"
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
            "Oct 1\u{2002}13:06"
        );
        assert_eq!(clock_text(&t, h24, parts(true, false, false)), "Thu 13:06");
        assert_eq!(clock_text(&t, h24, parts(false, false, false)), "13:06");
        assert_eq!(clock_text(&t, h24, parts(false, false, true)), "13:06:00");
        assert_eq!(
            clock_text(&t, ClockFormat::TwelveHour, parts(false, false, true)),
            "\u{2007}1:06:00 PM"
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
        assert_eq!(
            workspace_pills(&[0, 1, 2], 1, &[1]),
            vec![false, true, false]
        );
        assert_eq!(workspace_pills(&[], 0, &[]), vec![true, false]);
    }

    #[test]
    fn font_rendering_follows_gnome_keys() {
        assert_eq!(
            font_rendering("slight", "grayscale"),
            (1, "hintslight", 1, "none")
        );
        assert_eq!(font_rendering("none", "rgba"), (0, "hintnone", 1, "rgb"));
        assert_eq!(font_rendering("full", "none"), (1, "hintfull", 0, "none"));
    }

    #[test]
    fn volume_icons_follow_gnome_thirds() {
        assert_eq!(volume_icon(0.0, false), "audio-volume-muted-symbolic");
        assert_eq!(volume_icon(80.0, true), "audio-volume-muted-symbolic");
        assert_eq!(volume_icon(20.0, false), "audio-volume-low-symbolic");
        assert_eq!(volume_icon(33.0, false), "audio-volume-low-symbolic");
        assert_eq!(volume_icon(50.0, false), "audio-volume-medium-symbolic");
        assert_eq!(volume_icon(100.0, false), "audio-volume-high-symbolic");
        assert_eq!(volume_icon(150.0, false), "audio-volume-high-symbolic");
    }

    #[test]
    fn month_grid_matches_gnome_51() {
        // October 2026 from Sunday: Sep 27 .. Nov 7, as GNOME draws it.
        let grid = month_grid(2026, 10, 0);
        assert_eq!(grid.len(), 42);
        assert_eq!(grid[0], jiff::civil::date(2026, 9, 27));
        assert_eq!(grid[4], jiff::civil::date(2026, 10, 1));
        assert_eq!(grid[41], jiff::civil::date(2026, 11, 7));
        // From Monday the first row starts on Monday Sep 28.
        assert_eq!(month_grid(2026, 10, 1)[0], jiff::civil::date(2026, 9, 28));
        // A month starting on the week start leads with the first.
        assert_eq!(month_grid(2026, 11, 0)[0], jiff::civil::date(2026, 11, 1));
    }

    #[test]
    fn calendar_labels_follow_gnome() {
        assert_eq!(weekday_initials(0).concat(), "SMTWTFS");
        assert_eq!(weekday_initials(1).concat(), "MTWTFSS");
        assert!(is_weekend(jiff::civil::date(2026, 10, 3)));
        assert!(!is_weekend(jiff::civil::date(2026, 10, 2)));
        let today = jiff::civil::date(2026, 10, 1);
        assert_eq!(month_label(today, today), "October");
        assert_eq!(
            month_label(jiff::civil::date(2027, 1, 5), today),
            "January 2027"
        );
    }

    #[test]
    fn time_spans_follow_gnome() {
        assert_eq!(time_span(0), "Just now");
        assert_eq!(time_span(299), "Just now");
        assert_eq!(time_span(300), "5 minutes ago");
        assert_eq!(time_span(3600), "1 hour ago");
        assert_eq!(time_span(86_400), "Yesterday");
        assert_eq!(time_span(3 * 86_400), "3 days ago");
        assert_eq!(time_span(20 * 86_400), "2 weeks ago");
        assert_eq!(time_span(100 * 86_400), "3 months ago");
        assert_eq!(time_span(800 * 86_400), "2 years ago");
    }

    #[test]
    fn switcher_icons_shrink_like_gnome() {
        assert_eq!(switcher_icon_size(1, 1280), 96);
        assert_eq!(switcher_icon_size(3, 1280), 96);
        // 9 x 127 + 8 x 12 = 1239 > 1256? no: fits; 10 do not at 96.
        assert_eq!(switcher_icon_size(9, 1280), 96);
        assert_eq!(switcher_icon_size(10, 1280), 64);
        assert_eq!(switcher_icon_size(40, 1280), 22);
    }

    #[test]
    fn active_pill_widths_follow_gnome() {
        assert_eq!(active_pill_width(2), 29);
        assert_eq!(active_pill_width(5), 26);
        assert_eq!(active_pill_width(6), 22);
    }

    #[test]
    fn pills_show_gnomes_trailing_empty_workspace() {
        // No windows: GNOME still keeps two workspaces.
        assert_eq!(workspace_pills(&[0], 0, &[]), vec![true, false]);
        // A window on it: GNOME adds the empty one after it.
        assert_eq!(workspace_pills(&[0], 0, &[0]), vec![true, false]);
        // Already on the empty last one: nothing more.
        assert_eq!(workspace_pills(&[0, 1], 1, &[0]), vec![false, true]);
        assert_eq!(
            workspace_pills(&[0, 1], 0, &[0, 1]),
            vec![true, false, false]
        );
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

/// GNOME 51's default dash favorites (gnome-shell's `favorite-apps`
/// default as GNOME 51 ships it), used when the schema is missing;
/// only those installed are shown.
pub const DEFAULT_FAVORITES: &[&str] = &[
    "org.mozilla.firefox.desktop",
    "org.gnome.Calendar.desktop",
    "org.gnome.Nautilus.desktop",
    "org.gnome.Software.desktop",
    "org.gnome.TextEditor.desktop",
    "org.gnome.Calculator.desktop",
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

/// Alt+Tab icon size (altTab.js `_setIconSize`): the largest of 96, 64,
/// 48, 32 and 22 at which `items` tiles (icon plus 31px of label and
/// padding, 12px apart) fit `width` less the list's 24px padding.
pub fn switcher_icon_size(items: usize, width: i32) -> i32 {
    const SIZES: [i32; 5] = [96, 64, 48, 32, 22];
    if items <= 1 {
        return SIZES[0];
    }
    let n = items as i32;
    let avail = width - 24;
    SIZES
        .into_iter()
        .find(|size| (size + 31) * n + 12 * (n - 1) <= avail)
        .unwrap_or(SIZES[4])
}

/// GNOME's output volume icon (volume.js `getIcon`): muted at zero or
/// when muted, else low, medium or high by thirds of the range.
pub fn volume_icon(percent: f64, muted: bool) -> &'static str {
    if muted || percent <= 0.0 {
        return "audio-volume-muted-symbolic";
    }
    match (3.0 * percent / 100.0).ceil().clamp(1.0, 3.0) as u8 {
        1 => "audio-volume-low-symbolic",
        2 => "audio-volume-medium-symbolic",
        _ => "audio-volume-high-symbolic",
    }
}

/// How long ago a notification arrived, in GNOME's words
/// (dateUtils.js `formatTimeSpan`).
pub fn time_span(seconds: u64) -> String {
    let minutes = seconds / 60;
    let hours = seconds / 3600;
    let days = seconds / 86_400;
    let weeks = days / 7;
    let months = days / 30;
    let years = weeks / 52;
    let plural = |n: u64, one: &str, many: &str| {
        if n == 1 {
            format!("{n} {one}")
        } else {
            format!("{n} {many}")
        }
    };
    if minutes < 5 {
        "Just now".to_owned()
    } else if hours < 1 {
        plural(minutes, "minute ago", "minutes ago")
    } else if days < 1 {
        plural(hours, "hour ago", "hours ago")
    } else if days < 2 {
        "Yesterday".to_owned()
    } else if days < 15 {
        plural(days, "day ago", "days ago")
    } else if weeks < 8 {
        plural(weeks, "week ago", "weeks ago")
    } else if years < 1 {
        plural(months, "month ago", "months ago")
    } else {
        plural(years, "year ago", "years ago")
    }
}

/// Icon for a power profile, as GNOME's Power Mode toggle shows it.
pub fn power_mode_icon(profile: &str) -> &'static str {
    match profile {
        "power-saver" => "power-profile-power-saver-symbolic",
        "performance" => "power-profile-performance-symbolic",
        _ => "power-profile-balanced-symbolic",
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

/// One brightness-key step from `percent`: GNOME's scale has twenty
/// steps, and a press lands on the next one up or down.
pub fn brightness_step(percent: f64, up: bool) -> f64 {
    let step = 100.0 / 20.0;
    let index = (percent / step).round();
    let next = if up { index + 1.0 } else { index - 1.0 };
    (next * step).clamp(0.0, 100.0)
}

/// Backlight value for a slider percentage. Never zero: a black
/// screen is not a brightness level. GNOME 51's sysfs backlight minimum
/// is max(1, max_brightness / 100), so zero percent selects that floor.
pub fn brightness_value(percent: f64, max: u32) -> u32 {
    let raw = (percent.clamp(0.0, 100.0) * max as f64 / 100.0).round() as u32;
    raw.clamp((max / 100).max(1).min(max), max)
}

#[cfg(test)]
mod service_tests {
    use super::*;

    #[test]
    fn brightness_keys_step_by_a_twentieth() {
        assert_eq!(brightness_step(50.0, true), 55.0);
        assert_eq!(brightness_step(52.0, false), 45.0);
        assert_eq!(brightness_step(100.0, true), 100.0);
        assert_eq!(brightness_step(0.0, false), 0.0);
    }

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
    fn wpctl_volume_argument_is_bounded() {
        assert_eq!(wpctl_volume_arg(40.0), "0.40");
        assert_eq!(wpctl_volume_arg(250.0), "1.00");
    }

    #[test]
    fn brightness_round_trips_and_never_goes_black() {
        assert_eq!(brightness_percent(512, 1024), 50.0);
        assert_eq!(brightness_percent(5, 0), 0.0);
        assert_eq!(brightness_value(50.0, 1024), 512);
        assert_eq!(brightness_value(0.0, 1024), 10);
        assert_eq!(brightness_value(0.0, 1000), 10);
        assert_eq!(brightness_value(0.0, 20), 1);
        assert_eq!(brightness_value(100.0, 1024), 1024);
        assert_eq!(brightness_value(30.0, 0), 0);
    }
}

/// Idle lock follows the screen shield: fade for ten seconds, then lock,
/// or a longer configured lock-delay. Disabled animations remove that minimum.
pub fn idle_lock_ms(
    idle_delay_s: u32,
    lock_enabled: bool,
    lock_delay_s: u32,
    animations: bool,
) -> u64 {
    if idle_delay_s == 0 || !lock_enabled {
        return 0;
    }
    u64::from(idle_delay_s) * 1000
        + (u64::from(lock_delay_s) * 1000).max(if animations { 10_000 } else { 0 })
}

#[cfg(test)]
mod idle_tests {
    use super::*;

    #[test]
    fn idle_lock_follows_gnome_keys() {
        assert_eq!(
            idle_lock_ms(300, true, 0, true),
            310_000,
            "GNOME 51 default"
        );
        assert_eq!(idle_lock_ms(300, true, 30, true), 330_000);
        assert_eq!(idle_lock_ms(2, true, 0, false), 2000);
        assert_eq!(idle_lock_ms(2, true, 20, false), 22000);
        assert_eq!(idle_lock_ms(0, true, 0, true), 0, "idle-delay 0 is never");
        assert_eq!(idle_lock_ms(300, false, 0, true), 0, "lock disabled");
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
    accent: &str,
) -> String {
    let picture = if options == "none" {
        ""
    } else if prefer_dark && !uri_dark.is_empty() {
        uri_dark
    } else {
        uri
    };
    format!("{picture}\n{primary}\n{}\n", accent_hex(accent))
}

/// GNOME's accent colors (`org.gnome.desktop.interface accent-color`,
/// libadwaita's `AdwAccentColor` backgrounds), blue for anything else.
pub fn accent_hex(name: &str) -> &'static str {
    match name {
        "teal" => "#2190a4",
        "green" => "#3a944a",
        "yellow" => "#c88800",
        "orange" => "#ed5b00",
        "red" => "#e62d42",
        "pink" => "#d56199",
        "purple" => "#9141ac",
        "slate" => "#6f8396",
        _ => "#3584e4",
    }
}

#[cfg(test)]
mod wallpaper_tests {
    use super::wallpaper_drop;

    #[test]
    fn wallpaper_follows_gnome_background_keys() {
        let l = "file:///usr/share/backgrounds/gnome/adwaita-l.jxl";
        let d = "file:///usr/share/backgrounds/gnome/adwaita-d.jxl";
        assert_eq!(
            wallpaper_drop(l, d, false, "zoom", "#023c88", "blue"),
            format!("{l}\n#023c88\n#3584e4\n")
        );
        assert_eq!(
            wallpaper_drop(l, d, true, "zoom", "#023c88", "purple"),
            format!("{d}\n#023c88\n#9141ac\n")
        );
        assert_eq!(
            wallpaper_drop(l, "", true, "zoom", "#023c88", ""),
            format!("{l}\n#023c88\n#3584e4\n")
        );
        assert_eq!(
            wallpaper_drop(l, d, false, "none", "#023c88", "slate"),
            "\n#023c88\n#6f8396\n"
        );
    }
}

/// One event from org.gnome.Shell.CalendarServer: its id (source uid,
/// a newline, then the component's), summary, and Unix start and end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalEvent {
    pub id: String,
    pub summary: String,
    pub start: i64,
    pub end: i64,
}

/// Whether an event overlaps an interval (calendar.js
/// `_eventOverlapsInterval`; zero-length events count).
pub fn event_overlaps(e0: i64, e1: i64, i0: i64, i1: i64) -> bool {
    if e0 >= i0 && e1 < i1 {
        return true;
    }
    !(e1 <= i0 || i1 <= e0)
}

/// A day's bounds as Unix seconds in `tz`: its midnight and the next.
pub fn day_bounds(day: jiff::civil::Date, tz: &jiff::tz::TimeZone) -> (i64, i64) {
    let at = |d: jiff::civil::Date| {
        d.at(0, 0, 0, 0)
            .to_zoned(tz.clone())
            .map(|z| z.timestamp().as_second())
            .unwrap_or(0)
    };
    let next = day.tomorrow().unwrap_or(day);
    (at(day), at(next))
}

/// Merge EventsAddedOrUpdated: an occurrence of a recurring event
/// (an id not ending in a newline) first drops every earlier
/// occurrence of its parent, once per batch, as GNOME does.
pub fn events_added(
    events: &mut std::collections::BTreeMap<String, CalEvent>,
    added: Vec<CalEvent>,
) {
    let mut handled: Vec<String> = Vec::new();
    for event in added {
        if !event.id.ends_with('\n') {
            let parent = match event.id.rfind('\n') {
                Some(i) => event.id[..=i].to_owned(),
                None => String::new(),
            };
            if !handled.contains(&parent) {
                events_remove_matching(events, &parent);
                handled.push(parent);
            }
        }
        events.insert(event.id.clone(), event);
    }
}

/// Drop every event whose id starts with `prefix`; whether any went.
pub fn events_remove_matching(
    events: &mut std::collections::BTreeMap<String, CalEvent>,
    prefix: &str,
) -> bool {
    let before = events.len();
    events.retain(|id, _| !id.starts_with(prefix));
    events.len() != before
}

/// The events overlapping `[begin, end)`, in GNOME's order: by start,
/// or by end for events running in from before the interval.
pub fn events_between<'a>(
    events: impl IntoIterator<Item = &'a CalEvent>,
    begin: i64,
    end: i64,
) -> Vec<CalEvent> {
    let mut out: Vec<CalEvent> = events
        .into_iter()
        .filter(|e| event_overlaps(e.start, e.end, begin, end))
        .cloned()
        .collect();
    let key = |e: &CalEvent| {
        if e.start < begin && e.end <= end {
            e.end
        } else {
            e.start
        }
    };
    out.sort_by_key(key);
    out
}

/// The events card's title for the selected day (dateMenu.js
/// `_updateTitle`).
pub fn events_title(selected: jiff::civil::Date, today: jiff::civil::Date) -> String {
    if selected == today {
        "Today".to_owned()
    } else if today.yesterday().ok() == Some(selected) {
        "Yesterday".to_owned()
    } else if today.tomorrow().ok() == Some(selected) {
        "Tomorrow".to_owned()
    } else if selected.year() == today.year() {
        selected.strftime("%B %-d").to_string()
    } else {
        selected.strftime("%B %-d %Y").to_string()
    }
}

/// An event's time line under its summary on `day` (dateMenu.js
/// `_formatEventTime`): "All Day", a time span, or dates and times for
/// events running past the day.
pub fn event_time_text(
    event: &CalEvent,
    day: jiff::civil::Date,
    tz: &jiff::tz::TimeZone,
    format: ClockFormat,
    this_year: i16,
) -> String {
    const EN: char = '\u{2013}';
    let (day_start, day_end) = day_bounds(day, tz);
    let local = |t: i64| {
        jiff::Timestamp::from_second(t)
            .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
            .to_zoned(tz.clone())
            .datetime()
    };
    let time_only = |t: i64| {
        let fmt = match format {
            ClockFormat::TwentyFourHour => "%H:%M",
            ClockFormat::TwelveHour => "%l:%M %p",
        };
        local(t).strftime(fmt).to_string()
    };
    if event.start == day_start && event.end == day_end {
        return "All Day".to_owned();
    }
    let (start_time, end_time) = (time_only(event.start), time_only(event.end));
    if event.start < day_start || event.end > day_end {
        let midnight =
            |d: jiff::civil::DateTime| d.hour() == 0 && d.minute() == 0 && d.second() == 0;
        let start = local(event.start);
        let mut end = local(event.end);
        let (starts_midnight, ends_midnight) = (midnight(start), midnight(end));
        if ends_midnight {
            end = end.checked_sub(jiff::Span::new().days(1)).unwrap_or(end);
        }
        let fmt = if start.year() == this_year && end.year() == this_year {
            "%m/%d"
        } else {
            "%x"
        };
        let (sd, ed) = (
            start.strftime(fmt).to_string(),
            end.strftime(fmt).to_string(),
        );
        if starts_midnight && ends_midnight {
            format!("{sd} {EN} {ed}")
        } else {
            format!("{sd} {start_time} {EN} {ed} {end_time}")
        }
    } else {
        format!("{start_time} {EN} {end_time}")
    }
}

#[cfg(test)]
mod event_tests {
    use super::*;
    use jiff::civil::date;
    use std::collections::BTreeMap;

    fn tz() -> jiff::tz::TimeZone {
        jiff::tz::TimeZone::UTC
    }

    fn ev(id: &str, start: i64, end: i64) -> CalEvent {
        CalEvent {
            id: id.into(),
            summary: id.trim().into(),
            start,
            end,
        }
    }

    #[test]
    fn titles_follow_gnome_51() {
        let today = date(2026, 10, 2);
        assert_eq!(events_title(today, today), "Today");
        assert_eq!(events_title(date(2026, 10, 1), today), "Yesterday");
        assert_eq!(events_title(date(2026, 10, 3), today), "Tomorrow");
        assert_eq!(events_title(date(2026, 10, 9), today), "October 9");
        assert_eq!(events_title(date(2027, 1, 9), today), "January 9 2027");
    }

    #[test]
    fn event_times_follow_gnome_51() {
        let day = date(2026, 10, 2);
        let (d0, d1) = day_bounds(day, &tz());
        let h = 3600;
        let all_day = ev("a\n", d0, d1);
        assert_eq!(
            event_time_text(&all_day, day, &tz(), ClockFormat::TwentyFourHour, 2026),
            "All Day"
        );
        let meeting = ev("b\n", d0 + 10 * h, d0 + 10 * h + 1800);
        assert_eq!(
            event_time_text(&meeting, day, &tz(), ClockFormat::TwentyFourHour, 2026),
            "10:00 \u{2013} 10:30"
        );
        assert_eq!(
            event_time_text(&meeting, day, &tz(), ClockFormat::TwelveHour, 2026),
            "10:00 AM \u{2013} 10:30 AM"
        );
        // Three whole days through this one: dates only, the end day
        // inclusive.
        let trip = ev("c\n", d0 - 24 * h, d1 + 24 * h);
        assert_eq!(
            event_time_text(&trip, day, &tz(), ClockFormat::TwentyFourHour, 2026),
            "10/01 \u{2013} 10/03"
        );
        // Overnight: dates and times.
        let night = ev("d\n", d0 - 2 * h, d0 + 6 * h);
        assert_eq!(
            event_time_text(&night, day, &tz(), ClockFormat::TwentyFourHour, 2026),
            "10/01 22:00 \u{2013} 10/02 06:00"
        );
    }

    #[test]
    fn overlap_sort_and_recurrence_follow_gnome() {
        let (d0, d1) = day_bounds(date(2026, 10, 2), &tz());
        let mut events = BTreeMap::new();
        events_added(
            &mut events,
            vec![
                ev("src\nlate\n", d0 + 7200, d0 + 9000),
                ev("src\nearly\n", d0 + 3600, d0 + 5400),
                ev("src\nyesterday\n", d0 - 7200, d0 - 3600),
                ev("src\nzero\n", d0 + 600, d0 + 600),
            ],
        );
        let today: Vec<_> = events_between(events.values(), d0, d1)
            .into_iter()
            .map(|e| e.summary)
            .collect();
        assert_eq!(today, ["src\nzero", "src\nearly", "src\nlate"]);
        // A recurring occurrence replaces its parent's earlier ones.
        events_added(&mut events, vec![ev("src\nrec\n1", d0, d0 + 60)]);
        events_added(&mut events, vec![ev("src\nrec\n2", d0 + 120, d0 + 180)]);
        assert!(!events.contains_key("src\nrec\n1"));
        assert!(events.contains_key("src\nrec\n2"));
        // A source going away takes its events.
        assert!(events_remove_matching(&mut events, "src\n"));
        assert!(events.is_empty());
    }
}

/// One Wi-Fi access point as NetworkManager reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPoint {
    pub path: String,
    pub ssid: Vec<u8>,
    pub strength: u8,
    pub flags: u32,
    pub wpa_flags: u32,
    pub rsn_flags: u32,
    pub mode: u32,
}

/// GNOME's grouping of access points (network.js `WirelessNetwork`):
/// one network per SSID, mode and security, at its best access point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNetwork {
    pub name: String,
    pub ssid: Vec<u8>,
    /// The strongest access point's path and strength.
    pub ap: String,
    pub strength: u8,
    pub security: WifiSecurity,
    pub active: bool,
    /// A saved connection for it, when NetworkManager has one.
    pub connection: Option<String>,
}

/// What a network asks of a client, as far as GNOME's menu cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WifiSecurity {
    /// No privacy, or OWE (opportunistic encryption, no secret).
    Open,
    /// WEP or WPA/WPA2/WPA3 personal: a password.
    Personal,
    /// 802.1X: GNOME sends these to Settings.
    Enterprise,
}

impl WifiSecurity {
    /// From NM's AP flags (NM80211ApFlags, NM80211ApSecurityFlags).
    pub fn of(flags: u32, wpa: u32, rsn: u32) -> Self {
        const PRIVACY: u32 = 0x1;
        const KEY_MGMT_802_1X: u32 = 0x200;
        const KEY_MGMT_OWE: u32 = 0x800;
        const KEY_MGMT_OWE_TM: u32 = 0x1000;
        if (wpa | rsn) & KEY_MGMT_802_1X != 0 {
            Self::Enterprise
        } else if rsn & (KEY_MGMT_OWE | KEY_MGMT_OWE_TM) != 0
            && (wpa | rsn) & !(KEY_MGMT_OWE | KEY_MGMT_OWE_TM | 0xff) == 0
        {
            Self::Open
        } else if wpa != 0 || rsn != 0 || flags & PRIVACY != 0 {
            Self::Personal
        } else {
            Self::Open
        }
    }

    /// GNOME's lock icon: neither open nor OWE.
    pub fn secure(self) -> bool {
        self != Self::Open
    }
}

/// GNOME's signal icon names (network.js `signalToIcon`).
pub fn signal_icon(strength: u8) -> &'static str {
    match strength {
        0..=19 => "network-wireless-signal-none-symbolic",
        20..=39 => "network-wireless-signal-weak-symbolic",
        40..=49 => "network-wireless-signal-ok-symbolic",
        50..=79 => "network-wireless-signal-good-symbolic",
        _ => "network-wireless-signal-excellent-symbolic",
    }
}

/// GNOME shows at most this many networks (`MAX_VISIBLE_NETWORKS`).
pub const MAX_VISIBLE_NETWORKS: usize = 8;

/// The Wi-Fi menu's networks in GNOME's order: saved first, then
/// stronger, then secure, then by name; hidden SSIDs skipped.
/// `known` maps SSIDs to saved connection paths.
pub fn wifi_networks(
    aps: &[AccessPoint],
    active_ap: Option<&str>,
    known: &[(Vec<u8>, String)],
) -> Vec<WifiNetwork> {
    let mut nets: Vec<(WifiNetwork, u32, Vec<String>)> = Vec::new();
    for ap in aps.iter().filter(|ap| !ap.ssid.is_empty()) {
        let security = WifiSecurity::of(ap.flags, ap.wpa_flags, ap.rsn_flags);
        match nets
            .iter_mut()
            .find(|(n, mode, _)| n.ssid == ap.ssid && *mode == ap.mode && n.security == security)
        {
            Some((net, _, paths)) => {
                paths.push(ap.path.clone());
                if ap.strength > net.strength {
                    net.strength = ap.strength;
                    net.ap = ap.path.clone();
                }
            }
            None => {
                let name = String::from_utf8(ap.ssid.clone())
                    .unwrap_or_else(|_| String::from_utf8_lossy(&ap.ssid).into_owned());
                nets.push((
                    WifiNetwork {
                        name: if name.is_empty() {
                            "<unknown>".into()
                        } else {
                            name
                        },
                        ssid: ap.ssid.clone(),
                        ap: ap.path.clone(),
                        strength: ap.strength,
                        security,
                        active: false,
                        connection: None,
                    },
                    ap.mode,
                    vec![ap.path.clone()],
                ));
            }
        }
    }
    let mut out: Vec<WifiNetwork> = nets
        .into_iter()
        .map(|(mut net, _, paths)| {
            net.active = active_ap.is_some_and(|a| paths.iter().any(|p| p == a));
            net.connection = known
                .iter()
                .find(|(ssid, _)| *ssid == net.ssid)
                .map(|(_, path)| path.clone());
            net
        })
        .collect();
    out.sort_by(|a, b| {
        b.connection
            .is_some()
            .cmp(&a.connection.is_some())
            .then(b.strength.cmp(&a.strength))
            .then(b.security.secure().cmp(&a.security.secure()))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out.truncate(MAX_VISIBLE_NETWORKS);
    out
}

#[cfg(test)]
mod wifi_tests {
    use super::*;

    fn ap(path: &str, ssid: &str, strength: u8, rsn: u32) -> AccessPoint {
        AccessPoint {
            path: path.into(),
            ssid: ssid.as_bytes().to_vec(),
            strength,
            flags: if rsn != 0 { 1 } else { 0 },
            wpa_flags: 0,
            rsn_flags: rsn,
            mode: 2,
        }
    }

    #[test]
    fn signal_icons_follow_gnome() {
        assert_eq!(signal_icon(10), "network-wireless-signal-none-symbolic");
        assert_eq!(signal_icon(20), "network-wireless-signal-weak-symbolic");
        assert_eq!(signal_icon(45), "network-wireless-signal-ok-symbolic");
        assert_eq!(signal_icon(79), "network-wireless-signal-good-symbolic");
        assert_eq!(
            signal_icon(80),
            "network-wireless-signal-excellent-symbolic"
        );
    }

    #[test]
    fn security_follows_nm_flags() {
        assert_eq!(WifiSecurity::of(0, 0, 0), WifiSecurity::Open);
        assert_eq!(WifiSecurity::of(1, 0, 0), WifiSecurity::Personal); // WEP
        assert_eq!(WifiSecurity::of(1, 0, 0x188), WifiSecurity::Personal); // WPA2-PSK
        assert_eq!(WifiSecurity::of(1, 0, 0x288), WifiSecurity::Enterprise);
        assert_eq!(WifiSecurity::of(0, 0, 0x888), WifiSecurity::Open); // OWE
    }

    #[test]
    fn networks_group_and_sort_like_gnome() {
        let aps = vec![
            ap("/ap/1", "Cafe", 40, 0),
            ap("/ap/2", "Home", 30, 0x188),
            ap("/ap/3", "Home", 70, 0x188),
            ap("/ap/4", "Neighbour", 90, 0x188),
            ap("/ap/5", "", 99, 0),
            ap("/ap/6", "Library", 40, 0x188),
        ];
        let known = vec![(b"Home".to_vec(), "/conn/7".to_string())];
        let nets = wifi_networks(&aps, Some("/ap/2"), &known);
        let names: Vec<_> = nets.iter().map(|n| n.name.as_str()).collect();
        // Saved first, then strength, then secure before open at equal
        // strength; the hidden SSID is skipped.
        assert_eq!(names, ["Home", "Neighbour", "Library", "Cafe"]);
        let home = &nets[0];
        assert!(home.active, "any of its access points counts");
        assert_eq!(home.ap, "/ap/3", "the strongest access point");
        assert_eq!(home.strength, 70);
        assert_eq!(home.connection.as_deref(), Some("/conn/7"));
        assert!(!nets[3].security.secure());
    }

    #[test]
    fn at_most_eight_networks_show() {
        let aps: Vec<_> = (0..12)
            .map(|i| ap(&format!("/ap/{i}"), &format!("Net {i:02}"), 50, 0))
            .collect();
        assert_eq!(wifi_networks(&aps, None, &[]).len(), MAX_VISIBLE_NETWORKS);
    }
}

/// One BlueZ device (org.bluez.Device1) as GNOME's menu sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtDevice {
    pub path: String,
    pub alias: String,
    pub icon: Option<String>,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
}

/// GNOME's device list (bluetooth.js): paired or trusted devices,
/// connected first, then by name; none while the adapter is off.
pub fn bt_devices(devices: &[BtDevice], powered: bool) -> Vec<BtDevice> {
    if !powered {
        return Vec::new();
    }
    let mut out: Vec<BtDevice> = devices
        .iter()
        .filter(|d| d.paired || d.trusted)
        .cloned()
        .collect();
    out.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(a.alias.to_lowercase().cmp(&b.alias.to_lowercase()))
    });
    out
}

/// The Bluetooth toggle's subtitle: the one connected device's name,
/// "N Connected", or none.
pub fn bt_subtitle(devices: &[BtDevice]) -> Option<String> {
    let connected: Vec<&BtDevice> = devices.iter().filter(|d| d.connected).collect();
    match connected.as_slice() {
        [] => None,
        [one] => Some(one.alias.clone()),
        many => Some(format!("{} Connected", many.len())),
    }
}

/// A device's symbolic icon from BlueZ's Icon name.
pub fn bt_icon(icon: Option<&str>) -> String {
    match icon {
        Some(name) if name.ends_with("-symbolic") => name.to_owned(),
        Some(name) if !name.is_empty() => format!("{name}-symbolic"),
        _ => "bluetooth-active-symbolic".to_owned(),
    }
}

#[cfg(test)]
mod bt_tests {
    use super::*;

    fn dev(alias: &str, paired: bool, connected: bool) -> BtDevice {
        BtDevice {
            path: format!("/org/bluez/hci0/{alias}"),
            alias: alias.into(),
            icon: Some("audio-headset".into()),
            paired,
            trusted: false,
            connected,
        }
    }

    #[test]
    fn devices_filter_and_sort_like_gnome() {
        let all = vec![
            dev("Speaker", true, false),
            dev("headphones", true, true),
            dev("Stranger", false, false),
            dev("Keyboard", true, false),
        ];
        let names: Vec<_> = bt_devices(&all, true)
            .into_iter()
            .map(|d| d.alias)
            .collect();
        assert_eq!(names, ["headphones", "Keyboard", "Speaker"]);
        assert!(bt_devices(&all, false).is_empty(), "nothing while off");
    }

    #[test]
    fn subtitle_and_icon_follow_gnome() {
        assert_eq!(bt_subtitle(&[dev("A", true, false)]), None);
        assert_eq!(bt_subtitle(&[dev("A", true, true)]).as_deref(), Some("A"));
        assert_eq!(
            bt_subtitle(&[dev("A", true, true), dev("B", true, true)]).as_deref(),
            Some("2 Connected")
        );
        assert_eq!(bt_icon(Some("audio-headset")), "audio-headset-symbolic");
        assert_eq!(bt_icon(None), "bluetooth-active-symbolic");
    }
}

pub fn sink_icon(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.contains("headphone") || lower.contains("headset") {
        "audio-headphones-symbolic"
    } else if lower.contains("hdmi") || lower.contains("displayport") {
        "video-display-symbolic"
    } else {
        "audio-speakers-symbolic"
    }
}

/// GNOME's icon grid modes (iconGrid.js `defaultGridModes`) as
/// (columns, rows): the one whose shape is closest to the grid's area.
pub fn grid_mode(width: i32, height: i32) -> (usize, usize) {
    const MODES: [(usize, usize); 4] = [(3, 8), (4, 6), (6, 4), (8, 3)];
    let ratio = f64::from(width.max(1)) / f64::from(height.max(1));
    let mut best = MODES[3];
    let mut closest = f64::INFINITY;
    for (columns, rows) in MODES {
        let mode = columns as f64 / rows as f64;
        if (ratio - mode).abs() < (ratio - closest).abs() {
            closest = mode;
            best = (columns, rows);
        }
    }
    best
}

#[cfg(test)]
mod grid_mode_tests {
    use super::*;

    #[test]
    fn grid_modes_follow_the_area_like_gnome() {
        // 1280x800: the grid between search and dash is wide.
        assert_eq!(grid_mode(1280, 550), (8, 3));
        assert_eq!(grid_mode(1920, 830), (8, 3));
        // 4:3 screens get six columns of four.
        assert_eq!(grid_mode(1024, 600), (6, 4));
        // Portrait.
        assert_eq!(grid_mode(800, 1100), (4, 6));
        assert_eq!(grid_mode(600, 1600), (3, 8));
    }
}

/// A saved connection a wired device can use (NM `AvailableConnections`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiredConnection {
    pub path: String,
    /// `connection.id`.
    pub name: String,
    /// `connection.timestamp`: when it was last up, 0 if never.
    pub timestamp: u64,
}

/// One NetworkManager ethernet device as GNOME's wired toggle sees it
/// (network.js `NMWiredDeviceItem`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WiredDevice {
    pub path: String,
    /// NetworkManager device Interface, used to distinguish several adapters.
    pub interface: String,
    /// `NMDeviceState`.
    pub state: u32,
    /// The device's usable connections, any order.
    pub connections: Vec<WiredConnection>,
    /// The connection its active connection runs, and that active
    /// connection's `NMActiveConnectionState`.
    pub active: Option<(String, u32)>,
    /// Its active connection is NetworkManager's primary connection.
    pub primary: bool,
}

/// `NMActiveConnectionState` values GNOME's wired item tells apart.
const AC_ACTIVATING: u32 = 1;
const AC_ACTIVATED: u32 = 2;
const AC_DEACTIVATED: u32 = 4;
/// `NMConnectivityState` FULL.
const CONNECTIVITY_FULL: u32 = 4;

impl WiredDevice {
    /// `NMMenuItem.state`: the active connection's, else DEACTIVATED.
    fn ac_state(&self) -> u32 {
        self.active.as_ref().map_or(AC_DEACTIVATED, |(_, s)| *s)
    }

    /// `is_active`: activating or up.
    pub fn is_active(&self) -> bool {
        self.ac_state() <= AC_ACTIVATED
    }

    /// GNOME lists a device only in these states (`_shouldShowDevice`):
    /// not unmanaged, not unavailable (no cable).
    pub fn shown(&self) -> bool {
        matches!(
            self.state,
            30 | 40 | 50 | 60 | 70 | 80 | 90 | 100 | 110 | 120
        )
    }

    /// `NMWiredDeviceItem.icon_name`, given NetworkManager's
    /// `Connectivity`.
    pub fn icon(&self, connectivity: u32) -> &'static str {
        match self.ac_state() {
            AC_ACTIVATING => "network-wired-acquiring-symbolic",
            AC_ACTIVATED if !self.primary || connectivity == CONNECTIVITY_FULL => {
                "network-wired-symbolic"
            }
            AC_ACTIVATED => "network-wired-no-route-symbolic",
            _ => "network-wired-disconnected-symbolic",
        }
    }

    /// The connections by name (`ItemSorter`'s order).
    fn sorted(&self) -> Vec<&WiredConnection> {
        let mut c: Vec<&WiredConnection> = self.connections.iter().collect();
        c.sort_by(|a, b| a.name.cmp(&b.name));
        c
    }

    /// `_getActivatableItem`: the most recently used connection, else
    /// the first by name; `None` means connect automatically.
    fn activatable(&self) -> Option<&WiredConnection> {
        self.connections
            .iter()
            .filter(|c| c.timestamp > 0)
            .max_by_key(|c| c.timestamp)
            .or_else(|| self.sorted().into_iter().next())
    }

    /// What activating the device does (`NMDeviceItem.activate`).
    pub fn click(&self) -> WiredAction {
        if self.active.is_some() {
            WiredAction::Disconnect
        } else {
            match self.activatable() {
                Some(c) => WiredAction::Activate(c.path.clone()),
                None => WiredAction::AutoConnect,
            }
        }
    }

    /// The device's menu rows (`NMDeviceItem._sync`): one profile is a
    /// single row named for the device; several are radio rows plus
    /// Turn Off while up; none is a Connect row.
    pub fn menu(&self, device_name: &str) -> Vec<WiredRow> {
        let active = self.active.as_ref().map(|(c, _)| c.as_str());
        match self.connections.len() {
            0 => vec![WiredRow::AutoConnect {
                label: device_name.to_owned(),
            }],
            1 => vec![WiredRow::Single {
                label: device_name.to_owned(),
                active: self.is_active(),
                action: self.click(),
            }],
            _ => {
                let mut rows: Vec<WiredRow> = self
                    .sorted()
                    .into_iter()
                    .map(|c| {
                        let on = active == Some(c.path.as_str()) && self.is_active();
                        WiredRow::Radio {
                            label: c.name.clone(),
                            active: on,
                            // Radio mode only activates.
                            action: (!on).then(|| WiredAction::Activate(c.path.clone())),
                        }
                    })
                    .collect();
                if self.is_active() {
                    rows.push(WiredRow::TurnOff);
                }
                rows
            }
        }
    }
}

/// What a wired click asks NetworkManager for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WiredAction {
    /// `Device.Disconnect`.
    Disconnect,
    /// `ActivateConnection` of this saved connection on the device.
    Activate(String),
    /// `AddAndActivateConnection` of an empty connection (`_autoConnect`).
    AutoConnect,
}

/// One row of GNOME's wired menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WiredRow {
    /// No profile yet: the device's name, connecting automatically.
    AutoConnect { label: String },
    /// One profile: the device's name with a Connect/Disconnect subtitle.
    Single {
        label: String,
        active: bool,
        action: WiredAction,
    },
    /// Several profiles: each by name, a dot on the active one.
    Radio {
        label: String,
        active: bool,
        action: Option<WiredAction>,
    },
    /// Disconnect, under radio rows while up.
    TurnOff,
}

/// GNOME's wired toggle (`NMWiredToggle`): shown with any listed
/// device; checked while one is up; its icon is the primary device's
/// (the first active one, else the most recently used); its subtitle
/// is "N connected" with several up, else none (the device's name
/// equals the title).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiredToggle {
    pub visible: bool,
    pub checked: bool,
    pub icon: &'static str,
    pub subtitle: Option<String>,
}

pub fn wired_toggle(devices: &[WiredDevice], connectivity: u32) -> WiredToggle {
    let shown: Vec<&WiredDevice> = devices.iter().filter(|d| d.shown()).collect();
    let active: Vec<&&WiredDevice> = shown.iter().filter(|d| d.is_active()).collect();
    let primary = wired_primary(devices);
    WiredToggle {
        visible: !shown.is_empty(),
        checked: !active.is_empty(),
        icon: primary.map_or("network-wired-disconnected-symbolic", |d| {
            d.icon(connectivity)
        }),
        subtitle: (active.len() > 1).then(|| format!("{} connected", active.len())),
    }
}

/// `NMToggle._getPrimaryItem`: the first active listed device, else
/// the most recently used, else the first.
pub fn wired_primary(devices: &[WiredDevice]) -> Option<&WiredDevice> {
    let shown: Vec<&WiredDevice> = devices.iter().filter(|d| d.shown()).collect();
    shown.iter().copied().find(|d| d.is_active()).or_else(|| {
        shown
            .iter()
            .copied()
            .filter(|d| d.connections.iter().any(|c| c.timestamp > 0))
            .max_by_key(|d| d.connections.iter().map(|c| c.timestamp).max())
            .or_else(|| shown.first().copied())
    })
}

/// A toggle click (`NMToggle.activate`): every active device goes down;
/// with none up, the primary device comes up.
pub fn wired_click(devices: &[WiredDevice]) -> Vec<(String, WiredAction)> {
    let active: Vec<(String, WiredAction)> = devices
        .iter()
        .filter(|d| d.shown() && d.is_active())
        .map(|d| (d.path.clone(), WiredAction::Disconnect))
        .collect();
    if !active.is_empty() {
        return active;
    }
    wired_primary(devices)
        .map(|d| vec![(d.path.clone(), d.click())])
        .unwrap_or_default()
}

#[cfg(test)]
mod wired_tests {
    use super::*;

    fn conn(path: &str, name: &str, timestamp: u64) -> WiredConnection {
        WiredConnection {
            path: path.into(),
            name: name.into(),
            timestamp,
        }
    }

    #[test]
    fn wired_toggle_and_menu_follow_gnome() {
        // A plugged-in device with no profile: shown, off, Connect row.
        let mut dev = WiredDevice {
            path: "/d/1".into(),
            state: 30,
            ..Default::default()
        };
        let t = wired_toggle(std::slice::from_ref(&dev), 4);
        assert!(t.visible && !t.checked);
        assert_eq!(t.icon, "network-wired-disconnected-symbolic");
        assert_eq!(dev.click(), WiredAction::AutoConnect);
        assert_eq!(
            dev.menu("Wired"),
            [WiredRow::AutoConnect {
                label: "Wired".into()
            }]
        );
        // No cable (unavailable): hidden.
        dev.state = 20;
        assert!(!wired_toggle(std::slice::from_ref(&dev), 4).visible);

        // One profile, up as the primary connection without internet.
        dev.state = 100;
        dev.connections = vec![conn("/c/1", "Wired connection 1", 5)];
        dev.active = Some(("/c/1".into(), 2));
        dev.primary = true;
        let t = wired_toggle(std::slice::from_ref(&dev), 3);
        assert!(t.checked);
        assert_eq!(t.subtitle, None);
        assert_eq!(t.icon, "network-wired-no-route-symbolic");
        assert_eq!(dev.icon(4), "network-wired-symbolic");
        assert_eq!(dev.click(), WiredAction::Disconnect);
        assert_eq!(
            dev.menu("Wired"),
            [WiredRow::Single {
                label: "Wired".into(),
                active: true,
                action: WiredAction::Disconnect
            }]
        );

        // Two profiles, down: the most recently used activates; radio
        // rows by name, no Turn Off.
        dev.connections = vec![conn("/c/2", "Office", 9), conn("/c/1", "Home", 5)];
        dev.active = None;
        dev.state = 30;
        assert_eq!(dev.click(), WiredAction::Activate("/c/2".into()));
        let rows = dev.menu("Wired");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            WiredRow::Radio {
                label: "Home".into(),
                active: false,
                action: Some(WiredAction::Activate("/c/1".into()))
            }
        );
        // Up on Home: a dot on it, Turn Off last.
        dev.active = Some(("/c/1".into(), 1));
        let rows = dev.menu("Wired");
        assert_eq!(
            rows[0],
            WiredRow::Radio {
                label: "Home".into(),
                active: true,
                action: None
            }
        );
        assert_eq!(rows[2], WiredRow::TurnOff);
        assert_eq!(dev.icon(4), "network-wired-acquiring-symbolic");

        assert_eq!(
            wired_click(std::slice::from_ref(&dev)),
            [("/d/1".to_owned(), WiredAction::Disconnect)]
        );
        // Two devices up: "2 connected".
        let mut other = dev.clone();
        other.path = "/d/2".into();
        assert_eq!(
            wired_toggle(&[dev, other], 4).subtitle.as_deref(),
            Some("2 connected")
        );
    }
}

pub fn mic_icon(percent: f64, muted: bool) -> &'static str {
    if muted || percent <= 0.0 {
        return "microphone-sensitivity-muted-symbolic";
    }
    match (3.0 * percent / 100.0).ceil().clamp(1.0, 3.0) as u8 {
        1 => "microphone-sensitivity-low-symbolic",
        2 => "microphone-sensitivity-medium-symbolic",
        _ => "microphone-sensitivity-high-symbolic",
    }
}

#[cfg(test)]
mod mic_tests {
    use super::*;

    #[test]
    fn recording_shows_like_gnome() {
        assert_eq!(
            mic_icon(50.0, true),
            "microphone-sensitivity-muted-symbolic"
        );
        assert_eq!(mic_icon(20.0, false), "microphone-sensitivity-low-symbolic");
        assert_eq!(
            mic_icon(50.0, false),
            "microphone-sensitivity-medium-symbolic"
        );
        assert_eq!(
            mic_icon(100.0, false),
            "microphone-sensitivity-high-symbolic"
        );
    }
}

/// Where a drop on an app-grid tile lands (iconGrid.js `DragLocation`):
/// within 20px of either side it is between tiles, else on the tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropEdge {
    Start,
    OnIcon,
    End,
}

/// GNOME's `LEFT_DIVIDER_LEEWAY` and `RIGHT_DIVIDER_LEEWAY`.
const DIVIDER_LEEWAY: f64 = 20.0;

pub fn drop_edge(x: f64, width: f64) -> DropEdge {
    if x < DIVIDER_LEEWAY {
        DropEdge::Start
    } else if x > width - DIVIDER_LEEWAY {
        DropEdge::End
    } else {
        DropEdge::OnIcon
    }
}

/// The grid's order (appDisplay.js `_compareItems`): items the saved
/// `app-picker-layout` places come first by page and position, the rest
/// by name. `items` are (id, name); returns indices into it.
pub fn grid_order(items: &[(&str, &str)], layout: &[Vec<(String, i32)>]) -> Vec<usize> {
    let place = |id: &str| {
        layout.iter().enumerate().find_map(|(page, entries)| {
            entries
                .iter()
                .find(|(key, _)| key == id)
                .map(|(_, pos)| (page, *pos))
        })
    };
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| match (place(items[a].0), place(items[b].0)) {
        (None, None) => items[a].1.to_lowercase().cmp(&items[b].1.to_lowercase()),
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(pa), Some(pb)) => pa.cmp(&pb),
    });
    order
}

/// GNOME's drop between tiles (appDisplay.js `_getDropTarget` then
/// `_moveItem`): the dragged item takes the target's place, the others
/// reflowing away from where it came from; on an edge the reflow cannot
/// push, the neighbour in that direction is the target instead (not
/// past the row's ends). `None` when nothing moves.
pub fn grid_reorder(
    order: &[String],
    source: &str,
    target: usize,
    edge: DropEdge,
    columns: usize,
) -> Option<Vec<String>> {
    let from = order.iter().position(|id| id == source)?;
    if from == target || edge == DropEdge::OnIcon || target >= order.len() {
        return None;
    }
    let forward = from < target; // the reflow runs back toward `from`
    let column = target % columns.max(1);
    let mut to = target;
    if edge == DropEdge::Start && forward && column > 0 {
        to -= 1;
    } else if edge == DropEdge::End && !forward && column + 1 < columns {
        to += 1;
    }
    let mut out = order.to_vec();
    let item = out.remove(from);
    out.insert(to.min(out.len()), item);
    (out != order).then_some(out)
}

/// The order as `app-picker-layout` pages (appDisplay.js `_savePages`):
/// each item's position on its page.
pub fn grid_pages(order: &[String], per_page: usize) -> Vec<Vec<(String, i32)>> {
    order
        .chunks(per_page.max(1))
        .map(|page| {
            page.iter()
                .enumerate()
                .map(|(i, id)| (id.clone(), i as i32))
                .collect()
        })
        .collect()
}

/// A saved layout with `target` replaced by `folder` and `dragged`
/// gone (renumbered), as GNOME's `_createFolder` places a new folder
/// where the target was. `None` when there is no saved layout.
pub fn grid_pages_replace(
    pages: &[Vec<(String, i32)>],
    target: &str,
    folder: &str,
    dragged: &str,
) -> Option<Vec<Vec<(String, i32)>>> {
    if pages.iter().all(|p| p.is_empty()) {
        return None;
    }
    Some(
        pages
            .iter()
            .map(|page| {
                page.iter()
                    .filter(|(id, _)| id != dragged)
                    .map(|(id, _)| {
                        if id == target {
                            folder.to_owned()
                        } else {
                            id.clone()
                        }
                    })
                    .enumerate()
                    .map(|(i, id)| (id, i as i32))
                    .collect()
            })
            .collect(),
    )
}

#[cfg(test)]
mod grid_layout_tests {
    use super::*;

    #[test]
    fn grid_reorders_and_saves_like_gnome() {
        // The saved layout first, the rest by name.
        let items = [
            ("c", "Cheese"),
            ("a", "Abacus"),
            ("z", "Zebra"),
            ("b", "Boxes"),
        ];
        let layout = vec![vec![("z".to_owned(), 0), ("c".to_owned(), 1)]];
        assert_eq!(grid_order(&items, &layout), [2, 0, 1, 3]);
        assert_eq!(grid_order(&items, &[]), [1, 3, 0, 2]);

        let ids: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
        let ids_of = |v: Vec<String>| v.join("");
        // Dragged forward onto the start edge of "d": it lands before d.
        let moved = grid_reorder(&ids, "a", 3, DropEdge::Start, 8).unwrap();
        assert_eq!(ids_of(moved), "bcade");
        // Dragged back onto the end edge of "b": it lands after b.
        let moved = grid_reorder(&ids, "e", 1, DropEdge::End, 8).unwrap();
        assert_eq!(ids_of(moved), "abecd");
        // Back onto the start edge: before the target.
        assert_eq!(
            ids_of(grid_reorder(&ids, "e", 1, DropEdge::Start, 8).unwrap()),
            "aebcd"
        );
        // On the icon is a folder, not a move; onto itself nothing.
        assert_eq!(grid_reorder(&ids, "a", 3, DropEdge::OnIcon, 8), None);
        assert_eq!(grid_reorder(&ids, "c", 2, DropEdge::End, 8), None);
        // Pages of two.
        assert_eq!(
            grid_pages(&ids, 2),
            [
                vec![("a".to_owned(), 0), ("b".to_owned(), 1)],
                vec![("c".to_owned(), 0), ("d".to_owned(), 1)],
                vec![("e".to_owned(), 0)],
            ]
        );
        let saved = vec![vec![
            ("a".to_owned(), 0),
            ("b".to_owned(), 1),
            ("c".to_owned(), 2),
        ]];
        assert_eq!(
            grid_pages_replace(&saved, "b", "F", "a"),
            Some(vec![vec![("F".to_owned(), 0), ("c".to_owned(), 1)]])
        );
        assert_eq!(grid_pages_replace(&[], "b", "F", "a"), None);
        assert_eq!(drop_edge(10.0, 113.0), DropEdge::Start);
        assert_eq!(drop_edge(56.0, 113.0), DropEdge::OnIcon);
        assert_eq!(drop_edge(100.0, 113.0), DropEdge::End);
    }
}
