//! GNOME Shell's polkit authentication agent (components/polkitAgent.js):
//! when an app asks for administrator rights, polkit asks the session's
//! agent, and the agent shows GNOME's "Authentication Required" dialog.
//! Without an agent every such request fails.
//!
//! The agent registers itself with org.freedesktop.PolicyKit1.Authority
//! on the system bus for this session, answers BeginAuthentication with
//! the dialog, and checks the password through polkit's own helper
//! (polkit-agent-helper-1, setuid root, which speaks PAM and reports the
//! result to polkit itself). Harnesses point `ROOST_POLKIT_HELPER` at a
//! stand-in.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

const AGENT_PATH: &str = "/org/roost/PolkitAgent";
const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY_IFACE: &str = "org.freedesktop.PolicyKit1.Authority";
/// GNOME's failure line (polkitAgent.js).
const FAILED: &str = "Sorry, that didn’t work. Please try again.";

const XML: &str = r#"<node>
  <interface name="org.freedesktop.PolicyKit1.AuthenticationAgent">
    <method name="BeginAuthentication">
      <arg type="s" name="action_id" direction="in"/>
      <arg type="s" name="message" direction="in"/>
      <arg type="s" name="icon_name" direction="in"/>
      <arg type="a{ss}" name="details" direction="in"/>
      <arg type="s" name="cookie" direction="in"/>
      <arg type="a(sa{sv})" name="identities" direction="in"/>
    </method>
    <method name="CancelAuthentication">
      <arg type="s" name="cookie" direction="in"/>
    </method>
  </interface>
</node>"#;

/// One line from polkit-agent-helper-1 (its PAM conversation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperLine {
    /// PAM asks for a secret (a password): the prompt text.
    Secret(String),
    /// PAM asks for visible text.
    Text(String),
    /// PAM reports an error, or information.
    Error(String),
    Info(String),
    Success,
    Failure,
    Other,
}

/// Parse one helper output line.
pub fn parse_helper_line(line: &str) -> HelperLine {
    let line = line.trim_end_matches(['\n', '\r']);
    let rest = |prefix: &str| line.strip_prefix(prefix).map(|r| r.trim().to_owned());
    if let Some(p) = rest("PAM_PROMPT_ECHO_OFF") {
        HelperLine::Secret(p)
    } else if let Some(p) = rest("PAM_PROMPT_ECHO_ON") {
        HelperLine::Text(p)
    } else if let Some(m) = rest("PAM_ERROR_MSG") {
        HelperLine::Error(m)
    } else if let Some(m) = rest("PAM_TEXT_INFO") {
        HelperLine::Info(m)
    } else if line == "SUCCESS" {
        HelperLine::Success
    } else if line == "FAILURE" {
        HelperLine::Failure
    } else {
        HelperLine::Other
    }
}

/// GNOME's prompt text for a PAM prompt: "Password: " shows as "Password".
pub fn prompt_hint(prompt: &str) -> String {
    let p = prompt.trim().trim_end_matches([':', '：']).trim();
    if p.is_empty() {
        "Password".to_owned()
    } else {
        p.to_owned()
    }
}

/// Which identity to authenticate as, as GNOME picks it: the session's
/// user when polkit offers it, else root, else the first offered.
/// `identities` are the unix-user uids polkit offered.
pub fn pick_identity(identities: &[u32], own_uid: u32) -> Option<u32> {
    if identities.contains(&own_uid) {
        Some(own_uid)
    } else if identities.contains(&0) {
        Some(0)
    } else {
        identities.first().copied()
    }
}

/// (login name, display name) for `uid`.
fn user_names(uid: u32) -> Option<(String, String)> {
    // SAFETY: getpwuid returns static storage, read at once.
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return None;
        }
        let login = std::ffi::CStr::from_ptr((*pw).pw_name)
            .to_string_lossy()
            .into_owned();
        let gecos = if (*pw).pw_gecos.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr((*pw).pw_gecos)
                .to_string_lossy()
                .into_owned()
        };
        let display = crate::logic::display_name(&gecos, &login);
        Some((login, display))
    }
}

