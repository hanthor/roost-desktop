//! GNOME's keyboard accessibility settings for the compositor's input
//! aids, and the confirmation GNOME shows when the keyboard itself
//! switches one (#350, GNOME Shell's kbdA11yDialog).
//!
//! The compositor runs the aids. When Shift pressed five times, Shift
//! held eight seconds, or two modifiers pressed at once switches Sticky
//! or Slow Keys, it tells the shell; the shell saves the new value to
//! `org.gnome.desktop.a11y.keyboard` at once, as Mutter does, and asks
//! whether to keep it.

use std::cell::Cell;
use std::rc::Rc;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tuna_shell_control::{KeyboardAid, KeyboardAids};

pub const SCHEMA: &str = "org.gnome.desktop.a11y.keyboard";

/// GNOME's keyboard accessibility settings, flattened for the compositor.
/// Without the schema every aid stays off (GNOME's defaults).
pub fn read(settings: Option<&gio::Settings>) -> KeyboardAids {
    let Some(s) = settings else {
        return KeyboardAids::default();
    };
    let ms = |key: &str| s.int(key).max(0) as u32;
    KeyboardAids {
        shortcuts: s.boolean("enable"),
        feature_beep: s.boolean("feature-state-change-beep"),
        sticky: s.boolean("stickykeys-enable"),
        sticky_two_key_off: s.boolean("stickykeys-two-key-off"),
        sticky_beep: s.boolean("stickykeys-modifier-beep"),
        slow: s.boolean("slowkeys-enable"),
        slow_delay_ms: ms("slowkeys-delay"),
        slow_beep_press: s.boolean("slowkeys-beep-press"),
        slow_beep_accept: s.boolean("slowkeys-beep-accept"),
        slow_beep_reject: s.boolean("slowkeys-beep-reject"),
        bounce: s.boolean("bouncekeys-enable"),
        bounce_delay_ms: ms("bouncekeys-delay"),
        bounce_beep_reject: s.boolean("bouncekeys-beep-reject"),
        toggle_beep: s.boolean("togglekeys-enable"),
        mouse: s.boolean("mousekeys-enable"),
        mouse_max_speed: ms("mousekeys-max-speed"),
        mouse_accel_ms: ms("mousekeys-accel-time"),
        mouse_init_delay_ms: ms("mousekeys-init-delay"),
    }
}

/// The setting an aid lives in.
pub fn key_of(aid: KeyboardAid) -> &'static str {
    match aid {
        KeyboardAid::StickyKeys => "stickykeys-enable",
        KeyboardAid::SlowKeys => "slowkeys-enable",
    }
}

/// GNOME's dialog wording: title, description, and the labels of the
/// keep-on and keep-off buttons.
pub fn dialog_text(
    aid: KeyboardAid,
    enabled: bool,
) -> (&'static str, &'static str, &'static str, &'static str) {
    let (on, off) = if enabled {
        ("Leave On", "Turn Off")
    } else {
        ("Turn On", "Leave Off")
    };
    let (title, description) = match (aid, enabled) {
        (KeyboardAid::SlowKeys, true) => (
            "Slow Keys Turned On",
            "You just held down the Shift key for 8 seconds. This is the shortcut for the Slow Keys feature, which affects the way your keyboard works.",
        ),
        (KeyboardAid::SlowKeys, false) => (
            "Slow Keys Turned Off",
            "You just held down the Shift key for 8 seconds. This is the shortcut for the Slow Keys feature, which affects the way your keyboard works.",
        ),
        (KeyboardAid::StickyKeys, true) => (
            "Sticky Keys Turned On",
            "You just pressed the Shift key 5 times in a row. This is the shortcut for the Sticky Keys feature, which affects the way your keyboard works.",
        ),
        (KeyboardAid::StickyKeys, false) => (
            "Sticky Keys Turned Off",
            "You just pressed two keys at once, or pressed the Shift key 5 times in a row. This turns off the Sticky Keys feature, which affects the way your keyboard works.",
        ),
    };
    (title, description, on, off)
}

/// The confirmation dialog: GNOME's ModalDialog with a message and two
/// buttons. Escape picks the button that undoes nothing new: "Turn Off"
/// after switching on, "Turn On" after switching off, as in GNOME.
pub struct KbdA11yDialog {
    settings: Option<gio::Settings>,
    window: gtk::Window,
    title: gtk::Label,
    body: gtk::Label,
    on: gtk::Button,
    off: gtk::Button,
    pending: Cell<Option<(KeyboardAid, bool)>>,
}

