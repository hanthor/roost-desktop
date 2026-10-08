//! Notifications in the GTK shell (#56): GNOME 51 banners, the list in
//! the calendar popover, Clear, and Do Not Disturb.
//!
//! The shell owns `org.freedesktop.Notifications` through the existing
//! toolkit-free daemon (`roost_shell_host::intake::NotificationBus`) and
//! store (`NotificationCenter`); this module only draws them. Banners
//! stack top-center under the top bar and expire after
//! [`BANNER_TIMEOUT`] unless critical; the list shows history newest
//! first, one group per app (GNOME 51's NotificationMessageGroup: a
//! collapsed stack that expands on click); Do Not Disturb keeps banners
//! away (critical ones still show) while history keeps filling.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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

/// How far the second card of a collapsed group peeks below the first
/// (messageList.js HEIGHT_OFFSET_STACKED).
pub const HEIGHT_OFFSET_STACKED: f64 = 10.0;
/// Each further card peeks this many times less
/// (HEIGHT_OFFSET_REDUCTION_STACKED).
pub const HEIGHT_OFFSET_REDUCTION_STACKED: f64 = 1.4;
/// Each card down the stack is inset this much more per side
/// (WIDTH_OFFSET_STACKED).
pub const WIDTH_OFFSET_STACKED: i32 = 6;
/// Cards a collapsed group shows, the top one included
/// (MAX_VISIBLE_STACKED_MESSAGES).
pub const MAX_VISIBLE_STACKED_MESSAGES: usize = 3;

/// The message list's groups, as GNOME 51 forms them: one per source,
/// ordered by each group's newest notification, members newest first.
/// `newest_first` is the history, newest first, with each notification's
/// source key.
pub fn group_ids<K: PartialEq>(
    newest_first: impl IntoIterator<Item = (u64, K)>,
) -> Vec<(K, Vec<u64>)> {
    let mut groups: Vec<(K, Vec<u64>)> = Vec::new();
    for (id, key) in newest_first {
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, ids)) => ids.push(id),
            None => groups.push((key, vec![id])),
        }
    }
    groups
}

/// The cards peeking under the top one of a collapsed group of `count`:
/// for each, its inset per side and how far its bottom edge reaches
/// below the top card's (10 px, then 10/1.4 px more; at most
/// [`MAX_VISIBLE_STACKED_MESSAGES`] cards in all).
pub fn stack_layout(count: usize) -> Vec<(i32, i32)> {
    let mut reach = 0.0_f64;
    let mut step = HEIGHT_OFFSET_STACKED;
    (1..count.min(MAX_VISIBLE_STACKED_MESSAGES))
        .map(|k| {
            reach += step;
            step /= HEIGHT_OFFSET_REDUCTION_STACKED;
            (WIDTH_OFFSET_STACKED * k as i32, reach.round() as i32)
        })
        .collect()
}

/// The source a notification groups under: its desktop entry when it
/// names one, else the sender's app name (GNOME's per-app sources).
fn group_key(n: &Notification) -> String {
    if n.desktop_entry().is_empty() {
        n.app().to_owned()
    } else {
        n.desktop_entry().to_owned()
    }
}

