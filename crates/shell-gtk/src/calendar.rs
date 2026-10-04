//! GNOME 51's date menu calendar column (dateMenu.js, calendar.js): the
//! today button, a month grid of round day buttons and the events card.
//! Built to the geometry measured from GNOME Shell 51 (docs/gnome-parity.md).

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;
use jiff::civil::Date;

use crate::logic;

/// The locale's first weekday, 0 = Sunday, computed the way
/// gnome-shell's `shell_util_get_week_start` does from glibc.
pub fn week_start() -> i8 {
    #[cfg(target_env = "gnu")]
    {
        // glibc's _NL_TIME_WEEK_1STDAY and _NL_TIME_FIRST_WEEKDAY.
        const WEEK_1STDAY: libc::nl_item = 0x20066;
        const FIRST_WEEKDAY: libc::nl_item = 0x20068;
        // SAFETY: nl_langinfo returns a pointer into static locale data;
        // WEEK_1STDAY's "string" is really a 32-bit word, as glibc
        // documents and gnome-shell reads it.
        unsafe {
            let first = *libc::nl_langinfo(FIRST_WEEKDAY) as i8;
            let origin = libc::nl_langinfo(WEEK_1STDAY) as usize as u32;
            let week_1stday = match origin {
                19971130 => 0, // Sunday
                19971201 => 1, // Monday
                _ => 0,
            };
            return ((week_1stday + first - 1).rem_euclid(7)) as i8;
        }
    }
    #[allow(unreachable_code)]
    0
}

pub struct CalendarUi {
    pub column: gtk::Box,
    today_button: gtk::Button,
    day_label: gtk::Label,
    date_label: gtk::Label,
    month_label: gtk::Label,
    grid: gtk::Grid,
    events_title: gtk::Label,
    events_card: gtk::Button,
    events_list: gtk::Box,
    source: std::cell::RefCell<Option<Rc<crate::events::EventSource>>>,
    clock_format: Cell<logic::ClockFormat>,
    week_start: i8,
    today: Cell<Date>,
    shown: Cell<Date>,
    selected: Cell<Date>,
}

impl CalendarUi {
    pub fn new(today: Date) -> Rc<Self> {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
        column.add_css_class("datemenu-calendar-column");

        // Today button: weekday over the full date; insensitive while
        // today is selected.
        let today_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let day_label = gtk::Label::new(None);
        day_label.add_css_class("day-label");
        day_label.set_xalign(0.0);
        let date_label = gtk::Label::new(None);
        date_label.add_css_class("date-label");
        date_label.set_xalign(0.0);
        today_box.append(&day_label);
        today_box.append(&date_label);
        let today_button = gtk::Button::builder().child(&today_box).build();
        today_button.add_css_class("datemenu-today-button");
        column.append(&today_button);

        // Month header: back, month, forward.
        let calendar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        calendar.add_css_class("calendar");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("calendar-month-header");
        let back = gtk::Button::from_icon_name("pan-start-symbolic");
        back.add_css_class("pager-button");
        back.update_property(&[gtk::accessible::Property::Label("Previous month")]);
        let month_label = gtk::Label::new(None);
        month_label.add_css_class("calendar-month-label");
        month_label.set_hexpand(true);
        let forward = gtk::Button::from_icon_name("pan-end-symbolic");
        forward.add_css_class("pager-button");
        forward.update_property(&[gtk::accessible::Property::Label("Next month")]);
        header.append(&back);
        header.append(&month_label);
        header.append(&forward);
        calendar.append(&header);
        let grid = gtk::Grid::new();
        grid.set_halign(gtk::Align::Center);
        calendar.append(&grid);
        column.append(&calendar);

        // Events card (dateMenu.js EventsSection): the selected day's
        // title over its events, or "No Events"; shown while the
        // calendar server has calendars.
        let events = gtk::Button::new();
        events.update_property(&[gtk::accessible::Property::Label("Open Calendar")]);
        events.add_css_class("events-button");
        let events_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        events_box.add_css_class("events-box");
        let events_title = gtk::Label::new(Some("Today"));
        events_title.add_css_class("events-title");
        events_title.set_xalign(0.0);
        let events_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        events_list.add_css_class("events-list");
        events_box.append(&events_title);
        events_box.append(&events_list);
        events.set_child(Some(&events_box));
        column.append(&events);

        let ui = Rc::new(Self {
            column,
            today_button,
            day_label,
            date_label,
            month_label,
            grid,
            events_title,
            events_card: events,
            events_list,
            source: Default::default(),
            clock_format: Cell::new(logic::ClockFormat::TwentyFourHour),
            week_start: week_start(),
            today: Cell::new(today),
            shown: Cell::new(today.first_of_month()),
            selected: Cell::new(today),
        });
        {
            let ui2 = ui.clone();
            back.connect_clicked(move |_| ui2.page(-1));
        }
        {
            let ui2 = ui.clone();
            forward.connect_clicked(move |_| ui2.page(1));
        }
        {
            let ui2 = ui.clone();
            ui.today_button
                .connect_clicked(move |_| ui2.select(ui2.today.get()));
        }
        ui.render();
        ui
    }