impl KbdA11yDialog {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.set_title(Some("Keyboard Accessibility"));
        window.add_css_class("tuna-end-session");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::KBD_A11Y_NAMESPACE));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.add_css_class("modal-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(400, -1);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("modal-dialog-content-box");
        let title = gtk::Label::new(None);
        title.add_css_class("message-dialog-title");
        title.set_wrap(true);
        let body = gtk::Label::new(None);
        body.add_css_class("message-dialog-description");
        body.set_wrap(true);
        body.set_max_width_chars(1);
        body.set_hexpand(true);
        body.set_justify(gtk::Justification::Center);
        content.append(&title);
        content.append(&body);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.add_css_class("modal-dialog-button-box");
        buttons.set_homogeneous(true);
        let on = gtk::Button::with_label("");
        let off = gtk::Button::with_label("");
        for button in [&on, &off] {
            button.add_css_class("modal-dialog-button");
            buttons.append(button);
        }
        card.append(&content);
        card.append(&buttons);
        window.set_child(Some(&card));
        let ui = Rc::new(Self {
            settings: crate::settings(SCHEMA),
            window,
            title,
            body,
            on,
            off,
            pending: Cell::new(None),
        });
        for (button, value) in [(&ui.on, true), (&ui.off, false)] {
            let weak = Rc::downgrade(&ui);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.choose(value);
                }
            });
        }
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&ui);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gtk::gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            if let Some(ui) = weak.upgrade() {
                // Escape is the button that reverts the switch.
                let enabled = ui.pending.get().is_some_and(|(_, enabled)| enabled);
                ui.choose(!enabled);
            }
            glib::Propagation::Stop
        });
        ui.window.add_controller(keys);
        ui
    }

    /// The keyboard switched `aid`: save it now and ask to confirm.
    /// While locked the setting is saved without a dialog.
    pub fn switched(&self, aid: KeyboardAid, enabled: bool, locked: bool) {
        self.save(aid, enabled);
        if locked {
            self.dismiss();
            return;
        }
        let (title, body, on, off) = dialog_text(aid, enabled);
        self.title.set_text(title);
        self.body.set_text(body);
        self.on.set_label(on);
        self.off.set_label(off);
        self.pending.set(Some((aid, enabled)));
        self.window.present();
        // The default button holds the focus: keep the new state.
        if enabled {
            self.on.grab_focus();
        } else {
            self.off.grab_focus();
        }
    }

    pub fn dismiss(&self) {
        self.pending.set(None);
        self.window.set_visible(false);
    }

    fn save(&self, aid: KeyboardAid, enabled: bool) {
        if let Some(settings) = self.settings.as_ref() {
            if let Err(e) = settings.set_boolean(key_of(aid), enabled) {
                eprintln!("tuna-shell-gtk: could not save {}: {e}", key_of(aid));
            }
        }
    }

    fn choose(&self, enabled: bool) {
        if let Some((aid, _)) = self.pending.take() {
            self.save(aid, enabled);
        }
        self.window.set_visible(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_wording_matches_gnome() {
        assert_eq!(
            dialog_text(KeyboardAid::StickyKeys, true),
            (
                "Sticky Keys Turned On",
                "You just pressed the Shift key 5 times in a row. This is the shortcut for the Sticky Keys feature, which affects the way your keyboard works.",
                "Leave On",
                "Turn Off"
            )
        );
        let (title, description, on, off) = dialog_text(KeyboardAid::StickyKeys, false);
        assert_eq!(title, "Sticky Keys Turned Off");
        assert!(description.starts_with("You just pressed two keys at once"));
        assert_eq!((on, off), ("Turn On", "Leave Off"));
        let (title, description, _, _) = dialog_text(KeyboardAid::SlowKeys, true);
        assert_eq!(title, "Slow Keys Turned On");
        assert!(description.contains("held down the Shift key for 8 seconds"));
        assert_eq!(
            dialog_text(KeyboardAid::SlowKeys, false).0,
            "Slow Keys Turned Off"
        );
    }

    #[test]
    fn aids_map_to_gnome_keys() {
        assert_eq!(key_of(KeyboardAid::StickyKeys), "stickykeys-enable");
        assert_eq!(key_of(KeyboardAid::SlowKeys), "slowkeys-enable");
        assert_eq!(read(None), KeyboardAids::default());
    }
}
