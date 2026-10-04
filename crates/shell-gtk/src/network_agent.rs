//! GNOME's NetworkManager secret agent (networkAgent.js): registered
//! with NetworkManager's AgentManager as org.gnome.Shell.NetworkAgent,
//! it asks for a Wi-Fi network's password (WPA/SAE) or WEP key in
//! GNOME's "Authentication required" dialog when NetworkManager needs
//! one to connect. 802.1X secrets go to Settings, as before.

use std::cell::RefCell;
use std::rc::Rc;

use gio::prelude::*;
use gtk::prelude::*;
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

const NM: &str = "org.freedesktop.NetworkManager";
const AGENT_MANAGER_PATH: &str = "/org/freedesktop/NetworkManager/AgentManager";
const AGENT_MANAGER: &str = "org.freedesktop.NetworkManager.AgentManager";
const AGENT_PATH: &str = "/org/freedesktop/NetworkManager/SecretAgent";
const AGENT_ID: &str = "org.gnome.Shell.NetworkAgent";
const NO_SECRETS: &str = "org.freedesktop.NetworkManager.SecretAgent.NoSecrets";
const USER_CANCELED: &str = "org.freedesktop.NetworkManager.SecretAgent.UserCanceled";
/// NMSecretAgentGetSecretsFlags: the agent may prompt.
const ALLOW_INTERACTION: u32 = 0x1;

const XML: &str = r#"
<node>
  <interface name="org.freedesktop.NetworkManager.SecretAgent">
    <method name="GetSecrets">
      <arg type="a{sa{sv}}" name="connection" direction="in"/>
      <arg type="o" name="connection_path" direction="in"/>
      <arg type="s" name="setting_name" direction="in"/>
      <arg type="as" name="hints" direction="in"/>
      <arg type="u" name="flags" direction="in"/>
      <arg type="a{sa{sv}}" name="secrets" direction="out"/>
    </method>
    <method name="CancelGetSecrets">
      <arg type="o" name="connection_path" direction="in"/>
      <arg type="s" name="setting_name" direction="in"/>
    </method>
    <method name="SaveSecrets">
      <arg type="a{sa{sv}}" name="connection" direction="in"/>
      <arg type="o" name="connection_path" direction="in"/>
    </method>
    <method name="DeleteSecrets">
      <arg type="a{sa{sv}}" name="connection" direction="in"/>
      <arg type="o" name="connection_path" direction="in"/>
    </method>
  </interface>
</node>
"#;

/// The one secret GNOME's dialog asks for on a Wi-Fi network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WifiSecret {
    /// WPA/WPA2/WPA3 personal: `psk`.
    Psk,
    /// Static WEP: `wep-key<idx>`, of NM's key type (1 hex/ascii key,
    /// 2 passphrase).
    Wep { index: u32, key_type: u32 },
}

impl WifiSecret {
    /// From the connection's 802-11-wireless-security key-mgmt
    /// (networkAgent.js `_getWirelessSecrets`); `None` for 802.1X.
    pub fn for_key_mgmt(key_mgmt: &str, wep_index: u32, wep_type: u32) -> Option<Self> {
        match key_mgmt {
            "wpa-none" | "wpa-psk" | "sae" => Some(Self::Psk),
            "none" => Some(Self::Wep {
                index: wep_index,
                key_type: wep_type,
            }),
            _ => None,
        }
    }

    /// The secret's key in the reply and its entry's hint.
    pub fn key(&self) -> String {
        match self {
            Self::Psk => "psk".to_owned(),
            Self::Wep { index, .. } => format!("wep-key{index}"),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Psk => "Password",
            Self::Wep { .. } => "Key",
        }
    }

    /// GNOME's validators (`_validateWpaPsk`, `_validateStaticWep`).
    pub fn valid(&self, value: &str) -> bool {
        let hex = |v: &str| v.chars().all(|c| c.is_ascii_hexdigit());
        match self {
            Self::Psk => {
                if value.len() == 64 {
                    hex(value)
                } else {
                    (8..=63).contains(&value.len())
                }
            }
            Self::Wep { key_type: 1, .. } => match value.len() {
                10 | 26 => hex(value),
                5 | 13 => value.chars().all(|c| c.is_ascii_alphabetic()),
                _ => false,
            },
            Self::Wep { key_type: 2, .. } => value.len() <= 64,
            Self::Wep { .. } => true,
        }
    }
}