    /// Launch the selected day through the same session-aware path as overview apps.
    pub fn connect_open(self: &Rc<Self>, close: impl Fn() + 'static) {
        let ui = Rc::downgrade(self);
        self.events_card.connect_clicked(move |_| {
            let Some(ui) = ui.upgrade() else { return };
            if launch_calendar(ui.selected.get()) {
                close();
            }
        });
    }

    /// Take events from GNOME's calendar server.
    pub fn set_event_source(self: &Rc<Self>, source: Rc<crate::events::EventSource>) {
        let ui = Rc::downgrade(self);
        source.connect_changed(move || {
            if let Some(ui) = ui.upgrade() {
                ui.render();
            }
        });
        *self.source.borrow_mut() = Some(source);
        self.render();
    }

    /// Event times follow GNOME's clock-format.
    pub fn set_clock_format(self: &Rc<Self>, format: logic::ClockFormat) {
        if self.clock_format.replace(format) != format {
            self.render();
        }
    }

    /// Follow the clock: a new day moves "today" (and the selection, if
    /// it was today).
    pub fn set_today(self: &Rc<Self>, today: Date) {
        let old = self.today.replace(today);
        if old == today {
            return;
        }
        if self.selected.get() == old {
            self.selected.set(today);
            self.shown.set(today.first_of_month());
        }
        self.render();
    }

    /// Back to today, as GNOME does each time the menu opens.
    pub fn reset(self: &Rc<Self>) {
        self.select(self.today.get());
    }

    fn select(self: &Rc<Self>, day: Date) {
        self.selected.set(day);
        self.shown.set(day.first_of_month());
        self.render();
    }

    fn page(self: &Rc<Self>, months: i64) {
        let shown = self.shown.get();
        if let Ok(next) = shown.checked_add(jiff::Span::new().months(months)) {
            self.shown.set(next.first_of_month());
            self.render();
        }
    }

