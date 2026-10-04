//! GNOME 51's lock screen (`unlockDialog.js`) on ext-session-lock-v1.
//!
//! The compositor owns the lock: it decides when the session is locked,
//! draws the blurred, dimmed wallpaper underneath, and verifies the
//! password. This module only draws GNOME's curtain (clock, date and
//! "Click or press a key"), switches to the unlock prompt on a key or
//! click, and sends what is typed in a redacted `Unlock` command. It
//! releases its lock surfaces once the compositor's snapshot says the
//! session is unlocked, never on its own.
//!
//! Measurements are from GNOME Shell 51's own lock screen at 1280x800
//! (docs/gnome-parity.md, states 11 and 11b).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_session_lock as session_lock;

use crate::logic;

/// GNOME's wrong-password message (authPrompt.js).
const FAILED: &str = "Sorry, that didn't work. Please try again.";

/// Sends a password; returns the request id.
pub type SendUnlock = Rc<dyn Fn(String) -> Option<u64>>;

/// One monitor's lock window.
struct Screen {
    stack: gtk::Stack,
    clock: gtk::Label,
    date: gtk::Label,
    entry: gtk::PasswordEntry,
    message: gtk::Label,
}

pub struct LockUi {
    app: gtk::Application,
    instance: session_lock::Instance,
    send: SendUnlock,
    screens: RefCell<Vec<Screen>>,
    /// A lock was asked of the compositor and not yet released.
    requested: Cell<bool>,
    /// The password check in flight, by request id.
    pending: Cell<Option<u64>>,
    clock_format: Box<dyn Fn() -> logic::ClockFormat>,
}