/// Snapshot of what the surfaces show, to rebuild only on change.
#[derive(Default, PartialEq)]
struct Shown {
    banners: Vec<u64>,
    history: Vec<u64>,
    expanded: Vec<String>,
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
    /// The groups the user expanded, by source key.
    expanded: RefCell<HashSet<String>>,
    group_widgets: RefCell<HashMap<String, Rc<crate::group_animation::Group>>>,
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
    let (title, gicon) = source_of(n);
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

/// A notification's source name and icon: the app's own when its
/// desktop entry is known, else the sender's name and app_icon
/// (notificationDaemon.js).
fn source_of(n: &Notification) -> (String, Option<gio::Icon>) {
    let app = (!n.desktop_entry().is_empty())
        .then(|| desktop_app(n.desktop_entry()))
        .flatten();
    match app {
        Some(app) => (app.name.clone(), app.icon.as_deref().and_then(source_icon)),
        None => (n.app().to_owned(), source_icon(n.icon())),
    }
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
        banner_window.set_namespace(Some(roost_shell_control::GTK_BANNERS_NAMESPACE));
        banner_window.set_anchor(Edge::Top, true);
        // The card's own 4px margin puts it 4px under the bar (GNOME).
        banner_window.set_margin(Edge::Top, 0);
        banner_window.set_keyboard_mode(KeyboardMode::None);
        let keys = gtk::EventControllerKey::new();
        let banner_weak = banner_window.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(window) = banner_weak.upgrade() {
                    window.set_keyboard_mode(KeyboardMode::None);
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        banner_window.add_controller(keys);
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
            expanded: RefCell::new(HashSet::new()),
            group_widgets: RefCell::new(HashMap::new()),
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

    /// GNOME's Super+N gives the current banner keyboard focus.
    pub fn focus_active(&self) {
        if !self.banner_window.is_visible() {
            return;
        }
        self.banner_window
            .set_keyboard_mode(KeyboardMode::Exclusive);
        self.banner_window.present();
        self.banner_window.set_focus_visible(true);
        self.banner_box.child_focus(gtk::DirectionType::TabForward);
    }

    /// A locked session must not keep the banner's exclusive keyboard grab.
    pub fn release_focus(&self) {
        self.banner_window.set_keyboard_mode(KeyboardMode::None);
        self.banner_window.set_focus_visible(false);
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
        let groups = group_ids(center.history().iter().rev().map(|n| (n.id, group_key(n))));
        // A group that falls back to one card (or goes) forgets it was
        // expanded.
        self.expanded
            .borrow_mut()
            .retain(|k| groups.iter().any(|(g, ids)| g == k && ids.len() > 1));
        let mut expanded: Vec<String> = self.expanded.borrow().iter().cloned().collect();
        expanded.sort();
        let now = Shown {
            banners: center.banners().iter().map(|n| n.id).collect(),
            history: center.history().iter().rev().map(|n| n.id).collect(),
            expanded,
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
        if now.banners.is_empty() {
            self.banner_window.set_keyboard_mode(KeyboardMode::None);
        }
        self.banner_window.set_visible(!now.banners.is_empty());

        // History list.
        self.group_widgets.borrow_mut().clear();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let by_id: HashMap<u64, &Notification> =
            center.history().iter().map(|n| (n.id, n)).collect();
        for (key, ids) in &groups {
            let members: Vec<&Notification> =
                ids.iter().filter_map(|id| by_id.get(id).copied()).collect();
            match members.as_slice() {
                [] => {}
                // A group of one is just its card.
                [n] => {
                    let (c, close) = card(n);
                    let (me, id) = (self.clone(), n.id);
                    close.connect_clicked(move |_| me.remove(&[id]));
                    self.list.append(&c);
                }
                _ => {
                    let group = crate::group_animation::Group::new(
                        &self.collapsed_group(key, &members),
                        &self.expanded_group(key, &members),
                        now.expanded.contains(key),
                    );
                    self.list.append(&group.widget);
                    self.group_widgets.borrow_mut().insert(key.clone(), group);
                }
            }
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

    /// Close notifications from the list (and any banner they hold).
    fn remove(self: &Rc<Self>, ids: &[u64]) {
        for &id in ids {
            let _ = self
                .bus
                .borrow()
                .dismiss_banner(id as u32, REASON_DISMISSED);
            if let Ok(mut center) = self.center.lock() {
                center.remove(id);
            }
        }
        self.refresh();
    }

    fn set_expanded(self: &Rc<Self>, key: &str, on: bool) {
        if on {
            self.expanded.borrow_mut().insert(key.to_owned());
        } else {
            self.expanded.borrow_mut().remove(key);
        }
        let group = self.group_widgets.borrow().get(key).cloned();
        if let Some(group) = group {
            group.set_expanded(on);
            // Prevent the refresh ticker rebuilding the group mid-animation.
            let mut expanded: Vec<_> = self.expanded.borrow().iter().cloned().collect();
            expanded.sort();
            self.shown.borrow_mut().expanded = expanded;
        } else {
            self.refresh();
        }
    }

    /// A collapsed group (2+ notifications), as GNOME 51 stacks it: the
    /// newest card in full, the next ones peeking out below it, each
    /// inset more and darker. Clicking expands it; closing the top card
    /// closes the whole group.
    fn collapsed_group(self: &Rc<Self>, key: &str, members: &[&Notification]) -> gtk::Button {
        let (top, close) = card(members[0]);
        let layout = stack_layout(members.len());
        let reveal = layout.last().map_or(0, |&(_, reach)| reach);
        top.set_margin_bottom(reveal);
        // The top card is the overlay's main child, so it sizes the group
        // and gets its height for the list's width (a measured overlay is
        // allocated at least its height for its minimum width, which a
        // wrapping card overshoots). The peeking cards fill the overlay
        // above the card's margin, lowest first.
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&top));
        for (k, &(inset, reach)) in layout.iter().enumerate().rev() {
            let stub = gtk::Box::new(gtk::Orientation::Vertical, 0);
            stub.add_css_class("message-stack-card");
            stub.add_css_class(if k == 0 {
                "second-in-stack"
            } else {
                "lower-in-stack"
            });
            stub.set_margin_start(inset);
            stub.set_margin_end(inset);
            stub.set_margin_bottom(reveal - reach);
            stub.set_can_target(false);
            overlay.add_overlay(&stub);
        }
        // Overlays draw over the main child; moving the card to the end
        // of the overlay's children draws it over the stack instead
        // (layout still treats it as the main child).
        top.insert_before(&overlay, gtk::Widget::NONE);

        let group = gtk::Button::new();
        group.set_child(Some(&overlay));
        group.add_css_class("message-group");
        group.add_css_class("collapsed");
        let (title, _) = source_of(members[0]);
        group.update_property(&[
            gtk::accessible::Property::Label(&title),
            gtk::accessible::Property::Description(&format!(
                "{} notifications, collapsed",
                members.len()
            )),
        ]);
        {
            let (me, key) = (self.clone(), key.to_owned());
            group.connect_clicked(move |_| me.set_expanded(&key, true));
        }
        let (me, ids) = (
            self.clone(),
            members.iter().map(|n| n.id).collect::<Vec<_>>(),
        );
        close.connect_clicked(move |_| me.remove(&ids));
        group
    }

    /// An expanded group: a header with the app name and a round
    /// collapse button, then every card.
    fn expanded_group(self: &Rc<Self>, key: &str, members: &[&Notification]) -> gtk::Box {
        let (title, _) = source_of(members[0]);
        let group = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        group.add_css_class("message-group");
        group.add_css_class("expanded");
        group.update_property(&[gtk::accessible::Property::Label(&title)]);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("message-group-header");
        let label = gtk::Label::new(Some(&title));
        label.add_css_class("message-group-title");
        label.set_hexpand(true);
        label.set_xalign(0.0);
        header.append(&label);
        let collapse = gtk::Button::from_icon_name("group-collapse-symbolic");
        collapse.add_css_class("message-collapse-button");
        collapse.set_valign(gtk::Align::Center);
        collapse.update_property(&[gtk::accessible::Property::Label("Collapse")]);
        {
            let (me, key) = (self.clone(), key.to_owned());
            collapse.connect_clicked(move |_| me.set_expanded(&key, false));
        }
        header.append(&collapse);
        group.append(&header);
        for n in members {
            let (c, close) = card(n);
            let (me, id) = (self.clone(), n.id);
            close.connect_clicked(move |_| me.remove(&[id]));
            group.append(&c);
        }
        group
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;

    #[test]
    fn notifications_group_per_app_newest_first() {
        // History newest first: b2, a2, a1, c, b1.
        let history = [(5, "b"), (4, "a"), (3, "a"), (2, "c"), (1, "b")];
        assert_eq!(
            group_ids(history),
            vec![("b", vec![5, 1]), ("a", vec![4, 3]), ("c", vec![2])]
        );
        assert!(group_ids(Vec::<(u64, &str)>::new()).is_empty());
    }

    #[test]
    fn collapsed_stack_peeks_like_gnome() {
        // One card: nothing peeks.
        assert!(stack_layout(1).is_empty());
        // The second card 10 px below, 6 px in per side.
        assert_eq!(stack_layout(2), vec![(6, 10)]);
        // The third 10/1.4 px further, 12 px in; never more than three.
        assert_eq!(stack_layout(3), vec![(6, 10), (12, 17)]);
        assert_eq!(stack_layout(7), stack_layout(3));
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
