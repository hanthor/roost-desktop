//! Notifications in the GTK shell (#56): GNOME 51 banners, the list in
//! the calendar popover, Clear, and Do Not Disturb.
//!
//! The shell owns `org.freedesktop.Notifications` through the existing
//! toolkit-free daemon (`roost_shell_host::intake::NotificationBus`) and
//! store (`NotificationCenter`); this module only draws them. Banners
//! stack top-center under the top bar and expire after
//! [`BANNER_TIMEOUT`] unless critical; the list shows history newest
//! first; Do Not Disturb keeps banners away (critical ones still show)
//! while history keeps filling.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use roost_shell_host::intake::NotificationBus;
use roost_shell_host::notifications::{Notification, NotificationCenter, Urgency};

/// How long a normal or low banner stays up (GNOME: about 4 s).
pub const BANNER_TIMEOUT: Duration = Duration::from_secs(4);
/// GNOME's IDLE_TIME: idle longer than this when a banner shows means
/// the user is away, and the banner waits for them.
pub const AWAY_AFTER: Duration = Duration::from_secs(1);
/// How long a banner stays once an away user comes back.
pub const BACK_TIMEOUT: Duration = Duration::from_secs(2);

/// One banner's expiry, as GNOME's message tray times it: four seconds
/// when it appears to an active user; when the user is away it stays
/// until their next input and goes two seconds after it. `idle` is the
/// IdleMonitor's idle time, `None` when there is no idle monitor (then
/// the user counts as active).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BannerTimer {
    shown: Instant,
    deadline: Option<Instant>,
}

impl BannerTimer {
    pub fn new(now: Instant, idle: Option<Duration>) -> Self {
        let active = idle.is_none_or(|i| i <= AWAY_AFTER);
        Self {
            shown: now,
            deadline: active.then_some(now + BANNER_TIMEOUT),
        }
    }

    /// Whether the banner is due to go at `now`.
    pub fn expired(&mut self, now: Instant, idle: Option<Duration>) -> bool {
        if self.deadline.is_none() {
            match idle {
                // Input since the banner showed: the user is back.
                Some(i) if i < now.saturating_duration_since(self.shown) => {
                    self.deadline = Some(now.checked_sub(i).unwrap_or(now) + BACK_TIMEOUT);
                }
                Some(_) => {}
                None => self.deadline = Some(now + BACK_TIMEOUT),
            }
        }
        self.deadline.is_some_and(|d| now >= d)
    }
}

/// The session's idle time from org.gnome.Mutter.IdleMonitor, or `None`
/// without one.
fn idle_time() -> Option<Duration> {
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    let reply = conn
        .call_sync(
            Some("org.gnome.Mutter.IdleMonitor"),
            "/org/gnome/Mutter/IdleMonitor/Core",
            "org.gnome.Mutter.IdleMonitor",
            "GetIdletime",
            None,
            Some(glib::VariantTy::new("(t)").ok()?),
            gio::DBusCallFlags::NO_AUTO_START,
            200,
            gio::Cancellable::NONE,
        )
        .ok()?;
    reply.get::<(u64,)>().map(|(ms,)| Duration::from_millis(ms))
}
/// Close reason "dismissed by the user" (freedesktop spec).
const REASON_DISMISSED: u32 = 2;
/// Close reason "expired".
const REASON_EXPIRED: u32 = 1;

/// Snapshot of what the surfaces show, to rebuild only on change.
#[derive(Default, PartialEq)]
struct Shown {
    banners: Vec<u64>,
    history: Vec<u64>,
    dnd: bool,
}

/// Notification surfaces and their store.
pub struct NotifyUi {
    center: Arc<Mutex<NotificationCenter>>,
    bus: RefCell<NotificationBus>,
    banner_window: gtk::ApplicationWindow,
    banner_box: gtk::Box,
    /// The calendar popover's left pane.
    pane: gtk::Box,
    list: gtk::Box,
    empty: gtk::Box,
    clear: gtk::Button,
    /// GNOME binds Clear's visibility to the placeholder without
    /// SYNC_CREATE, so Clear shows (insensitive) until the list first
    /// changes; after that it hides whenever the list is empty.
    clear_bound: std::cell::Cell<bool>,
    first_seen: RefCell<HashMap<u64, BannerTimer>>,
    shown: RefCell<Shown>,
    last_bus_try: RefCell<Option<Instant>>,
}