impl LockUi {
    pub fn new(
        app: &gtk::Application,
        send: SendUnlock,
        clock_format: Box<dyn Fn() -> logic::ClockFormat>,
    ) -> Rc<Self> {
        let ui = Rc::new(Self {
            app: app.clone(),
            instance: session_lock::Instance::new(),
            send,
            screens: RefCell::new(Vec::new()),
            requested: Cell::new(false),
            pending: Cell::new(None),
            clock_format,
        });
        {
            let weak = Rc::downgrade(&ui);
            ui.instance.connect_failed(move |_| {
                eprintln!("roost-shell-gtk: the compositor refused the session lock");
                if let Some(ui) = weak.upgrade() {
                    ui.screens.borrow_mut().clear();
                }
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            ui.instance.connect_unlocked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.screens.borrow_mut().clear();
                    ui.pending.set(None);
                }
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            glib::timeout_add_seconds_local(1, move || match weak.upgrade() {
                Some(ui) => {
                    ui.tick();
                    glib::ControlFlow::Continue
                }
                None => glib::ControlFlow::Break,
            });
        }
        ui
    }

    /// Follow the compositor's lock flag: lock when it says locked,
    /// release when it says unlocked.
    pub fn sync(self: &Rc<Self>, locked: bool) {
        if locked && !self.requested.get() {
            self.requested.set(true);
            if !self.instance.lock() {
                eprintln!("roost-shell-gtk: session lock unavailable");
                return;
            }
            // One lock window per monitor, assigned right after lock()
            // (gtk4-layer-shell 1.1's contract).
            let Some(display) = gtk::gdk::Display::default() else {
                return;
            };
            let monitors = display.monitors();
            for i in 0..monitors.n_items() {
                let Some(monitor) = monitors
                    .item(i)
                    .and_then(|m| m.downcast::<gtk::gdk::Monitor>().ok())
                else {
                    continue;
                };
                let window = self.screen(&self.app.clone());
                self.instance.assign_window_to_monitor(&window, &monitor);
                window.present();
            }
            self.tick();
        } else if !locked && self.requested.get() {
            self.requested.set(false);
            self.instance.unlock();
        }
    }

    /// A command result arrived: the answer to our password, if it is
    /// the one in flight.
    pub fn command_result(&self, id: u64, applied: bool) {
        if self.pending.get() != Some(id) {
            return;
        }
        self.pending.set(None);
        if std::env::var_os("ROOST_LOCK_TRACE").is_some() {
            eprintln!("roost-shell-gtk: lock command result applied={applied}");
        }
        for screen in self.screens.borrow().iter() {
            screen.entry.set_sensitive(true);
            if !applied {
                screen.stack.set_visible_child_name("prompt");
                screen.entry.set_text("");
                screen.message.set_label(FAILED);
                screen.entry.grab_focus();
            }
        }
    }

    /// Clock and date, as GNOME's UnlockDialogClock shows them.
    fn tick(&self) {
        let now = jiff::Zoned::now().datetime();
        let time = logic::lock_clock_text(&now, (self.clock_format)());
        let date = logic::lock_date_text(now.date());
        for screen in self.screens.borrow().iter() {
            screen.clock.set_label(&time);
            screen.date.set_label(&date);
        }
    }

    /// Build one monitor's lock window: GNOME's unlock-screen top bar
    /// over a stack of the clock and the prompt.
    fn screen(self: &Rc<Self>, app: &gtk::Application) -> gtk::Window {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("roost-lock");
        window.set_title(Some("Lock Screen"));

        // The clock page (unlock-dialog-clock).
        let clock = gtk::Label::new(None);
        clock.add_css_class("unlock-dialog-clock-time");
        let date = gtk::Label::new(None);
        date.add_css_class("unlock-dialog-clock-date");
        let hint = gtk::Label::new(Some("Click or press a key"));
        hint.add_css_class("unlock-dialog-clock-hint");
        hint.set_halign(gtk::Align::Center);
        let clock_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        clock_box.add_css_class("unlock-dialog-clock");
        clock_box.set_halign(gtk::Align::Center);
        clock_box.set_valign(gtk::Align::Start);
        clock_box.append(&clock);
        clock_box.append(&date);
        clock_box.append(&hint);

        // The prompt page (login-dialog-prompt-layout).
        let avatar = gtk::Image::from_icon_name("avatar-default-symbolic");
        avatar.add_css_class("user-icon");
        avatar.set_halign(gtk::Align::Center);
        let name = gtk::Label::new(Some(&logic::real_name()));
        name.add_css_class("user-widget-label");
        // GNOME keeps the Back button's room but shows it only from a
        // second PAM step on; a password is one step, so it stays
        // invisible (authPrompt.js `_updateCancelButton`).
        let cancel = gtk::Button::from_icon_name("go-previous-symbolic");
        cancel.add_css_class("cancel-button");
        cancel.set_valign(gtk::Align::Center);
        cancel.set_opacity(0.0);
        cancel.set_can_target(false);
        cancel.set_can_focus(false);
        cancel.update_property(&[gtk::accessible::Property::Label("Back")]);
        let entry = gtk::PasswordEntry::new();
        entry.add_css_class("login-dialog-prompt-entry");
        entry.set_hexpand(true);
        entry.set_show_peek_icon(true);
        entry.set_property("placeholder-text", "Password");
        entry.update_property(&[gtk::accessible::Property::Label("Password")]);
        // Handle Return before the internal text/IM delegate: a password
        // field must submit even when that delegate consumes activation.
        // Use the same signal as the arrow, so pending checks and PAM
        // verification stay in the single handler below. This controller
        // only sees keys focused within the entry, not other lock controls.
        let submit_keys = gtk::EventControllerKey::new();
        submit_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let entry = entry.clone();
            submit_keys.connect_key_pressed(move |_, key, _, _| {
                if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter) {
                    if entry.is_sensitive() {
                        entry.emit_by_name::<()>("activate", &[]);
                    }
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
        }
        entry.add_controller(submit_keys);
        // The Submit arrow sits inside the entry's right end.
        let next = gtk::Button::from_icon_name("go-next-symbolic");
        next.add_css_class("next-button");
        next.set_halign(gtk::Align::End);
        next.set_valign(gtk::Align::Center);
        next.set_can_focus(false);
        next.update_property(&[gtk::accessible::Property::Label("Submit")]);
        {
            let entry = entry.clone();
            next.connect_clicked(move |_| {
                entry.emit_by_name::<()>("activate", &[]);
            });
        }
        let entry_area = gtk::Overlay::new();
        entry_area.add_css_class("login-dialog-prompt-entry-area");
        entry_area.set_hexpand(true);
        entry_area.set_child(Some(&entry));
        entry_area.add_overlay(&next);
        // Mirrors the Back button so the entry stays centred.
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.add_css_class("button-box-spacer");
        let button_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        button_box.add_css_class("login-dialog-button-box");
        button_box.append(&cancel);
        button_box.append(&entry_area);
        button_box.append(&spacer);
        let caps = gtk::Label::new(None);
        caps.add_css_class("caps-lock-warning-label");
        let message = gtk::Label::new(None);
        message.add_css_class("login-dialog-message");
        message.set_wrap(true);
        message.set_justify(gtk::Justification::Center);
        message.set_yalign(0.0);
        let prompt = gtk::Box::new(gtk::Orientation::Vertical, 0);
        prompt.add_css_class("login-dialog-prompt-layout");
        prompt.set_halign(gtk::Align::Center);
        prompt.set_valign(gtk::Align::Start);
        prompt.append(&avatar);
        prompt.append(&name);
        prompt.append(&button_box);
        prompt.append(&caps);
        prompt.append(&message);

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(300);
        stack.add_named(&clock_box, Some("clock"));
        stack.add_named(&prompt, Some("prompt"));
        stack.set_visible_child_name("clock");
        stack.set_vexpand(true);

        // GNOME's #panel.unlock-screen: transparent, the system menu's
        // power icon alone at the right.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.add_css_class("unlock-screen-panel");
        let power = gtk::Image::from_icon_name("system-shutdown-symbolic");
        power.add_css_class("system-status-icon");
        power.set_hexpand(true);
        power.set_halign(gtk::Align::End);
        bar.append(&power);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));
        bar.set_valign(gtk::Align::Start);
        overlay.add_overlay(&bar);
        window.set_child(Some(&overlay));

        // Vertical placement: GNOME puts the stack a third of the way
        // down (UnlockDialogLayout), which lands the 1280x800 clock at
        // y 275 and the prompt at y 205.
        {
            let (clock_box, prompt) = (clock_box.clone(), prompt.clone());
            let last = Cell::new(-1);
            window.add_tick_callback(move |w, _| {
                let height = w.height();
                if height != last.get() {
                    last.set(height);
                    let (clock_top, prompt_top) = logic::lock_offsets(height);
                    clock_box.set_margin_top(clock_top);
                    prompt.set_margin_top(prompt_top);
                }
                glib::ControlFlow::Continue
            });
        }

        // Any key or click on the curtain shows the prompt; a printable
        // key starts the password with it (GNOME's shape). Escape on the
        // prompt goes back to the curtain.
        let show_prompt = {
            let (stack, entry) = (stack.clone(), entry.clone());
            move || {
                if stack.visible_child_name().as_deref() != Some("prompt") {
                    stack.set_visible_child_name("prompt");
                }
                entry.grab_focus();
            }
        };
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let (stack, entry, message, show_prompt) = (
                stack.clone(),
                entry.clone(),
                message.clone(),
                show_prompt.clone(),
            );
            keys.connect_key_pressed(move |_, key, _, _| {
                let on_prompt = stack.visible_child_name().as_deref() == Some("prompt");
                if key == gtk::gdk::Key::Escape {
                    if on_prompt {
                        entry.set_text("");
                        message.set_label("");
                        stack.set_visible_child_name("clock");
                    }
                    return glib::Propagation::Stop;
                }
                if on_prompt {
                    if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter)
                        && std::env::var_os("ROOST_LOCK_TRACE").is_some()
                    {
                        eprintln!(
                            "roost-shell-gtk: lock Return received sensitive={}",
                            entry.is_sensitive()
                        );
                    }
                    return glib::Propagation::Proceed;
                }
                // Shift and Caps Lock alone do not lift the curtain.
                if matches!(
                    key,
                    gtk::gdk::Key::Shift_L
                        | gtk::gdk::Key::Shift_R
                        | gtk::gdk::Key::Shift_Lock
                        | gtk::gdk::Key::Caps_Lock
                ) {
                    return glib::Propagation::Proceed;
                }
                show_prompt();
                if let Some(c) = key.to_unicode().filter(|c| !c.is_control()) {
                    entry.set_text(&c.to_string());
                    entry.set_position(-1);
                }
                glib::Propagation::Stop
            });
        }
        window.add_controller(keys);
        let click = gtk::GestureClick::new();
        {
            let (stack, show_prompt) = (stack.clone(), show_prompt.clone());
            click.connect_pressed(move |_, _, _, _| {
                if stack.visible_child_name().as_deref() != Some("prompt") {
                    show_prompt();
                }
            });
        }
        clock_box.add_controller(click);
        let curtain_click = gtk::GestureClick::new();
        {
            let (stack, show_prompt) = (stack.clone(), show_prompt.clone());
            curtain_click.connect_pressed(move |_, _, _, _| {
                if stack.visible_child_name().as_deref() != Some("prompt") {
                    show_prompt();
                }
            });
        }
        window.add_controller(curtain_click);
        {
            let (stack, entry, message) = (stack.clone(), entry.clone(), message.clone());
            cancel.connect_clicked(move |_| {
                entry.set_text("");
                message.set_label("");
                stack.set_visible_child_name("clock");
            });
        }
        {
            let weak = Rc::downgrade(self);
            let message = message.clone();
            entry.connect_activate(move |entry| {
                let Some(ui) = weak.upgrade() else { return };
                if ui.pending.get().is_some() {
                    return;
                }
                if std::env::var_os("ROOST_LOCK_TRACE").is_some() {
                    eprintln!("roost-shell-gtk: lock password submitted");
                }
                let password = entry.text().to_string();
                message.set_label("");
                entry.set_sensitive(false);
                match (ui.send)(password) {
                    Some(id) => ui.pending.set(Some(id)),
                    None => {
                        entry.set_sensitive(true);
                        message.set_label(FAILED);
                    }
                }
            });
        }
        // GNOME's caps-lock warning under the entry.
        if let Some(display) = gtk::gdk::Display::default() {
            if let Some(seat) = display.default_seat() {
                if let Some(keyboard) = seat.keyboard() {
                    let update = {
                        let caps = caps.clone();
                        move |k: &gtk::gdk::Device| {
                            caps.set_label("Caps lock is on.");
                            caps.set_visible(k.is_caps_locked());
                        }
                    };
                    update(&keyboard);
                    keyboard.connect_caps_lock_state_notify(update);
                }
            }
        }

        self.screens.borrow_mut().push(Screen {
            stack,
            clock,
            date,
            entry,
            message,
        });
        window
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        // Never leave a typed password behind in a dead window.
        self.entry.set_text("");
    }
}
