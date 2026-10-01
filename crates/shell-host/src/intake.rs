//! Notification intake: the `org.freedesktop.Notifications` bus edge.
//!
//! The shell owns the desktop-notifications name (a Roost session runs
//! no other daemon, so nothing fights for it) and files wire arrivals
//! into the shared [`NotificationCenter`]. Banner presses route back
//! through [`NotificationBus::invoke_action`] and
//! [`NotificationBus::dismiss_banner`], which report to the sending app
//! as `ActionInvoked` / `NotificationClosed` signals.
//!
//! The core ([`NotificationCenter`], queueing, paint) is wire-free. This
//! D-Bus edge mirrors [`WatcherBus`](crate::watcher::WatcherBus): the
//! panel's slow tick calls [`NotificationBus::ensure`], and a taken
//! name (or no bus) reads as no intake — never a hang. Method calls
//! dispatch on zbus worker threads straight into the shared center.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::object_server::SignalEmitter;

use crate::notifications::{NotificationAction, NotificationCenter, NotificationError, Urgency};

/// Well-known notifications name (freedesktop Desktop Notifications).
pub const NOTIFICATIONS_NAME: &str = "org.freedesktop.Notifications";
/// Fixed object path for the notifications bus object.
pub const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";

/// Pair a freedesktop `actions` list (`[id, label, ...]`) into
/// invokable actions. A trailing odd entry carries no label and is
/// dropped.
fn pair_actions(actions: &[String]) -> Vec<NotificationAction> {
    actions
        .chunks(2)
        .filter_map(|pair| match pair {
            [id, label] => Some(NotificationAction {
                id: id.clone(),
                label: label.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Urgency from the `urgency` hint byte (0 low, 1 normal, 2
/// critical). A missing or mistyped hint reads as normal.
fn urgency_hint(hints: &HashMap<String, zbus::zvariant::OwnedValue>) -> Urgency {
    let level: Option<u8> = hints
        .get("urgency")
        .and_then(|hint| u8::try_from(hint).ok());
    match level {
        Some(0) => Urgency::Low,
        Some(2..=u8::MAX) => Urgency::Critical,
        _ => Urgency::Normal,
    }
}

/// The `org.freedesktop.Notifications` object: wire calls become
/// center mutations. Banner-side presses never arrive here as method
/// calls — they go through [`NotificationBus`], which mutates and
/// then emits the matching signal.
#[derive(Debug)]
struct Notifications {
    center: Arc<Mutex<NotificationCenter>>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Notifications {
    /// File a notification; returns the store id (also the outward
    /// id the app uses to replace or close it). A poisoned center
    /// reads as id zero, which no real notification carries.
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, zbus::zvariant::OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        let replaces = (replaces_id != 0).then_some(replaces_id as u64);
        self.center
            .lock()
            .map(|mut center| {
                center.notify(
                    &app_name,
                    &summary,
                    &body,
                    pair_actions(&actions),
                    urgency_hint(&hints),
                    replaces,
                )
            })
            .unwrap_or(0) as u32
    }

    /// Close a notification by id. Unknown ids are ignored: the
    /// caller already forgot the banner.
    fn close_notification(&self, id: u32) {
        let _ = self
            .center
            .lock()
            .map(|mut center| center.dismiss(id as u64));
    }

    /// Server capabilities: banner actions plus queue persistence.
    fn get_capabilities(&self) -> Vec<String> {
        vec!["actions".to_owned(), "persistence".to_owned()]
    }

    /// Banner press fired an action key (emitted by
    /// [`NotificationBus::invoke_action`]).
    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: String,
    ) -> zbus::Result<()>;

    /// A banner left the queue (emitted by
    /// [`NotificationBus::dismiss_banner`]).
    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}

/// D-Bus edge behind the notification center: owns the
/// notifications role when the name is free. The served object and
/// the panel share one center, so wire arrivals show up in banners
/// without a copy.
pub struct NotificationBus {
    conn: Option<zbus::blocking::Connection>,
    center: Arc<Mutex<NotificationCenter>>,
}

impl NotificationBus {
    /// Disconnected edge over a shared center;
    /// [`NotificationBus::ensure`] connects.
    pub fn new(center: Arc<Mutex<NotificationCenter>>) -> Self {
        Self { conn: None, center }
    }

    /// The shared center behind the edge (the panel paints from it).
    pub fn center(&self) -> Arc<Mutex<NotificationCenter>> {
        self.center.clone()
    }

    /// Connect the session bus, serve the notifications object, and
    /// take the notifications name without queueing. Returns true
    /// while we own the role. A taken name (or no bus) reads as no
    /// intake: banners stay empty and nothing else changes.
    pub fn ensure(&mut self) -> bool {
        self.connect(None)
    }

    /// Serve over an explicit bus address (`None` means the session
    /// bus). Tests point this at a private daemon; production passes
    /// `None` through [`NotificationBus::ensure`].
    fn connect(&mut self, address: Option<&str>) -> bool {
        if self.conn.is_some() {
            return true;
        }
        let builder = match address {
            Some(address) => zbus::blocking::connection::Builder::address(address),
            None => zbus::blocking::connection::Builder::session(),
        };
        let builder = match builder {
            Ok(builder) => builder,
            Err(e) => {
                eprintln!("roost-shell-host: notification bus unavailable: {e}");
                return false;
            }
        };
        let notifications = Notifications {
            center: self.center.clone(),
        };
        let conn = match builder.serve_at(NOTIFICATIONS_PATH, notifications) {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("roost-shell-host: notification serve failed: {e}");
                return false;
            }
        };
        let conn = match conn.build() {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("roost-shell-host: notification connect failed: {e}");
                return false;
            }
        };
        let reply = conn.request_name_with_flags(
            NOTIFICATIONS_NAME,
            zbus::fdo::RequestNameFlags::DoNotQueue.into(),
        );
        match reply {
            Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                self.conn = Some(conn);
                true
            }
            Ok(other) => {
                eprintln!("roost-shell-host: notifications name taken ({other:?}), intake off");
                false
            }
            Err(e) => {
                eprintln!("roost-shell-host: notifications name request failed: {e}");
                false
            }
        }
    }

    /// Banner press on an action row: invoke the key once, then tell
    /// the app which key fired. Replay-guarded by the center — a
    /// second press of the same key fails without emitting, so the
    /// app's callback fires exactly once.
    pub fn invoke_action(&self, id: u32, action: &str) -> Result<(), NotificationError> {
        self.center
            .lock()
            .map_err(|_| NotificationError::UnknownNotification)?
            .invoke_action(id as u64, action)?;
        self.emit_action_invoked(id, action);
        Ok(())
    }

    /// Banner dismissal: drop the banner, then tell the app why it
    /// closed (2 dismissed by the user, 1 expired, 3 closed by a
    /// `CloseNotification` call, 4 undefined).
    pub fn dismiss_banner(&self, id: u32, reason: u32) -> Result<(), NotificationError> {
        self.center
            .lock()
            .map_err(|_| NotificationError::UnknownNotification)?
            .dismiss(id as u64)?;
        self.emit_notification_closed(id, reason);
        Ok(())
    }

    /// Broadcast `ActionInvoked` for a banner press. Emission never
    /// fails the press it reports: a dead bus logs and the center
    /// mutation still stands.
    fn emit_action_invoked(&self, id: u32, action: &str) {
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        if let Err(e) = conn.emit_signal(
            None::<&str>,
            NOTIFICATIONS_PATH,
            NOTIFICATIONS_NAME,
            "ActionInvoked",
            &(id, action.to_owned()),
        ) {
            eprintln!("roost-shell-host: notification signal ActionInvoked failed: {e}");
        }
    }

    /// Broadcast `NotificationClosed` for a banner dismissal. Same
    /// log-and-keep-mutation rule as
    /// [`NotificationBus::emit_action_invoked`].
    fn emit_notification_closed(&self, id: u32, reason: u32) {
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        if let Err(e) = conn.emit_signal(
            None::<&str>,
            NOTIFICATIONS_PATH,
            NOTIFICATIONS_NAME,
            "NotificationClosed",
            &(id, reason),
        ) {
            eprintln!("roost-shell-host: notification signal NotificationClosed failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_pair_id_with_label_and_drop_a_trailing_odd_entry() {
        let paired = pair_actions(&[
            "default".to_owned(),
            "Open".to_owned(),
            "later".to_owned(),
            "Remind me".to_owned(),
            "dangling".to_owned(),
        ]);
        assert_eq!(
            paired,
            vec![
                NotificationAction {
                    id: "default".to_owned(),
                    label: "Open".to_owned(),
                },
                NotificationAction {
                    id: "later".to_owned(),
                    label: "Remind me".to_owned(),
                },
            ]
        );
        assert!(pair_actions(&[]).is_empty());
    }

    #[test]
    fn urgency_hint_maps_the_byte_and_defaults_to_normal() {
        use zbus::zvariant::{OwnedValue, Value};

        let hinted = |byte: u8| {
            let mut hints = HashMap::new();
            hints.insert(
                "urgency".to_owned(),
                Value::U8(byte).try_to_owned().expect("hint value"),
            );
            urgency_hint(&hints)
        };
        assert_eq!(hinted(0), Urgency::Low);
        assert_eq!(hinted(1), Urgency::Normal);
        assert_eq!(hinted(2), Urgency::Critical);
        assert_eq!(hinted(9), Urgency::Critical);
        assert_eq!(urgency_hint(&HashMap::new()), Urgency::Normal);
        let mut mistyped = HashMap::new();
        mistyped.insert("urgency".to_owned(), OwnedValue::from(true));
        assert_eq!(urgency_hint(&mistyped), Urgency::Normal);
    }

    /// Live stubs over a private session bus: a stub client speaks
    /// the freedesktop `Notify` shape to the real [`NotificationBus`]
    /// edge and listens for its signals. Each test owns its daemon
    /// and address, so no session-bus name is touched and parallel
    /// tests never share a bus. Skips (loudly) when no bus daemon
    /// is available.
    mod live_bus {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use super::super::{NotificationBus, NOTIFICATIONS_NAME, NOTIFICATIONS_PATH};
        use crate::notifications::{NotificationCenter, NotificationError};

        /// Private bus daemon plus its address, or `None` when no
        /// daemon is usable (the caller skips the test).
        fn private_bus() -> Option<(String, std::process::Child)> {
            let mut child = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let stdout = child.stdout.take()?;
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(stdout);
                use std::io::BufRead;
                let mut line = String::new();
                let line = reader.read_line(&mut line).ok().map(|_| line);
                let _ = tx.send(line);
            });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Some(line)) if !line.trim().is_empty() => Some((line.trim().to_owned(), child)),
                _ => {
                    let _ = child.kill();
                    None
                }
            }
        }

        fn shared_center() -> Arc<Mutex<NotificationCenter>> {
            Arc::new(Mutex::new(NotificationCenter::new()))
        }

        /// Connect a client to the private bus, retrying while the
        /// daemon starts listening.
        fn client_conn(address: &str) -> Option<zbus::blocking::Connection> {
            for _ in 0..50 {
                match zbus::blocking::connection::Builder::address(address) {
                    Ok(builder) => match builder.build() {
                        Ok(conn) => return Some(conn),
                        Err(_) => std::thread::sleep(Duration::from_millis(100)),
                    },
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            None
        }

        /// Take the notifications role on the private bus, retrying
        /// while the daemon starts listening.
        fn owned_bus(
            center: Arc<Mutex<NotificationCenter>>,
            address: &str,
        ) -> Option<NotificationBus> {
            let mut bus = NotificationBus::new(center);
            for _ in 0..50 {
                if bus.connect(Some(address)) {
                    return Some(bus);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            None
        }

        fn notify_proxy(client: &zbus::blocking::Connection) -> Option<zbus::blocking::Proxy<'_>> {
            zbus::blocking::Proxy::new(
                client,
                NOTIFICATIONS_NAME,
                NOTIFICATIONS_PATH,
                NOTIFICATIONS_NAME,
            )
            .ok()
        }

        /// Stub `Notify`: full freedesktop shape, empty hints.
        fn stub_notify(proxy: &zbus::blocking::Proxy<'_>, actions: Vec<String>) -> u32 {
            use std::collections::HashMap;
            use zbus::zvariant::OwnedValue;

            let hints: HashMap<String, OwnedValue> = HashMap::new();
            proxy
                .call::<&str, _, u32>(
                    "Notify",
                    &(
                        "stub-app".to_owned(),
                        0u32,
                        String::new(),
                        "Hello".to_owned(),
                        "A stub notification".to_owned(),
                        actions,
                        hints,
                        0i32,
                    ),
                )
                .expect("stub Notify reaches the intake")
        }

        /// Subscribe to one intake signal before triggering it, so
        /// the emission cannot race the subscription.
        fn subscribe(
            client: &zbus::blocking::Connection,
            member: &str,
        ) -> zbus::blocking::MessageIterator {
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface(NOTIFICATIONS_NAME)
                .expect("signal rule")
                .member(member)
                .expect("signal rule")
                .build();
            zbus::blocking::MessageIterator::for_match_rule(rule, client, Some(8))
                .expect("signal subscription")
        }

        #[test]
        fn stub_notify_shows_a_banner_and_close_notification_dismisses_it() {
            let Some((address, mut daemon)) = private_bus() else {
                eprintln!("SKIP live notification test: no usable dbus-daemon on PATH");
                return;
            };
            let center = shared_center();
            let Some(bus) = owned_bus(center.clone(), &address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live notification test: private bus refused connections");
                return;
            };
            let Some(client) = client_conn(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live notification test: private bus refused connections");
                return;
            };
            let Some(proxy) = notify_proxy(&client) else {
                let _ = daemon.kill();
                panic!("intake proxy builds on a private bus");
            };

            let caps: Vec<String> = proxy
                .call::<&str, _, Vec<String>>("GetCapabilities", &())
                .expect("stub GetCapabilities reaches the intake");
            assert_eq!(caps, vec!["actions".to_owned(), "persistence".to_owned()]);

            let id = stub_notify(&proxy, vec!["default".to_owned(), "Open".to_owned()]);
            assert_ne!(id, 0, "intake returns a live notification id");
            let banners = center
                .lock()
                .expect("center lock")
                .banners()
                .iter()
                .map(|n| n.id)
                .collect::<Vec<_>>();
            assert_eq!(banners, vec![id as u64], "stub Notify shows a banner");
            assert_eq!(
                center.lock().expect("center lock").banners()[0].pending_actions(),
                vec!["default"],
                "wire actions land on the banner"
            );

            proxy
                .call::<&str, _, ()>("CloseNotification", &(id,))
                .expect("stub CloseNotification reaches the intake");
            assert!(
                center.lock().expect("center lock").banners().is_empty(),
                "CloseNotification dismisses the banner"
            );
            assert_eq!(
                center.lock().expect("center lock").history().len(),
                1,
                "dismiss keeps history"
            );

            let _ = daemon.kill();
            let _ = bus;
        }

        #[test]
        fn default_action_press_fires_the_stub_callback_exactly_once() {
            let Some((address, mut daemon)) = private_bus() else {
                eprintln!("SKIP live notification test: no usable dbus-daemon on PATH");
                return;
            };
            let center = shared_center();
            let Some(bus) = owned_bus(center.clone(), &address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live notification test: private bus refused connections");
                return;
            };
            let Some(client) = client_conn(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live notification test: private bus refused connections");
                return;
            };
            let Some(proxy) = notify_proxy(&client) else {
                let _ = daemon.kill();
                panic!("intake proxy builds on a private bus");
            };

            let id = stub_notify(&proxy, vec!["default".to_owned(), "Open".to_owned()]);
            let mut invoked = subscribe(&client, "ActionInvoked");

            // Banner-side press: the default row invokes once.
            bus.invoke_action(id, "default")
                .expect("banner press invokes the default action");
            let msg = invoked
                .next()
                .expect("action signal arrives")
                .expect("action signal decodes");
            let (got_id, got_key): (u32, String) =
                msg.body().deserialize().expect("action signal body");
            assert_eq!((got_id, got_key.as_str()), (id, "default"));

            // A second press of the same key is a replay: it fails
            // without emitting, so the stub callback fires once.
            assert_eq!(
                bus.invoke_action(id, "default"),
                Err(NotificationError::ActionReplayed)
            );
            assert!(
                center.lock().expect("center lock").banners().is_empty(),
                "invoke dismisses the banner"
            );

            let _ = daemon.kill();
        }

        #[test]
        fn close_press_tells_the_stub_its_notification_closed() {
            let Some((address, mut daemon)) = private_bus() else {
                eprintln!("SKIP live notification test: no usable dbus-daemon on PATH");
                return;
            };
            let center = shared_center();
            let Some(bus) = owned_bus(center.clone(), &address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live notification test: private bus refused connections");
                return;
            };
            let Some(client) = client_conn(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live notification test: private bus refused connections");
                return;
            };
            let Some(proxy) = notify_proxy(&client) else {
                let _ = daemon.kill();
                panic!("intake proxy builds on a private bus");
            };

            let id = stub_notify(&proxy, Vec::new());
            let mut closed = subscribe(&client, "NotificationClosed");

            // Banner-side dismiss affordance: user-dismissed (2).
            bus.dismiss_banner(id, 2).expect("banner press dismisses");
            let msg = closed
                .next()
                .expect("close signal arrives")
                .expect("close signal decodes");
            let (got_id, got_reason): (u32, u32) =
                msg.body().deserialize().expect("close signal body");
            assert_eq!((got_id, got_reason), (id, 2));
            assert!(
                center.lock().expect("center lock").banners().is_empty(),
                "close press clears the banner"
            );

            let _ = daemon.kill();
        }
    }
}