/// One notification as GNOME 51 draws it (messageList.js
/// NotificationMessage): a header with the source icon, app name, time
/// and close button, then the title and body. Returns the card and its
/// close button.
fn card(n: &Notification) -> (gtk::Box, gtk::Button) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("message");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.add_css_class("message-header");
    // The app's own name and icon when its desktop entry is known, else
    // the sender's name and app_icon (notificationDaemon.js).
    let app = (!n.desktop_entry().is_empty())
        .then(|| desktop_app(n.desktop_entry()))
        .flatten();
    let (title, gicon): (String, Option<gio::Icon>) = match &app {
        Some(app) => (app.name.clone(), app.icon.as_deref().and_then(source_icon)),
        None => (n.app().to_owned(), source_icon(n.icon())),
    };
    let icon = gtk::Image::new();
    icon.add_css_class("message-source-icon");
    icon.set_pixel_size(16);
    match gicon {
        Some(gicon) => icon.set_from_gicon(&gicon),
        None => icon.set_visible(false),
    }
    header.append(&icon);
    let source = gtk::Label::new(Some(&title));
    source.add_css_class("message-source-title");
    header.append(&source);
    let age = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
        .saturating_sub(n.received());
    let time = gtk::Label::new(Some(&crate::logic::time_span(age)));
    time.add_css_class("event-time");
    time.set_hexpand(true);
    time.set_xalign(0.0);
    header.append(&time);
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("message-close-button");
    close.set_valign(gtk::Align::Center);
    close.update_property(&[gtk::accessible::Property::Label("Close")]);
    header.append(&close);
    card.append(&header);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.add_css_class("message-content");
    let summary = gtk::Label::new(Some(n.summary()));
    summary.add_css_class("message-title");
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    content.append(&summary);
    if !n.body().is_empty() {
        let body = gtk::Label::new(Some(n.body()));
        body.add_css_class("message-body");
        body.set_xalign(0.0);
        body.set_wrap(true);
        body.set_max_width_chars(48);
        content.append(&body);
    }
    card.append(&content);
    card.update_property(&[gtk::accessible::Property::Label(n.summary())]);
    (card, close)
}

/// The installed app a `desktop-entry` hint names, if any.
fn desktop_app(id: &str) -> Option<roost_shell_host::apps::AppEntry> {
    roost_shell_host::apps::default_app_dirs()
        .into_iter()
        .map(|dir| dir.join(format!("{id}.desktop")))
        .find(|path| path.is_file())
        .and_then(|path| roost_shell_host::apps::entry_from_file(&path))
}

/// A `Notify` app_icon as GNOME reads it: a file URI, an absolute path,
/// or an icon name.
fn source_icon(icon: &str) -> Option<gio::Icon> {
    if icon.is_empty() {
        None
    } else if icon.starts_with("file://") {
        Some(gio::FileIcon::new(&gio::File::for_uri(icon)).upcast())
    } else if icon.starts_with('/') {
        Some(gio::FileIcon::new(&gio::File::for_path(icon)).upcast())
    } else {
        Some(gio::ThemedIcon::new(icon).upcast())
    }
}

impl NotifyUi {
    /// Build the surfaces; the bus is claimed on the first tick.
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let center = Arc::new(Mutex::new(NotificationCenter::load_system()));
        let bus = NotificationBus::new(center.clone());

        let banner_window = gtk::ApplicationWindow::new(app);
        banner_window.add_css_class("roost-banners");
        banner_window.init_layer_shell();
        banner_window.set_layer(Layer::Overlay);
        banner_window.set_namespace(Some("roost-shell-banners"));
        banner_window.set_anchor(Edge::Top, true);
        // The card's own 4px margin puts it 4px under the bar (GNOME).
        banner_window.set_margin(Edge::Top, 0);
        banner_window.set_keyboard_mode(KeyboardMode::None);
        banner_window.set_title(Some("Notifications"));
        let banner_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        banner_window.set_child(Some(&banner_box));

