//! Power menu and end-session dialog (GNOME 51 shape, #62).
//!
//! The quick-settings power button opens an in-panel menu: Suspend,
//! Restart…, Power Off…, Log Out. The "…" actions open GNOME's
//! end-session dialog: a dimmed full-screen layer with a 60-second
//! countdown, Cancel, and the action; the countdown runs the action.
//! Everything goes through logind on the system bus.
//!
//! In the nested preview (`TUNA_SESSION_KIND=nested`) logind's session
//! is the host's: only Log Out is offered, and it ends this compositor
//! (SIGTERM to its parent), never the host session.
//!
//! Before sleep, logind's `PrepareForSleep(true)` locks the screen, so
//! the session resumes locked from any suspend (menu, lid, idle).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

const LOGIND: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER_IFACE: &str = "org.freedesktop.login1.Manager";
const SESSION_PATH: &str = "/org/freedesktop/login1/session/auto";
const SESSION_IFACE: &str = "org.freedesktop.login1.Session";
/// GNOME's end-session countdown.
pub const COUNTDOWN_S: u32 = 60;

/// One end-session action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Suspend,
    Restart,
    PowerOff,
    LogOut,
}

impl Action {
    /// Dialog title and confirm button label.
    pub fn title(self) -> &'static str {
        match self {
            Action::Suspend => "Suspend",
            Action::Restart => "Restart",
            Action::PowerOff => "Power Off",
            Action::LogOut => "Log Out",
        }
    }

    /// GNOME's countdown sentence for `seconds` left.
    pub fn countdown_text(self, seconds: u32) -> String {
        let unit = if seconds == 1 { "second" } else { "seconds" };
        match self {
            Action::Restart => {
                format!("The system will restart automatically in {seconds} {unit}")
            }
            Action::PowerOff => {
                format!("The system will power off automatically in {seconds} {unit}")
            }
            // GNOME 51's strings, without a closing period.
            Action::LogOut => format!("You will be logged out automatically in {seconds} {unit}"),
            Action::Suspend => String::new(),
        }
    }
}

/// Which menu rows a session offers.
pub fn menu_actions(nested: bool) -> &'static [Action] {
    if nested {
        &[Action::LogOut]
    } else {
        &[
            Action::Suspend,
            Action::Restart,
            Action::PowerOff,
            Action::LogOut,
        ]
    }
}

/// Whether this shell runs inside another desktop.
pub fn is_nested() -> bool {
    std::env::var("TUNA_SESSION_KIND").map_or(true, |kind| kind != "hardware")
}

fn system_call(
    conn: &gio::DBusConnection,
    path: &str,
    iface: &str,
    method: &str,
    args: Option<glib::Variant>,
) {
    conn.call(
        Some(LOGIND),
        path,
        iface,
        method,
        args.as_ref(),
        None,
        gio::DBusCallFlags::NONE,
        5000,
        None::<&gio::Cancellable>,
        |reply| {
            if let Err(e) = reply {
                eprintln!("tuna-shell-gtk: logind call failed: {e}");
            }
        },
    );
}

/// Carry out `action` now.
fn perform(action: Action, nested: bool, conn: Option<&gio::DBusConnection>) {
    if nested {
        if action == Action::LogOut {
            // End this compositor only: it shuts down cleanly on SIGTERM.
            unsafe {
                libc::kill(libc::getppid(), libc::SIGTERM);
            }
        }
        return;
    }
    let Some(conn) = conn else {
        eprintln!("tuna-shell-gtk: no system bus for {action:?}");
        return;
    };
    let interactive = Some((true,).to_variant());
    match action {
        Action::Suspend => system_call(conn, MANAGER_PATH, MANAGER_IFACE, "Suspend", interactive),
        Action::Restart => system_call(conn, MANAGER_PATH, MANAGER_IFACE, "Reboot", interactive),
        Action::PowerOff => system_call(conn, MANAGER_PATH, MANAGER_IFACE, "PowerOff", interactive),
        Action::LogOut => system_call(conn, SESSION_PATH, SESSION_IFACE, "Terminate", None),
    }
}

/// The power surfaces: the menu rows for quick settings, the dialog.
pub struct PowerUi {
    nested: bool,
    conn: Rc<RefCell<Option<gio::DBusConnection>>>,
    dialog: gtk::ApplicationWindow,
    /// The modal fade in and out (`ModalDialog`, 100 ms).
    fade: crate::transient::ModalFade,
    title: gtk::Label,
    body: gtk::Label,
    confirm: gtk::Button,
    pending: Cell<Option<Action>>,
    left: Cell<u32>,
    timer: RefCell<Option<glib::SourceId>>,
}

