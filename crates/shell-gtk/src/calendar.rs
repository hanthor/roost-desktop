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

        // Events card: "Today" and a placeholder without a calendar server.
        let events = gtk::Box::new(gtk::Orientation::Vertical, 0);
        events.add_css_class("events-button");
        let events_title = gtk::Label::new(Some("Today"));
        events_title.add_css_class("events-title");
        events_title.set_xalign(0.0);
        let placeholder = gtk::Label::new(Some("No Events"));
        placeholder.add_css_class("event-placeholder");
        placeholder.set_xalign(0.0);
        events.append(&events_title);
        events.append(&placeholder);
        column.append(&events);

        let ui = Rc::new(Self {
            column,
            today_button,
            day_label,
            date_label,
            month_label,
            grid,
            events_title,
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
        self.events_title.set_text(&if selected == today {
            "Today".to_owned()
        } else {
            selected.strftime("%A, %B %-d").to_string()
        });

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
            button.update_property(&[gtk::accessible::Property::Label(
                &day.strftime("%A, %B %-d %Y").to_string(),
            )]);
            let ui = self.clone();
            button.connect_clicked(move |_| ui.select(day));
            self.grid
                .attach(&button, (i % 7) as i32, (i / 7) as i32 + 1, 1, 1);
        }
    }
}