/// polkit's password helper.
fn helper_path() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("ROOST_POLKIT_HELPER") {
        return Some(p.into());
    }
    [
        "/usr/lib/polkit-1/polkit-agent-helper-1",
        "/usr/libexec/polkit-agent-helper-1",
        "/usr/lib/policykit-1/polkit-agent-helper-1",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .find(|p| p.exists())
}

/// One authentication in progress.
struct Request {
    cookie: String,
    invocation: gio::DBusMethodInvocation,
    user: String,
    helper: Option<gio::Subprocess>,
}

pub struct PolkitAgent {
    window: gtk::Window,
    message: gtk::Label,
    user_label: gtk::Label,
    entry: gtk::PasswordEntry,
    error: gtk::Label,
    info: gtk::Label,
    ok: gtk::Button,
    request: RefCell<Option<Request>>,
    /// The helper is waiting for the password.
    awaiting: Cell<bool>,
}

impl PolkitAgent {
    fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("roost-end-session");
        window.add_css_class("roost-polkit");
        window.set_title(Some("Authentication Required"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("roost-polkit"));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);

        // GNOME's prompt-dialog: a 28em modal card.
        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.add_css_class("modal-dialog");
        card.add_css_class("prompt-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(411, -1);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("modal-dialog-content-box");
        let header = gtk::Box::new(gtk::Orientation::Vertical, 18);
        let title = gtk::Label::new(Some("Authentication Required"));
        title.add_css_class("message-dialog-title");
        let message = gtk::Label::new(None);
        message.add_css_class("message-dialog-description");
        message.set_wrap(true);
        message.set_max_width_chars(1);
        message.set_hexpand(true);
        message.set_justify(gtk::Justification::Center);
        header.append(&title);
        header.append(&message);
        content.append(&header);
        // The user (avatar and name) and the password.
        let user_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        user_box.add_css_class("polkit-dialog-user-layout");
        let avatar = gtk::Image::from_icon_name("avatar-default-symbolic");
        avatar.add_css_class("polkit-user-icon");
        avatar.set_halign(gtk::Align::Center);
        let user_label = gtk::Label::new(None);
        user_label.add_css_class("polkit-dialog-user-label");
        user_box.append(&avatar);
        user_box.append(&user_label);
        content.append(&user_box);
        let entry = gtk::PasswordEntry::new();
        entry.add_css_class("prompt-dialog-password-entry");
        entry.set_halign(gtk::Align::Center);
        entry.set_property("placeholder-text", "Password");
        entry.update_property(&[gtk::accessible::Property::Label("Password")]);
        content.append(&entry);
        let error = gtk::Label::new(None);
        error.add_css_class("prompt-dialog-error-label");
        error.set_wrap(true);
        error.set_visible(false);
        let info = gtk::Label::new(None);
        info.add_css_class("prompt-dialog-info-label");
        info.set_wrap(true);
        info.set_visible(false);
        content.append(&error);
        content.append(&info);
        card.append(&content);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.add_css_class("modal-dialog-button-box");
        buttons.set_homogeneous(true);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("modal-dialog-button");
        let ok = gtk::Button::with_label("Authenticate");
        ok.add_css_class("modal-dialog-button");
        ok.set_sensitive(false);
        buttons.append(&cancel);
        buttons.append(&ok);
        card.append(&buttons);
        window.set_child(Some(&card));

        let agent = Rc::new(Self {
            window,
            message,
            user_label,
            entry,
            error,
            info,
            ok,
            request: RefCell::new(None),
            awaiting: Cell::new(false),
        });
        {
            let weak = Rc::downgrade(&agent);
            cancel.connect_clicked(move |_| {
                if let Some(a) = weak.upgrade() {
                    a.finish(false);
                }
            });
        }
        {
            let weak = Rc::downgrade(&agent);
            agent.ok.connect_clicked(move |_| {
                if let Some(a) = weak.upgrade() {
                    a.submit();
                }
            });
        }
        {
            let weak = Rc::downgrade(&agent);
            agent.entry.connect_activate(move |_| {
                if let Some(a) = weak.upgrade() {
                    a.submit();
                }
            });
        }
        {
            let ok = agent.ok.clone();
            agent
                .entry
                .connect_changed(move |e| ok.set_sensitive(!e.text().is_empty()));
        }
        let keys = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(&agent);
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    if let Some(a) = weak.upgrade() {
                        a.finish(false);
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        agent.window.add_controller(keys);
        agent
    }

    /// polkit's BeginAuthentication.
    fn begin(
        self: &Rc<Self>,
        message: &str,
        cookie: &str,
        identities: &[u32],
        invocation: gio::DBusMethodInvocation,
    ) {
        if self.request.borrow().is_some() {
            invocation.return_dbus_error(
                "org.freedesktop.PolicyKit1.Error.Failed",
                "an authentication is already in progress",
            );
            return;
        }
        // SAFETY: getuid never fails.
        let own = unsafe { libc::getuid() };
        let Some(uid) = pick_identity(identities, own) else {
            invocation.return_dbus_error("org.freedesktop.PolicyKit1.Error.Failed", "no identity");
            return;
        };
        let Some((login, display)) = user_names(uid) else {
            invocation.return_dbus_error("org.freedesktop.PolicyKit1.Error.Failed", "unknown user");
            return;
        };
        self.message.set_label(message);
        if uid == 0 {
            self.user_label.set_label("Administrator");
            self.user_label
                .add_css_class("polkit-dialog-user-root-label");
        } else {
            self.user_label.set_label(&display);
            self.user_label
                .remove_css_class("polkit-dialog-user-root-label");
        }
        self.error.set_visible(false);
        self.info.set_visible(false);
        self.entry.set_text("");
        *self.request.borrow_mut() = Some(Request {
            cookie: cookie.to_owned(),
            invocation,
            user: login,
            helper: None,
        });
        self.window.present();
        self.entry.grab_focus();
        self.start_helper();
    }

    /// Run polkit's helper for the request's user and follow its PAM
    /// conversation.
    fn start_helper(self: &Rc<Self>) {
        let (user, cookie) = match self.request.borrow().as_ref() {
            Some(r) => (r.user.clone(), r.cookie.clone()),
            None => return,
        };
        let Some(path) = helper_path() else {
            self.error
                .set_label("No authentication helper is installed.");
            self.error.set_visible(true);
            return;
        };
        let argv = [path.as_os_str(), std::ffi::OsStr::new(&user)];
        let proc = match gio::Subprocess::newv(
            &argv,
            gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE,
        ) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("roost-shell-gtk: polkit helper: {e}");
                return;
            }
        };
        if let Some(stdin) = proc.stdin_pipe() {
            let line = format!("{cookie}\n");
            let _ = stdin.write_all(line.as_bytes(), gio::Cancellable::NONE);
        }
        let Some(stdout) = proc.stdout_pipe() else {
            return;
        };
        if let Some(r) = self.request.borrow_mut().as_mut() {
            r.helper = Some(proc);
        }
        let reader = gio::DataInputStream::new(&stdout);
        self.read_line(reader);
    }

    fn read_line(self: &Rc<Self>, reader: gio::DataInputStream) {
        let weak = Rc::downgrade(self);
        reader.clone().read_line_utf8_async(
            glib::Priority::DEFAULT,
            gio::Cancellable::NONE,
            move |res| {
                let Some(agent) = weak.upgrade() else { return };
                let line = match res {
                    Ok(Some(line)) => line.to_string(),
                    // The helper went away without a verdict: a failure.
                    _ => "FAILURE".to_owned(),
                };
                match parse_helper_line(&line) {
                    HelperLine::Secret(prompt) | HelperLine::Text(prompt) => {
                        agent
                            .entry
                            .set_property("placeholder-text", prompt_hint(&prompt));
                        agent.entry.set_sensitive(true);
                        agent.entry.grab_focus();
                        agent.awaiting.set(true);
                        agent.read_line(reader);
                    }
                    HelperLine::Error(m) => {
                        agent.error.set_label(&m);
                        agent.error.set_visible(true);
                        agent.read_line(reader);
                    }
                    HelperLine::Info(m) => {
                        agent.info.set_label(&m);
                        agent.info.set_visible(true);
                        agent.read_line(reader);
                    }
                    HelperLine::Success => agent.finish(true),
                    HelperLine::Failure => {
                        // GNOME says so and starts over.
                        agent.awaiting.set(false);
                        agent.entry.set_text("");
                        agent.entry.set_sensitive(true);
                        agent.error.set_label(FAILED);
                        agent.error.set_visible(true);
                        if agent.request.borrow().is_some() {
                            agent.start_helper();
                        }
                    }
                    HelperLine::Other => agent.read_line(reader),
                }
            },
        );
    }

    fn submit(self: &Rc<Self>) {
        if !self.awaiting.get() {
            return;
        }
        let password = self.entry.text().to_string();
        if password.is_empty() {
            return;
        }
        let stdin = self
            .request
            .borrow()
            .as_ref()
            .and_then(|r| r.helper.as_ref())
            .and_then(|h| h.stdin_pipe());
        if let Some(stdin) = stdin {
            self.awaiting.set(false);
            // Let GtkText leave focus before making its entry insensitive.
            if let Some(root) = self.entry.root() {
                root.set_focus(gtk::Widget::NONE);
            }
            self.entry.set_sensitive(false);
            self.ok.set_sensitive(false);
            self.error.set_visible(false);
            let line = format!("{password}\n");
            let _ = stdin.write_all(line.as_bytes(), gio::Cancellable::NONE);
        }
        self.entry.set_text("");
    }

    /// End the request: authorized, or cancelled by the user.
    fn finish(&self, authorized: bool) {
        self.window.set_visible(false);
        self.entry.set_text("");
        self.awaiting.set(false);
        let Some(request) = self.request.borrow_mut().take() else {
            return;
        };
        if let Some(helper) = request.helper {
            helper.force_exit();
        }
        if authorized {
            request.invocation.return_value(None);
        } else {
            request.invocation.return_dbus_error(
                "org.freedesktop.PolicyKit1.Error.Cancelled",
                "The user cancelled the authentication",
            );
        }
    }

    /// polkit's CancelAuthentication.
    fn cancel(&self, cookie: &str) {
        let ours = self
            .request
            .borrow()
            .as_ref()
            .is_some_and(|r| r.cookie == cookie);
        if ours {
            self.finish(false);
        }
    }
}

