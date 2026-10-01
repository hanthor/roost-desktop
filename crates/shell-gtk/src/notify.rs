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
pub const BANNER_TIMEOUT: Duration = Duration::from_secs(5);
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
    dnd: gtk::Switch,
    first_seen: RefCell<HashMap<u64, Instant>>,
    shown: RefCell<Shown>,
    last_bus_try: RefCell<Option<Instant>>,
}

fn card(n: &Notification) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("notification-card");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let app = gtk::Label::new(Some(n.app()));
    app.add_css_class("notification-app");
    app.set_halign(gtk::Align::Start);
    app.set_hexpand(true);
    header.append(&app);
    card.append(&header);
    let summary = gtk::Label::new(Some(n.summary()));
    summary.add_css_class("notification-summary");
    summary.set_halign(gtk::Align::Start);
    summary.set_wrap(true);
    card.append(&summary);
    if !n.body().is_empty() {
        let body = gtk::Label::new(Some(n.body()));
        body.set_halign(gtk::Align::Start);
        body.set_wrap(true);
        body.set_max_width_chars(48);
        card.append(&body);
    }
    card.update_property(&[gtk::accessible::Property::Label(n.summary())]);
    card
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
        banner_window.set_margin(Edge::Top, 6);
        banner_window.set_keyboard_mode(KeyboardMode::None);
        banner_window.set_title(Some("Notifications"));
        let banner_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        banner_box.set_width_request(420);
        banner_window.set_child(Some(&banner_box));

        // Calendar pane: list (or empty state) above a DND + Clear row.
        let pane = gtk::Box::new(gtk::Orientation::Vertical, 12);
        pane.set_size_request(400, -1);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(240)
            .vexpand(true)
            .build();
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 12);
        empty.set_vexpand(true);
        empty.set_valign(gtk::Align::Center);
        let bell = gtk::Image::from_icon_name("notifications-disabled-symbolic");
        bell.set_pixel_size(72);
        let none = gtk::Label::new(Some("No Notifications"));
        none.add_css_class("no-notifications");
        empty.append(&bell);
        empty.append(&none);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let dnd_label = gtk::Label::new(Some("Do Not Disturb"));
        let dnd = gtk::Switch::new();
        dnd.set_valign(gtk::Align::Center);
        dnd.update_property(&[gtk::accessible::Property::Label("Do Not Disturb")]);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let clear = gtk::Button::with_label("Clear");
        footer.append(&dnd_label);
        footer.append(&dnd);
        footer.append(&spacer);
        footer.append(&clear);
        pane.append(&scroller);
        pane.append(&empty);
        pane.append(&footer);

        let ui = Rc::new(Self {
            center,
            bus: RefCell::new(bus),
            banner_window,
            banner_box,
            pane,
            list,
            empty,
            clear,
            dnd,
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
        {
            let me = ui.clone();
            ui.dnd
                .connect_active_notify(move |switch| me.set_dnd(switch.is_active()));
        }
        ui.refresh();
        ui
    }

    /// The calendar popover's notification pane.
    pub fn pane(&self) -> &gtk::Box {
        &self.pane
    }

    /// Turn Do Not Disturb on or off (calendar switch, quick settings).
    pub fn set_dnd(&self, on: bool) {
        if let Ok(mut center) = self.center.lock() {
            if center.dnd() != on {
                center.set_dnd(on);
            }
        }
        if self.dnd.is_active() != on {
            self.dnd.set_active(on);
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
            live.iter()
                .filter(|(_, u)| *u != Urgency::Critical)
                .filter_map(|(id, _)| {
                    let t = *seen.entry(*id).or_insert(now);
                    (now.duration_since(t) >= BANNER_TIMEOUT).then_some(*id)
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
            let c = card(n);
            c.add_css_class("banner");
            let close = gtk::Button::from_icon_name("window-close-symbolic");
            close.add_css_class("flat");
            close.update_property(&[gtk::accessible::Property::Label("Close")]);
            if let Some(header) = c.first_child().and_downcast::<gtk::Box>() {
                header.append(&close);
            }
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
            self.list.append(&card(n));
        }
        let any = !now.history.is_empty();
        self.list.set_visible(any);
        self.empty.set_visible(!any);
        self.clear.set_sensitive(any);
        // Release the store before touching the switch: its handler
        // takes the same lock, and std's mutex is not re-entrant (with
        // DND persisted on, this deadlocked the shell at startup).
        drop(center);
        let dnd = now.dnd;
        *self.shown.borrow_mut() = now;
        if self.dnd.is_active() != dnd {
            self.dnd.set_active(dnd);
        }
    }
}