/// The dialog's description, with GNOME's curly quotes.
pub fn wifi_message(ssid: &str) -> String {
    format!("Passwords or encryption keys are required to access the wireless network \u{201c}{ssid}\u{201d}")
}

struct Pending {
    invocation: gio::DBusMethodInvocation,
    connection_path: String,
    setting: String,
    secret: WifiSecret,
}

/// The agent and its dialog.
pub struct NetworkAgent {
    window: gtk::Window,
    message: gtk::Label,
    entry: gtk::PasswordEntry,
    ok: gtk::Button,
    pending: RefCell<Option<Pending>>,
}

impl NetworkAgent {
    fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("roost-end-session");
        window.add_css_class("roost-network-agent");
        window.set_title(Some("Authentication required"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("roost-network-agent"));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        // GNOME's prompt-dialog: title, description, the entry, then
        // Cancel and Connect.
        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.add_css_class("modal-dialog");
        card.add_css_class("prompt-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(411, -1);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("modal-dialog-content-box");
        let title = gtk::Label::new(Some("Authentication required"));
        title.add_css_class("message-dialog-title");
        let message = gtk::Label::new(None);
        message.add_css_class("message-dialog-description");
        message.set_wrap(true);
        message.set_max_width_chars(1);
        message.set_hexpand(true);
        message.set_justify(gtk::Justification::Center);
        content.append(&title);
        content.append(&message);
        let entry = gtk::PasswordEntry::new();
        entry.add_css_class("prompt-dialog-password-entry");
        entry.set_halign(gtk::Align::Center);
        entry.set_show_peek_icon(true);
        content.append(&entry);
        card.append(&content);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.add_css_class("modal-dialog-button-box");
        buttons.set_homogeneous(true);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("modal-dialog-button");
        let ok = gtk::Button::with_label("Connect");
        ok.add_css_class("modal-dialog-button");
        ok.set_sensitive(false);
        buttons.append(&cancel);
        buttons.append(&ok);
        card.append(&buttons);
        window.set_child(Some(&card));
        let agent = Rc::new(Self {
            window,
            message,
            entry,
            ok,
            pending: RefCell::new(None),
        });
        {
            let weak = Rc::downgrade(&agent);
            cancel.connect_clicked(move |_| {
                if let Some(a) = weak.upgrade() {
                    a.reply(None);
                }
            });
        }
        let submit = {
            let weak = Rc::downgrade(&agent);
            move || {
                if let Some(a) = weak.upgrade() {
                    if a.ok.is_sensitive() {
                        a.reply(Some(a.entry.text().to_string()));
                    }
                }
            }
        };
        {
            let submit = submit.clone();
            agent.ok.connect_clicked(move |_| submit());
        }
        agent.entry.connect_activate(move |_| submit());
        {
            let weak = Rc::downgrade(&agent);
            agent.entry.connect_changed(move |e| {
                if let Some(a) = weak.upgrade() {
                    let valid = a
                        .pending
                        .borrow()
                        .as_ref()
                        .is_some_and(|p| p.secret.valid(&e.text()));
                    a.ok.set_sensitive(valid);
                }
            });
        }
        let keys = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(&agent);
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    if let Some(a) = weak.upgrade() {
                        a.reply(None);
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        agent.window.add_controller(keys);
        agent
    }

    /// NetworkManager's GetSecrets.
    fn get_secrets(
        &self,
        connection: &glib::Variant,
        connection_path: String,
        setting: String,
        flags: u32,
        invocation: gio::DBusMethodInvocation,
    ) {
        let section = |name: &str| {
            connection.iter().find_map(|entry| {
                (entry.child_value(0).str() == Some(name))
                    .then(|| glib::VariantDict::new(Some(&entry.child_value(1))))
            })
        };
        let ssid = section("802-11-wireless")
            .and_then(|w| w.lookup_value("ssid", None))
            .and_then(|v| v.get::<Vec<u8>>())
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        let security = section("802-11-wireless-security");
        let text = |k: &str| {
            security
                .as_ref()
                .and_then(|s| s.lookup_value(k, None))
                .and_then(|v| v.get::<String>())
        };
        let number = |k: &str| {
            security
                .as_ref()
                .and_then(|s| s.lookup_value(k, None))
                .and_then(|v| v.get::<u32>())
                .unwrap_or(0)
        };
        let secret = (setting == "802-11-wireless-security")
            .then(|| {
                WifiSecret::for_key_mgmt(
                    &text("key-mgmt").unwrap_or_default(),
                    number("wep-tx-keyidx"),
                    number("wep-key-type"),
                )
            })
            .flatten();
        let (Some(ssid), Some(secret)) = (ssid, secret) else {
            invocation.return_dbus_error(NO_SECRETS, "only Wi-Fi passwords are asked for here");
            return;
        };
        if flags & ALLOW_INTERACTION == 0 {
            invocation.return_dbus_error(NO_SECRETS, "no stored secrets, and no prompting allowed");
            return;
        }
        // A newer request replaces an open one.
        self.reply(None);
        self.message.set_text(&wifi_message(&ssid));
        self.entry.set_text("");
        self.entry.set_property("placeholder-text", secret.label());
        self.entry
            .update_property(&[gtk::accessible::Property::Label(secret.label())]);
        self.ok.set_sensitive(false);
        *self.pending.borrow_mut() = Some(Pending {
            invocation,
            connection_path,
            setting,
            secret,
        });
        self.window.present();
        self.entry.grab_focus();
    }

    /// Answer the open request: the typed secret, or cancelled.
    fn reply(&self, value: Option<String>) {
        let Some(p) = self.pending.borrow_mut().take() else {
            return;
        };
        self.window.set_visible(false);
        self.entry.set_text("");
        match value {
            Some(value) => {
                let inner = glib::VariantDict::new(None);
                inner.insert_value(&p.secret.key(), &value.to_variant());
                // a{sa{sv}}: the setting's name over its secrets.
                let reply = glib::Variant::from_dict_entry(&p.setting.to_variant(), &inner.end());
                let dict = glib::Variant::array_from_iter_with_type(
                    glib::VariantTy::new("{sa{sv}}").expect("valid type"),
                    [reply],
                );
                p.invocation
                    .return_value(Some(&glib::Variant::tuple_from_iter([dict])));
            }
            None => p
                .invocation
                .return_dbus_error(USER_CANCELED, "the user cancelled"),
        }
    }

    /// NetworkManager's CancelGetSecrets: close the dialog.
    fn cancel(&self, connection_path: &str, setting: &str) {
        let matches = self
            .pending
            .borrow()
            .as_ref()
            .is_some_and(|p| p.connection_path == connection_path && p.setting == setting);
        if matches {
            self.reply(None);
        }
    }
}

/// Serve the agent on the system bus and register it with
/// NetworkManager, again whenever NetworkManager (re)starts.
pub fn start(app: &gtk::Application) {
    let agent = NetworkAgent::new(app);
    let node = match gio::DBusNodeInfo::for_xml(XML) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("roost-shell-gtk: network agent interface: {e}");
            return;
        }
    };
    gio::bus_get(gio::BusType::System, gio::Cancellable::NONE, move |conn| {
        let Ok(conn) = conn else {
            return;
        };
        let Some(info) = node.lookup_interface("org.freedesktop.NetworkManager.SecretAgent") else {
            return;
        };
        let calls = agent.clone();
        let registered = conn
            .register_object(AGENT_PATH, &info)
            .method_call(move |_, sender, _, _, method, params, invocation| {
                // Only NetworkManager may ask.
                if !sender_is_nm(sender) {
                    invocation.return_dbus_error(
                        "org.freedesktop.DBus.Error.AccessDenied",
                        "only NetworkManager may ask for secrets",
                    );
                    return;
                }
                match method {
                    "GetSecrets" => {
                        let path = params.child_value(1).str().unwrap_or_default().to_owned();
                        let setting = params.child_value(2).str().unwrap_or_default().to_owned();
                        let flags = params.child_value(4).get::<u32>().unwrap_or(0);
                        calls.get_secrets(&params.child_value(0), path, setting, flags, invocation);
                    }
                    "CancelGetSecrets" => {
                        let path = params.child_value(0).str().unwrap_or_default().to_owned();
                        let setting = params.child_value(1).str().unwrap_or_default().to_owned();
                        calls.cancel(&path, &setting);
                        invocation.return_value(None);
                    }
                    // NetworkManager keeps system-owned secrets itself.
                    "SaveSecrets" | "DeleteSecrets" => invocation.return_value(None),
                    _ => invocation
                        .return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
                }
            })
            .build();
        if let Err(e) = registered {
            eprintln!("roost-shell-gtk: network agent object: {e}");
            return;
        }
        let conn2 = conn.clone();
        let owner = NM_OWNER.with(|o| o.clone());
        crate::services::watch_name(&conn, NM, move |owned| {
            if !owned {
                owner.borrow_mut().take();
                return;
            }
            register(&conn2, owner.clone());
        });
    });
}