/// The unix-user uids among polkit's identities.
fn unix_users(identities: &glib::Variant) -> Vec<u32> {
    (0..identities.n_children())
        .filter_map(|i| {
            let ident = identities.child_value(i);
            let kind: String = ident.child_value(0).get()?;
            if kind != "unix-user" {
                return None;
            }
            let details = glib::VariantDict::new(Some(&ident.child_value(1)));
            details.lookup::<u32>("uid").ok().flatten()
        })
        .collect()
}

/// This session's polkit subject: logind's session id.
fn session_subject() -> glib::Variant {
    let id = std::env::var("XDG_SESSION_ID").unwrap_or_default();
    let details: HashMap<String, glib::Variant> =
        HashMap::from([("session-id".to_owned(), id.to_variant())]);
    ("unix-session", details).to_variant()
}

/// Serve the agent and register it with polkit for this session.
pub fn start(app: &gtk::Application) {
    let agent = PolkitAgent::new(app);
    let node = match gio::DBusNodeInfo::for_xml(XML) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("roost-shell-gtk: polkit agent interface: {e}");
            return;
        }
    };
    gio::bus_get(gio::BusType::System, gio::Cancellable::NONE, move |conn| {
        let Ok(conn) = conn else {
            eprintln!("roost-shell-gtk: polkit agent: no system bus");
            return;
        };
        let Some(info) = node.lookup_interface("org.freedesktop.PolicyKit1.AuthenticationAgent")
        else {
            return;
        };
        let calls = agent.clone();
        let registered =
            conn.register_object(AGENT_PATH, &info)
                .method_call(move |_, _, _, _, method, params, invocation| match method {
                    "BeginAuthentication" => {
                        let message: String = params.child_value(1).get().unwrap_or_default();
                        let cookie: String = params.child_value(4).get().unwrap_or_default();
                        let users = unix_users(&params.child_value(5));
                        calls.begin(&message, &cookie, &users, invocation);
                    }
                    "CancelAuthentication" => {
                        let cookie: String = params.child_value(0).get().unwrap_or_default();
                        calls.cancel(&cookie);
                        invocation.return_value(None);
                    }
                    _ => invocation
                        .return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
                })
                .build();
        if let Err(e) = registered {
            eprintln!("roost-shell-gtk: polkit agent object: {e}");
            return;
        }
        // Register now, and again whenever polkit (re)starts, as GNOME's
        // agent does. A NameOwnerChanged watch: gio's name watcher hands
        // its callbacks a NULL connection when the bus goes away, which
        // its Rust binding unwraps.
        let conn2 = conn.clone();
        crate::services::watch_name(&conn, AUTHORITY, move |owned| {
            if owned {
                register(&conn2);
            }
        });
    });
}

