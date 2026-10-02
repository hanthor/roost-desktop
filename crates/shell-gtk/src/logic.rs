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

/// One brightness-key step from `percent`: GNOME's scale has twenty
/// steps, and a press lands on the next one up or down.
pub fn brightness_step(percent: f64, up: bool) -> f64 {
    let step = 100.0 / 20.0;
    let index = (percent / step).round();
    let next = if up { index + 1.0 } else { index - 1.0 };
    (next * step).clamp(0.0, 100.0)
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