thread_local! {
    /// NetworkManager's unique name while it runs: the only sender the
    /// agent answers.
    static NM_OWNER: Rc<RefCell<Option<String>>> = Rc::default();
}

fn sender_is_nm(sender: Option<&str>) -> bool {
    NM_OWNER.with(|o| o.borrow().as_deref().is_some_and(|nm| Some(nm) == sender))
}

fn register(conn: &gio::DBusConnection, owner: Rc<RefCell<Option<String>>>) {
    conn.call(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetNameOwner",
        Some(&(NM,).to_variant()),
        glib::VariantTy::new("(s)").ok(),
        gio::DBusCallFlags::NONE,
        5000,
        gio::Cancellable::NONE,
        move |reply| {
            *owner.borrow_mut() = reply.ok().and_then(|v| v.child_value(0).get::<String>());
        },
    );
    conn.call(
        Some(NM),
        AGENT_MANAGER_PATH,
        AGENT_MANAGER,
        "Register",
        Some(&(AGENT_ID,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        10_000,
        gio::Cancellable::NONE,
        |res| match res {
            Ok(_) => eprintln!("roost-shell-gtk: network agent registered"),
            Err(e) => eprintln!("roost-shell-gtk: network agent not registered: {e}"),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_follow_key_management_like_gnome() {
        assert_eq!(
            WifiSecret::for_key_mgmt("wpa-psk", 0, 0),
            Some(WifiSecret::Psk)
        );
        assert_eq!(WifiSecret::for_key_mgmt("sae", 0, 0), Some(WifiSecret::Psk));
        assert_eq!(
            WifiSecret::for_key_mgmt("none", 2, 1).map(|s| s.key()),
            Some("wep-key2".to_owned())
        );
        assert_eq!(WifiSecret::for_key_mgmt("wpa-eap", 0, 0), None);
        assert_eq!(WifiSecret::Psk.label(), "Password");
    }

    #[test]
    fn validators_follow_gnome() {
        let psk = WifiSecret::Psk;
        assert!(!psk.valid("short"));
        assert!(psk.valid("eight ch"));
        assert!(psk.valid(&"a".repeat(63)));
        assert!(psk.valid(&"0123456789abcdef".repeat(4)));
        assert!(!psk.valid(&"g".repeat(64)));
        let wep = WifiSecret::Wep {
            index: 0,
            key_type: 1,
        };
        assert!(wep.valid("0123456789"));
        assert!(wep.valid("abcde"));
        assert!(!wep.valid("abc12"));
        assert!(!wep.valid("0123"));
    }

    #[test]
    fn message_quotes_the_network_like_gnome() {
        assert_eq!(
            wifi_message("Home"),
            "Passwords or encryption keys are required to access the wireless network \u{201c}Home\u{201d}"
        );
    }
}