    fn render(self: &Rc<Self>) {
        let (today, shown, selected) = (self.today.get(), self.shown.get(), self.selected.get());
        let (day, date) = logic::calendar_heading(&today.at(0, 0, 0, 0));
        self.day_label.set_text(&day);
        self.date_label.set_text(&date);
        self.today_button.set_sensitive(selected != today);
        self.month_label.set_text(&logic::month_label(shown, today));
        self.events_title
            .set_text(&logic::events_title(selected, today));
        let tz = jiff::tz::TimeZone::system();
        let source = self.source.borrow().clone();
        let grid_days = logic::month_grid(shown.year(), shown.month(), self.week_start);
        if let (Some(source), Some(first), Some(last)) =
            (&source, grid_days.first(), grid_days.last())
        {
            source.request_range(
                logic::day_bounds(*first, &tz).0,
                logic::day_bounds(*last, &tz).1,
            );
        }
        self.render_events(source.as_deref(), selected, today, &tz);

        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        for (col, initial) in logic::weekday_initials(self.week_start)
            .into_iter()
            .enumerate()
        {
            let heading = gtk::Label::new(Some(initial));
            heading.add_css_class("calendar-day-heading");
            self.grid.attach(&heading, col as i32, 0, 1, 1);
        }
        for (i, day) in logic::month_grid(shown.year(), shown.month(), self.week_start)
            .into_iter()
            .enumerate()
        {
            let button = gtk::Button::with_label(&day.strftime("%d").to_string());
            button.add_css_class("calendar-day");
            button.add_css_class(if logic::is_weekend(day) {
                "calendar-weekend"
            } else {
                "calendar-weekday"
            });
            if day.month() != shown.month() {
                button.add_css_class("calendar-other-month");
            }
            if day == today {
                button.add_css_class("calendar-today");
            } else if day == selected {
                button.add_css_class("selected");
            }
            if let Some(source) = &source {
                let (begin, end) = logic::day_bounds(day, &tz);
                if !source.events_between(begin, end).is_empty() {
                    button.add_css_class("calendar-day-with-events");
                }
            }
            button.update_property(&[gtk::accessible::Property::Label(
                &day.strftime("%A, %B %-d %Y").to_string(),
            )]);
            let ui = self.clone();
            button.connect_clicked(move |_| ui.select(day));
            self.grid
                .attach(&button, (i % 7) as i32, (i / 7) as i32 + 1, 1, 1);
        }
    }

    /// The events card: hidden without calendars (GNOME), else the
    /// selected day's events or "No Events".
    fn render_events(
        &self,
        source: Option<&crate::events::EventSource>,
        selected: Date,
        today: Date,
        tz: &jiff::tz::TimeZone,
    ) {
        self.events_card
            .set_visible(source.is_some_and(|s| s.has_calendars()));
        while let Some(child) = self.events_list.first_child() {
            self.events_list.remove(&child);
        }
        let (begin, end) = logic::day_bounds(selected, tz);
        let events = source
            .map(|s| s.events_between(begin, end))
            .unwrap_or_default();
        for event in &events {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 6);
            row.add_css_class("event-box");
            let summary = gtk::Label::new(Some(&event.summary));
            summary.add_css_class("event-summary");
            summary.set_xalign(0.0);
            summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let time = gtk::Label::new(Some(&logic::event_time_text(
                event,
                selected,
                tz,
                self.clock_format.get(),
                today.year(),
            )));
            time.add_css_class("event-time");
            time.set_xalign(0.0);
            row.append(&summary);
            row.append(&time);
            self.events_list.append(&row);
        }
        if events.is_empty() {
            let placeholder = gtk::Label::new(Some("No Events"));
            placeholder.add_css_class("event-placeholder");
            placeholder.set_xalign(0.0);
            self.events_list.append(&placeholder);
        }
    }
}

/// Resolve the preferred calendar handler; an absent handler is a quiet no-op.
fn launch_calendar(day: Date) -> bool {
    let info = gio::AppInfo::default_for_type("x-scheme-handler/calendar", false)
        .or_else(|| gio::AppInfo::default_for_type("text/calendar", false));
    let id = info
        .as_ref()
        .and_then(|info| info.id())
        .unwrap_or_else(|| "org.gnome.Calendar.desktop".into());
    let Some(mut entry) = roost_shell_host::apps::default_app_dirs()
        .iter()
        .find_map(|dir| roost_shell_host::apps::entry_from_file(&dir.join(id.as_str())))
    else {
        return false;
    };
    if entry.app_id == "org.gnome.Calendar.desktop" {
        entry.argv.push("--date".into());
        entry.argv.push(day.to_string().into());
    } else if info.as_ref().is_some_and(|info| info.supports_uris()) {
        entry.argv.push(format!("calendar:///{}", day).into());
    }
    roost_shell_host::apps::launch(&entry).is_ok()
}