        // Calendar pane, GNOME 51's CalendarMessageList: the placeholder
        // overlaid on the list, and a controls row holding only Clear (Do
        // Not Disturb lives in quick settings).
        let pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pane.add_css_class("message-list");
        let overlay = gtk::Overlay::new();
        overlay.set_vexpand(true);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("message-view");
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        column.append(&scroller);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        controls.add_css_class("message-list-controls");
        let clear = gtk::Button::with_label("Clear");
        clear.add_css_class("message-list-clear-button");
        clear.set_halign(gtk::Align::Start);
        clear.update_property(&[gtk::accessible::Property::Description(
            "Clear all notifications",
        )]);
        controls.append(&clear);
        column.append(&controls);
        overlay.set_child(Some(&column));
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 0);
        empty.add_css_class("message-list-placeholder");
        empty.set_halign(gtk::Align::Center);
        empty.set_valign(gtk::Align::Center);
        empty.set_can_target(false);
        let bell = gtk::Image::from_icon_name("no-notifications-symbolic");
        bell.set_pixel_size(96);
        let none = gtk::Label::new(Some("No Notifications"));
        empty.append(&bell);
        empty.append(&none);
        overlay.add_overlay(&empty);
        pane.append(&overlay);

        let ui = Rc::new(Self {
            center,
            bus: RefCell::new(bus),
            banner_window,
            banner_box,
            pane,
            list,
            empty,
            clear,
            clear_bound: std::cell::Cell::new(false),
            first_seen: RefCell::new(HashMap::new()),
            shown: RefCell::new(Shown {
                banners: vec![u64::MAX],
                ..Default::default()
            }),
            last_bus_try: RefCell::new(None),
        });
        {
            let me = ui.clone();
            ui.clear.connect_clicked(move |_| {
                if let Ok(mut center) = me.center.lock() {
                    center.clear_history();
                }
                me.refresh();
            });
        }
        ui.refresh();
        ui
    }

    /// Post the shell's own notification under a GNOME source name and
    /// icon (screenshots: "Screenshot", `screenshot-recorded-symbolic`).
    pub fn post(self: &Rc<Self>, app: &str, icon: &str, summary: &str, body: &str) {
        if let Ok(mut center) = self.center.lock() {
            let id = center.notify(app, summary, body, Vec::new(), Urgency::Normal, None);
            center.set_source(id, icon, "");
        }
        self.refresh();
    }

    /// The calendar popover's notification pane.
    pub fn pane(&self) -> &gtk::Box {
        &self.pane
    }

    /// Turn Do Not Disturb on or off (the quick-settings toggle).
    pub fn set_dnd(&self, on: bool) {
        if let Ok(mut center) = self.center.lock() {
            if center.dnd() != on {
                center.set_dnd(on);
            }
        }
    }

    /// Whether Do Not Disturb is on.
    pub fn dnd(&self) -> bool {
        self.center.lock().map(|c| c.dnd()).unwrap_or(false)
    }

    /// Periodic work: claim the bus, expire banners, redraw on change.
    pub fn tick(self: &Rc<Self>) {
        let retry = self
            .last_bus_try
            .borrow()
            .is_none_or(|t| t.elapsed() > Duration::from_secs(2));
        if retry {
            *self.last_bus_try.borrow_mut() = Some(Instant::now());
            self.bus.borrow_mut().ensure();
        }
        // Expire non-critical banners.
        let expired: Vec<u64> = {
            let Ok(center) = self.center.lock() else {
                return;
            };
            let mut seen = self.first_seen.borrow_mut();
            let now = Instant::now();
            let live: Vec<(u64, Urgency)> =
                center.banners().iter().map(|n| (n.id, n.urgency)).collect();
            seen.retain(|id, _| live.iter().any(|(l, _)| l == id));
            let timed: Vec<u64> = live
                .iter()
                .filter(|(_, u)| *u != Urgency::Critical)
                .map(|(id, _)| *id)
                .collect();
            // Ask for idle time only while a banner is timing.
            let idle = if timed.is_empty() { None } else { idle_time() };
            timed
                .into_iter()
                .filter(|id| {
                    seen.entry(*id)
                        .or_insert_with(|| BannerTimer::new(now, idle))
                        .expired(now, idle)
                })
                .collect()
        };
        for id in expired {
            let _ = self.bus.borrow().dismiss_banner(id as u32, REASON_EXPIRED);
        }
        self.refresh();
    }

    fn refresh(self: &Rc<Self>) {
        let Ok(center) = self.center.lock() else {
            return;
        };
        let now = Shown {
            banners: center.banners().iter().map(|n| n.id).collect(),
            history: center.history().iter().rev().map(|n| n.id).collect(),
            dnd: center.dnd(),
        };
        if *self.shown.borrow() == now {
            return;
        }
        // Banners.
        while let Some(child) = self.banner_box.first_child() {
            self.banner_box.remove(&child);
        }
        for n in center.banners() {
            let (c, close) = card(n);
            c.add_css_class("banner");
            let has_default = n.actions().iter().any(|a| a.id == "default");
            if n.actions().iter().any(|a| a.id != "default") {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                for action in n.actions().iter().filter(|a| a.id != "default") {
                    let button = gtk::Button::with_label(&action.label);
                    let (me, id, key) = (self.clone(), n.id, action.id.clone());
                    button.connect_clicked(move |_| {
                        let _ = me.bus.borrow().invoke_action(id as u32, &key);
                        me.refresh();
                    });
                    row.append(&button);
                }
                c.append(&row);
            }
            {
                let (me, id) = (self.clone(), n.id);
                close.connect_clicked(move |_| {
                    let _ = me.bus.borrow().dismiss_banner(id as u32, REASON_DISMISSED);
                    me.refresh();
                });
            }
            let click = gtk::GestureClick::new();
            {
                let (me, id) = (self.clone(), n.id);
                click.connect_released(move |_, _, _, _| {
                    let bus = me.bus.borrow();
                    let _ = if has_default {
                        bus.invoke_action(id as u32, "default")
                    } else {
                        bus.dismiss_banner(id as u32, REASON_DISMISSED)
                    };
                    drop(bus);
                    me.refresh();
                });
            }
            c.add_controller(click);
            self.banner_box.append(&c);
        }
        self.banner_window.set_visible(!now.banners.is_empty());

        // History list.
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for n in center.history().iter().rev() {
            let (c, close) = card(n);
            let (me, id) = (self.clone(), n.id);
            close.connect_clicked(move |_| {
                let _ = me.bus.borrow().dismiss_banner(id as u32, REASON_DISMISSED);
                if let Ok(mut center) = me.center.lock() {
                    center.remove(id);
                }
                me.refresh();
            });
            self.list.append(&c);
        }
        let any = !now.history.is_empty();
        self.list.set_visible(any);
        if self.empty.is_visible() == any {
            // The placeholder changes: from here on Clear follows it.
            self.clear_bound.set(true);
        }
        self.empty.set_visible(!any);
        self.clear.set_visible(!self.clear_bound.get() || any);
        self.clear.set_sensitive(any);
        drop(center);
        *self.shown.borrow_mut() = now;
    }
}

#[cfg(test)]
mod timer_tests {
    use super::*;

    #[test]
    fn banners_time_out_like_gnome() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        // Shown to an active user: four seconds.
        let mut t = BannerTimer::new(t0, Some(Duration::from_millis(200)));
        assert!(!t.expired(t0 + Duration::from_millis(3900), Some(s(3))));
        assert!(t.expired(t0 + s(4), Some(s(4))));
        // Shown to an away user: stays while they stay away...
        let mut t = BannerTimer::new(t0, Some(s(30)));
        assert!(!t.expired(t0 + s(60), Some(s(90))));
        // ...and goes two seconds after they come back.
        assert!(!t.expired(t0 + s(61), Some(Duration::from_millis(500))));
        assert!(t.expired(t0 + Duration::from_millis(62_500), Some(s(2))));
        // No idle monitor: the user counts as active.
        let mut t = BannerTimer::new(t0, None);
        assert!(t.expired(t0 + s(4), None));
    }
}