/// Register the agent with polkit's Authority for this session.
fn register(conn: &gio::DBusConnection) {
    let locale = std::env::var("LANG").unwrap_or_else(|_| "C".to_owned());
    conn.call(
        Some(AUTHORITY),
        AUTHORITY_PATH,
        AUTHORITY_IFACE,
        "RegisterAuthenticationAgent",
        Some(&glib::Variant::tuple_from_iter([
            session_subject(),
            locale.to_variant(),
            AGENT_PATH.to_variant(),
        ])),
        None,
        gio::DBusCallFlags::NONE,
        10_000,
        gio::Cancellable::NONE,
        |res| match res {
            Ok(_) => eprintln!("roost-shell-gtk: polkit agent registered"),
            Err(e) => eprintln!("roost-shell-gtk: polkit agent not registered: {e}"),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_lines_parse() {
        assert_eq!(
            parse_helper_line("PAM_PROMPT_ECHO_OFF Password: \n"),
            HelperLine::Secret("Password:".into())
        );
        assert_eq!(
            parse_helper_line("PAM_ERROR_MSG nope"),
            HelperLine::Error("nope".into())
        );
        assert_eq!(parse_helper_line("SUCCESS"), HelperLine::Success);
        assert_eq!(parse_helper_line("FAILURE\n"), HelperLine::Failure);
        assert_eq!(prompt_hint("Password: "), "Password");
        assert_eq!(prompt_hint(""), "Password");
    }

    #[test]
    fn registration_matches_polkits_signature() {
        let args = glib::Variant::tuple_from_iter([
            session_subject(),
            "C".to_variant(),
            AGENT_PATH.to_variant(),
        ]);
        assert_eq!(args.type_().as_str(), "((sa{sv})ss)");
    }

    #[test]
    fn identities_are_picked_like_gnome() {
        assert_eq!(pick_identity(&[0, 1000], 1000), Some(1000));
        assert_eq!(pick_identity(&[1001, 0], 1000), Some(0));
        assert_eq!(pick_identity(&[1001], 1000), Some(1001));
        assert_eq!(pick_identity(&[], 1000), None);
    }
}