impl PowerUi {
    /// Build the dialog; `lock` locks the session (before sleep).
    pub fn new(app: &gtk::Application, lock: Rc<dyn Fn()>) -> Rc<Self> {
        let nested = is_nested();
        let dialog = gtk::ApplicationWindow::new(app);
        dialog.add_css_class("tuna-end-session");
        dialog.init_layer_shell();
        dialog.set_layer(Layer::Overlay);
        dialog.set_namespace(Some(tuna_shell_control::END_SESSION_NAMESPACE));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            dialog.set_anchor(edge, true);
        }
        // Cover the top bar too: a modal dims the whole screen.
        dialog.set_exclusive_zone(-1);
        dialog.set_keyboard_mode(KeyboardMode::Exclusive);
        dialog.set_title(Some("End Session"));
        // GNOME's ModalDialog (.modal-dialog.end-session-dialog): a 24em
        // card, the title and countdown centered, two 43px buttons.
        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.add_css_class("modal-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(400, -1);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("modal-dialog-content-box");
        let title = gtk::Label::new(None);
        title.add_css_class("message-dialog-title");
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
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("modal-dialog-button");
        let confirm = gtk::Button::with_label("");
        confirm.add_css_class("modal-dialog-button");
        buttons.append(&cancel);
        buttons.append(&confirm);
        card.append(&content);
        card.append(&buttons);
        dialog.set_child(Some(&card));

        let ui = Rc::new(Self {
            fade: crate::transient::ModalFade::new(&dialog),
            nested,
            conn: Rc::new(RefCell::new(None)),
            dialog,
            title,
            body,
            confirm,
            pending: Cell::new(None),
            left: Cell::new(0),
            timer: RefCell::new(None),
        });
        {
            let me = ui.clone();
            cancel.connect_clicked(move |_| me.cancel());
        }
        {
            let me = ui.clone();
            ui.confirm.connect_clicked(move |_| me.run_pending());
        }
        {
            // Escape cancels, as in GNOME.
            let keys = gtk::EventControllerKey::new();
            let me = ui.clone();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    me.cancel();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            ui.dialog.add_controller(keys);
        }
        // System bus: logind for actions and the before-sleep lock.
        {
            let conn_slot = ui.conn.clone();
            gio::bus_get(
                gio::BusType::System,
                None::<&gio::Cancellable>,
                move |conn| {
                    let Ok(conn) = conn else {
                        return;
                    };
                    let sub = conn.subscribe_to_signal(
                        Some(LOGIND),
                        Some(MANAGER_IFACE),
                        Some("PrepareForSleep"),
                        Some(MANAGER_PATH),
                        None,
                        gio::DBusSignalFlags::NONE,
                        move |signal| {
                            let going = signal
                                .parameters
                                .try_child_value(0)
                                .and_then(|v| v.get::<bool>())
                                .unwrap_or(false);
                            if going {
                                lock();
                            }
                        },
                    );
                    std::mem::forget(sub);
                    *conn_slot.borrow_mut() = Some(conn);
                },
            );
        }
        ui
    }

    /// Menu rows for this session, wired to their actions. Each row
    /// calls `close_menu` first (the quick-settings popover goes away).
    /// The actions the power menu offers here.
    pub fn actions(&self) -> &'static [Action] {
        menu_actions(self.nested)
    }

    /// Run a menu choice: Suspend at once, the rest through GNOME's
    /// countdown dialog.
    pub fn activate(self: &Rc<Self>, action: Action) {
        if action == Action::Suspend {
            self.perform(Action::Suspend);
        } else {
            self.ask(action);
        }
    }

    fn perform(&self, action: Action) {
        perform(action, self.nested, self.conn.borrow().as_ref());
    }

    /// Open the dialog for `action` with a fresh countdown.
    pub fn ask(self: &Rc<Self>, action: Action) {
        self.stop_timer();
        self.pending.set(Some(action));
        self.left.set(COUNTDOWN_S);
        self.title.set_text(action.title());
        self.confirm.set_label(action.title());
        self.body.set_text(&action.countdown_text(COUNTDOWN_S));
        self.fade.present();
        // Cancel holds the focus when the dialog opens, as in GNOME.
        if let Some(cancel) = self.confirm.prev_sibling().and_downcast::<gtk::Button>() {
            cancel.grab_focus();
        }
        let me = self.clone();
        let id = glib::timeout_add_local(Duration::from_secs(1), move || {
            let left = me.left.get().saturating_sub(1);
            me.left.set(left);
            if let Some(action) = me.pending.get() {
                me.body.set_text(&action.countdown_text(left));
            }
            if left == 0 {
                me.timer.borrow_mut().take();
                me.run_pending();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.timer.borrow_mut() = Some(id);
    }

    fn stop_timer(&self) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
    }

    fn cancel(&self) {
        self.stop_timer();
        self.pending.set(None);
        self.fade.hide();
    }

    fn run_pending(&self) {
        self.stop_timer();
        self.fade.hide();
        if let Some(action) = self.pending.take() {
            self.perform(action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_sessions_only_offer_log_out() {
        assert_eq!(menu_actions(true), [Action::LogOut]);
        assert_eq!(
            menu_actions(false),
            [
                Action::Suspend,
                Action::Restart,
                Action::PowerOff,
                Action::LogOut
            ]
        );
    }

    #[test]
    fn countdown_text_matches_gnome() {
        assert_eq!(
            Action::PowerOff.countdown_text(60),
            "The system will power off automatically in 60 seconds"
        );
        assert_eq!(
            Action::Restart.countdown_text(1),
            "The system will restart automatically in 1 second"
        );
        assert_eq!(
            Action::LogOut.countdown_text(5),
            "You will be logged out automatically in 5 seconds"
        );
    }
}
