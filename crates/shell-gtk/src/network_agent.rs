//! GNOME's NetworkManager secret agent (networkAgent.js): registered
//! with NetworkManager's AgentManager as org.gnome.Shell.NetworkAgent,
//! it asks for a Wi-Fi network's password (WPA/SAE) or WEP key in
//! GNOME's "Authentication required" dialog when NetworkManager needs
//! one to connect. It also handles 802.1X, mobile PINs and VPN plugin
//! external-UI prompts, and stores agent-owned secrets through libsecret.

use crate::network_secrets::{self, Field};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
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
    connection: glib::Variant,
    connection_path: String,
    setting: String,
    fields: Vec<Field>,
    values: BTreeMap<String, String>,
}

/// The agent owns one modal request; cancellation also invalidates async work.
pub struct NetworkAgent {
    window: gtk::Window,
    title: gtk::Label,
    message: gtk::Label,
    content: gtk::Box,
    entries: RefCell<Vec<gtk::Entry>>,
    ok: gtk::Button,
    pending: RefCell<Option<Pending>>,
    generation: Cell<u64>,
    replying: RefCell<Option<(String, String)>>,
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
            title,
            message,
            content,
            entries: RefCell::default(),
            ok,
            pending: RefCell::default(),
            generation: Cell::new(0),
            replying: RefCell::default(),
        });
        let weak = Rc::downgrade(&agent);
        cancel.connect_clicked(move |_| {
            if let Some(a) = weak.upgrade() {
                a.reply(false);
            }
        });
        let weak = Rc::downgrade(&agent);
        agent.ok.connect_clicked(move |_| {
            if let Some(a) = weak.upgrade() {
                a.reply(true);
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&agent);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(a) = weak.upgrade() {
                    a.reply(false);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        agent.window.add_controller(keys);
        agent
    }

    fn get_secrets(
        self: &Rc<Self>,
        connection: glib::Variant,
        connection_path: String,
        setting: String,
        hints: Vec<String>,
        flags: u32,
        invocation: gio::DBusMethodInvocation,
    ) {
        self.reply(false);
        let generation = self.generation.get();
        *self.pending.borrow_mut() = Some(Pending {
            invocation,
            connection: connection.clone(),
            connection_path,
            setting: setting.clone(),
            fields: vec![],
            values: BTreeMap::new(),
        });
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let prompt = if setting == "vpn" {
                network_secrets::vpn(&connection, &hints, flags).await
            } else {
                network_secrets::fields(&connection, &setting, &hints)
                    .map(|fields| {
                        let ssid = network_secrets::section(&connection, "802-11-wireless")
                            .and_then(|s| s.lookup_value("ssid", None))
                            .and_then(|v| v.get::<Vec<u8>>())
                            .map(|s| String::from_utf8_lossy(&s).into_owned());
                        let conn = network_secrets::section(&connection, "connection");
                        network_secrets::VpnPrompt {
                            title: "Authentication required".into(),
                            message: ssid.map(|s| wifi_message(&s)).unwrap_or_else(|| {
                                format!(
                                    "Authentication is required to connect to “{}”",
                                    network_secrets::text(conn.as_ref(), "id")
                                )
                            }),
                            fields,
                            values: BTreeMap::new(),
                        }
                    })
                    .ok_or_else(|| "unsupported NetworkManager secret request".to_owned())
            };
            let Some(agent) = weak.upgrade() else { return };
            if agent.generation.get() != generation {
                return;
            }
            let mut prompt = match prompt {
                Ok(p) => p,
                Err(error) => {
                    agent.fail(&error);
                    return;
                }
            };
            let conn = network_secrets::section(&connection, "connection");
            let uuid = network_secrets::text(conn.as_ref(), "uuid");
            for field in &mut prompt.fields {
                if field.password {
                    if flags & 2 != 0 {
                        field.value.clear();
                    } else if let Some(key) = field.key.as_deref() {
                        if let Some(stored) = network_secrets::lookup(&uuid, &setting, key).await {
                            field.value = stored;
                        }
                    }
                }
            }
            if agent.generation.get() != generation {
                return;
            }
            let complete = prompt.fields.iter().all(|f| f.valid(&f.value));
            {
                let mut request = agent.pending.borrow_mut();
                let Some(pending) = request.as_mut() else {
                    return;
                };
                pending.fields = prompt.fields;
                pending.values = prompt.values;
            }
            // Interaction requested: show even prefilled fields, as GNOME does.
            if flags & ALLOW_INTERACTION == 0 {
                if complete {
                    agent.reply_values();
                } else {
                    agent.fail("no stored secrets, and no prompting allowed");
                }
            } else if agent
                .pending
                .borrow()
                .as_ref()
                .is_some_and(|p| p.fields.is_empty())
            {
                agent.reply_values();
            } else {
                agent.show(&prompt.title, &prompt.message);
            }
        });
    }
    fn show(self: &Rc<Self>, title: &str, message: &str) {
        self.title.set_text(title);
        self.window.set_title(Some(title));
        self.message.set_text(message);
        for entry in self.entries.take() {
            self.content.remove(&entry);
        }
        let Some(pending) = self.pending.borrow().as_ref().map(|p| p.fields.clone()) else {
            return;
        };
        let mut entries = vec![];
        for field in &pending {
            let entry = gtk::Entry::new();
            entry.add_css_class("prompt-dialog-password-entry");
            entry.set_halign(gtk::Align::Center);
            entry.set_placeholder_text(Some(&field.label));
            entry.set_text(&field.value);
            entry.set_visibility(!field.password);
            entry.set_editable(field.key.is_some());
            entry.set_can_focus(field.key.is_some());
            entry.update_property(&[gtk::accessible::Property::Label(&field.label)]);
            if field.password {
                entry.set_icon_from_icon_name(
                    gtk::EntryIconPosition::Secondary,
                    Some("view-reveal-symbolic"),
                );
                entry.connect_icon_press(|e, _| {
                    e.set_visibility(!gtk::prelude::EntryExt::is_visible(e))
                });
            }
            let weak = Rc::downgrade(self);
            entry.connect_changed(move |_| {
                if let Some(a) = weak.upgrade() {
                    a.validate();
                }
            });
            let weak = Rc::downgrade(self);
            entry.connect_activate(move |_| {
                if let Some(a) = weak.upgrade() {
                    a.reply(true);
                }
            });
            self.content.append(&entry);
            entries.push(entry);
        }
        *self.entries.borrow_mut() = entries;
        self.validate();
        self.window.present();
        if let Some(entry) = self.entries.borrow().iter().find(|e| e.is_editable()) {
            entry.grab_focus();
        }
    }
    fn validate(&self) {
        let pending = self.pending.borrow();
        self.ok.set_sensitive(pending.as_ref().is_some_and(|p| {
            p.fields
                .iter()
                .zip(self.entries.borrow().iter())
                .all(|(f, e)| f.valid(&e.text()))
        }));
    }
    fn reply(self: &Rc<Self>, submit: bool) {
        if submit {
            if !self.ok.is_sensitive() {
                return;
            }
            if let Some(pending) = self.pending.borrow_mut().as_mut() {
                for (field, entry) in pending.fields.iter_mut().zip(self.entries.borrow().iter()) {
                    field.value = entry.text().to_string();
                }
            }
            self.reply_values();
        } else {
            self.generation.set(self.generation.get() + 1);
            if let Some(p) = self.pending.borrow_mut().take() {
                p.invocation
                    .return_dbus_error(USER_CANCELED, "the user cancelled");
            }
            self.clear();
        }
    }
    fn clear(&self) {
        self.window.set_visible(false);
        for entry in self.entries.borrow().iter() {
            entry.set_text("");
        }
    }
    fn fail(&self, error: &str) {
        if let Some(p) = self.pending.borrow_mut().take() {
            p.invocation.return_dbus_error(NO_SECRETS, error);
        }
        self.clear();
    }
    fn reply_values(self: &Rc<Self>) {
        let Some(mut p) = self.pending.borrow_mut().take() else {
            return;
        };
        self.clear();
        for f in p.fields {
            if let Some(key) = f.key {
                p.values.insert(key, f.value);
            }
        }
        let generation = self.generation.get();
        *self.replying.borrow_mut() = Some((p.connection_path.clone(), p.setting.clone()));
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let conn = network_secrets::section(&p.connection, "connection");
            let uuid = network_secrets::text(conn.as_ref(), "uuid");
            let id = network_secrets::text(conn.as_ref(), "id");
            for (key, value) in &p.values {
                if network_secrets::agent_owned(&p.connection, &p.setting, key) {
                    let stored = network_secrets::store(&uuid, &id, &p.setting, key, value).await;
                    if weak
                        .upgrade()
                        .is_none_or(|a| a.generation.get() != generation)
                    {
                        p.invocation
                            .return_dbus_error(USER_CANCELED, "the request was cancelled");
                        return;
                    }
                    if let Err(error) = stored {
                        if let Some(agent) = weak.upgrade() {
                            agent.replying.borrow_mut().take();
                        }
                        p.invocation.return_dbus_error(NO_SECRETS, &error);
                        return;
                    }
                }
            }
            if let Some(agent) = weak.upgrade() {
                if agent.generation.get() != generation {
                    p.invocation
                        .return_dbus_error(USER_CANCELED, "the request was cancelled");
                    return;
                }
                agent.replying.borrow_mut().take();
            } else {
                p.invocation
                    .return_dbus_error(USER_CANCELED, "the agent stopped");
                return;
            }
            let inner = glib::VariantDict::new(None);
            if p.setting == "vpn" {
                inner.insert_value("secrets", &p.values.to_variant());
            } else {
                for (key, value) in p.values {
                    inner.insert_value(&key, &value.to_variant());
                }
            }
            let entry = glib::Variant::from_dict_entry(&p.setting.to_variant(), &inner.end());
            let dict = glib::Variant::array_from_iter_with_type(
                glib::VariantTy::new("{sa{sv}}").expect("valid type"),
                [entry],
            );
            p.invocation
                .return_value(Some(&glib::Variant::tuple_from_iter([dict])));
        });
    }
    fn cancel(self: &Rc<Self>, connection_path: &str, setting: &str) {
        if self
            .pending
            .borrow()
            .as_ref()
            .is_some_and(|p| p.connection_path == connection_path && p.setting == setting)
            || self
                .replying
                .borrow()
                .as_ref()
                .is_some_and(|(path, name)| path == connection_path && name == setting)
        {
            self.reply(false);
        }
    }
}

