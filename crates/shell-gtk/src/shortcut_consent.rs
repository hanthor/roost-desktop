//! GNOME's shortcuts-inhibitor consent and PermissionStore vocabulary.
//! Unknown/window-backed apps are deliberately not remembered.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use glib::variant::ToVariant;
use gtk::prelude::*;
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

const BUS: &str = "org.freedesktop.impl.portal.PermissionStore";
const PATH: &str = "/org/freedesktop/impl/portal/PermissionStore";
const TABLE: &str = "gnome";
const ID: &str = "shortcuts-inhibitor";

type Answer = Rc<dyn Fn(u64, bool)>;

pub struct Consent {
    window: gtk::Window,
    title: gtk::Label,
    pending: RefCell<Option<(u64, Option<String>)>>,
    answer: Answer,
}

impl Consent {
    pub fn new(app: &gtk::Application, answer: Answer) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.set_title(Some("Allow inhibiting shortcuts"));
        window.add_css_class("roost-end-session");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("roost-shortcut-consent"));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.add_css_class("modal-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_margin_top(24);
        card.set_margin_bottom(24);
        let title = gtk::Label::new(None);
        title.add_css_class("message-dialog-title");
        title.set_wrap(true);
        title.set_max_width_chars(50);
        card.append(&title);
        let hint = gtk::Label::new(Some("You can restore shortcuts by pressing Super+Escape"));
        hint.add_css_class("message-dialog-description");
        hint.set_wrap(true);
        card.append(&hint);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.add_css_class("modal-dialog-button-box");
        let deny = gtk::Button::with_label("Deny");
        let allow = gtk::Button::with_label("Allow");
        for button in [&deny, &allow] {
            button.add_css_class("modal-dialog-button");
            buttons.append(button);
        }
        card.append(&buttons);
        window.set_child(Some(&card));
        let ui = Rc::new(Self {
            window,
            title,
            pending: RefCell::new(None),
            answer,
        });
        let weak = Rc::downgrade(&ui);
        deny.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.respond(false, true);
            }
        });
        let weak = Rc::downgrade(&ui);
        allow.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.respond(true, true);
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&ui);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(ui) = weak.upgrade() {
                    ui.respond(false, true);
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        ui.window.add_controller(keys);
        let weak = Rc::downgrade(&ui);
        ui.window.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.respond(false, false);
            }
            glib::Propagation::Stop
        });
        ui
    }

    pub fn dismiss(&self) {
        self.pending.borrow_mut().take();
        self.window.set_visible(false);
    }

    pub fn sync(self: &Rc<Self>, request: Option<(u64, String)>) {
        self.dismiss();
        let Some((id, app)) = request else {
            return;
        };
        let desktop_id = if app.ends_with(".desktop") {
            app
        } else {
            format!("{app}.desktop")
        };
        let desktop = roost_shell_host::apps::discover_system()
            .into_iter()
            .find(|a| a.app_id == desktop_id);
        let name = desktop.as_ref().map(|a| a.name.clone());
        let stable = desktop.map(|_| desktop_id);
        self.title.set_text(
            &name
                .map(|n| format!("Allow {n} to inhibit shortcuts?"))
                .unwrap_or_else(|| "Allow this app to inhibit shortcuts?".into()),
        );
        *self.pending.borrow_mut() = Some((id, stable.clone()));
        let Some(app_id) = stable else {
            self.window.present();
            return;
        };
        // Both bus acquisition and calls are asynchronous and bounded;
        // missing/broken PermissionStore opens the prompt, never grants.
        let weak = Rc::downgrade(self);
        gio::bus_get(
            gio::BusType::Session,
            gio::Cancellable::NONE,
            move |result| {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                if !ui.is_pending(id) {
                    return;
                }
                let Ok(conn) = result else {
                    ui.window.present();
                    return;
                };
                let weak = Rc::downgrade(&ui);
                conn.call(
                    Some(BUS),
                    PATH,
                    BUS,
                    "Lookup",
                    Some(&(TABLE, ID).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    2000,
                    gio::Cancellable::NONE,
                    move |result| {
                        let Some(ui) = weak.upgrade() else {
                            return;
                        };
                        if !ui.is_pending(id) {
                            return;
                        }
                        let decision = result
                            .ok()
                            .and_then(|v| v.try_child_value(0))
                            .and_then(|v| v.get::<HashMap<String, Vec<String>>>())
                            .and_then(|p| p.get(&app_id).and_then(|v| v.first()).cloned());
                        match decision.as_deref() {
                            Some("GRANTED") => ui.respond(true, false),
                            Some("DENIED") => ui.respond(false, false),
                            _ => ui.window.present(),
                        }
                    },
                );
            },
        );
    }

    fn is_pending(&self, id: u64) -> bool {
        self.pending
            .borrow()
            .as_ref()
            .is_some_and(|(pending, _)| *pending == id)
    }

    fn respond(&self, allow: bool, remember: bool) {
        let Some((id, app)) = self.pending.borrow_mut().take() else {
            return;
        };
        (self.answer)(id, allow);
        self.window.set_visible(false);
        if let Some(app) = app.filter(|_| remember) {
            gio::bus_get(
                gio::BusType::Session,
                gio::Cancellable::NONE,
                move |result| {
                    let Ok(conn) = result else {
                        return;
                    };
                    let grant = if allow { "GRANTED" } else { "DENIED" };
                    conn.call(
                        Some(BUS),
                        PATH,
                        BUS,
                        "SetPermission",
                        Some(&(TABLE, true, ID, app.as_str(), vec![grant]).to_variant()),
                        None,
                        gio::DBusCallFlags::NONE,
                        2000,
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                },
            );
        }
    }
}