/// Save/DeleteSecrets use the same libsecret attributes as GNOME's agent.
fn keyring_method(connection: glib::Variant, delete: bool, invocation: gio::DBusMethodInvocation) {
    glib::MainContext::default().spawn_local(async move {
        let conn = network_secrets::section(&connection, "connection");
        let uuid = network_secrets::text(conn.as_ref(), "uuid");
        let id = network_secrets::text(conn.as_ref(), "id");
        for entry in connection.iter() {
            let setting = entry.child_value(0).str().unwrap_or_default().to_owned();
            if delete {
                if let Err(error) = network_secrets::delete(&uuid, &setting).await {
                    invocation.return_dbus_error(NO_SECRETS, &error);
                    return;
                }
                continue;
            }
            let values = if setting == "vpn" {
                glib::VariantDict::new(Some(&entry.child_value(1)))
                    .lookup_value("secrets", None)
                    .and_then(|v| v.get::<BTreeMap<String, String>>())
                    .unwrap_or_default()
            } else {
                entry
                    .child_value(1)
                    .iter()
                    .filter_map(|e| {
                        Some((
                            e.child_value(0).str()?.to_owned(),
                            e.child_value(1).as_variant()?.get::<String>()?,
                        ))
                    })
                    .collect()
            };
            for (key, value) in values {
                if network_secrets::agent_owned(&connection, &setting, &key) {
                    if let Err(error) =
                        network_secrets::store(&uuid, &id, &setting, &key, &value).await
                    {
                        invocation.return_dbus_error(NO_SECRETS, &error);
                        return;
                    }
                }
            }
        }
        invocation.return_value(None);
    });
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
                        let hints = params
                            .child_value(3)
                            .get::<Vec<String>>()
                            .unwrap_or_default();
                        calls.get_secrets(
                            params.child_value(0),
                            path,
                            setting,
                            hints,
                            flags,
                            invocation,
                        );
                    }
                    "CancelGetSecrets" => {
                        let path = params.child_value(0).str().unwrap_or_default().to_owned();
                        let setting = params.child_value(1).str().unwrap_or_default().to_owned();
                        calls.cancel(&path, &setting);
                        invocation.return_value(None);
                    }
                    "SaveSecrets" | "DeleteSecrets" => {
                        keyring_method(params.child_value(0), method == "DeleteSecrets", invocation)
                    }
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
