//! GNOME's native KeyboardMonitor, restricted to our unreaped Orca child.
//! Bus credentials are resolved off the seat thread. Signals are unicast and
//! carry a lifetime/lock epoch; raw keys never cross the locked-session route.
use std::collections::HashSet;
use std::process::{Child, Command};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use zbus::{fdo, interface, message::Header, Connection};

pub const NAME: &str = "org.freedesktop.a11y.Manager";
const PATH: &str = "/org/freedesktop/a11y/Manager";
const INTERFACE: &str = "org.freedesktop.a11y.KeyboardMonitor";
const MONITOR_NAME: &str = "org.gnome.Orca.KeyboardMonitor";
const ORCA_NAME: &str = "org.gnome.Orca1.Service";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Pending,
    Ready,
    Conflict,
    Unavailable,
}
#[derive(Clone, Copy)]
pub struct Key {
    pub released: bool,
    pub state: u32,
    pub keysym: u32,
    pub unichar: u32,
    pub keycode: u16,
}
#[derive(Default)]
struct Grants {
    owner: String,
    watch: bool,
    all: bool,
    modifiers: Vec<u32>,
    keys: Vec<(u32, u32)>,
}
struct Inner {
    available: Availability,
    pid: u32,
    epoch: u64,
    locked: bool,
    grants: Option<Grants>,
    held: HashSet<u16>,
    custom: HashSet<u32>,
    last_custom: Option<(u32, Instant)>,
    passed_custom: HashSet<u16>,
    revoked: HashSet<String>,
}
impl Inner {
    fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.grants = None;
        self.custom.clear();
        self.last_custom = None;
        self.passed_custom.clear();
        // Held grabbed releases remain suppressed until physically released.
    }
    fn valid(&self, epoch: u64, owner: &str) -> bool {
        self.pid != 0
            && !self.locked
            && self.epoch == epoch
            && self.grants.as_ref().is_some_and(|g| g.owner == owner)
    }
}
pub(crate) struct Event {
    epoch: u64,
    owner: String,
    key: Option<Key>,
}
#[derive(Clone)]
pub struct Monitor {
    inner: Arc<Mutex<Inner>>,
    events: mpsc::SyncSender<Event>,
}
impl Monitor {
    pub fn start() -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            available: Availability::Pending,
            pid: 0,
            epoch: 0,
            locked: false,
            grants: None,
            held: HashSet::new(),
            custom: HashSet::new(),
            last_custom: None,
            passed_custom: HashSet::new(),
            revoked: HashSet::new(),
        }));
        let (events, incoming) = mpsc::sync_channel::<Event>(256);
        let monitor = Self { inner, events };
        let service = monitor.clone();
        std::thread::spawn(move || service.serve(incoming));
        monitor
    }
    pub fn snapshot(&self) -> serde_json::Value {
        let inner = self.inner.lock().unwrap();
        let grants = inner.grants.as_ref();
        serde_json::json!({ "reader_pid": inner.pid, "epoch": inner.epoch, "locked": inner.locked,
            "owner": grants.map(|g| g.owner.as_str()), "watch": grants.is_some_and(|g| g.watch),
            "grab_all": grants.is_some_and(|g| g.all),
            "modifier_count": grants.map_or(0, |g| g.modifiers.len()),
            "keystroke_count": grants.map_or(0, |g| g.keys.len()) })
    }
    pub fn availability(&self) -> Availability {
        self.inner.lock().unwrap().available
    }
    /// Hold admission closed across fork until the owned child identity exists.
    pub fn spawn(&self, command: &mut Command) -> std::io::Result<Child> {
        let mut inner = self.inner.lock().unwrap();
        let child = command.spawn()?;
        inner.invalidate();
        inner.revoked.clear();
        inner.pid = child.id();
        Ok(child)
    }
    pub fn revoke_reader(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.pid = 0;
        inner.invalidate();
    }
    pub fn shutdown(&self) {
        self.revoke_reader();
        self.inner.lock().unwrap().available = Availability::Unavailable;
        let _ = self.events.try_send(Event {
            epoch: 0,
            owner: String::new(),
            key: None,
        });
    }
    pub fn set_locked(&self, locked: bool) {
        let mut inner = self.inner.lock().unwrap();
        if inner.locked != locked {
            inner.locked = locked;
            inner.epoch = inner.epoch.wrapping_add(1);
            inner.custom.clear();
            inner.last_custom = None;
            inner.passed_custom.clear();
        }
    }
    /// Pure local seat decision; no credential lookup or D-Bus roundtrip.
    pub fn key(&self, key: Key, repeat_delay: Duration) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.locked {
            inner.held.remove(&key.keycode);
            return false;
        }
        let previous = inner.held.contains(&key.keycode);
        let Some(grants) = inner.grants.as_ref().filter(|_| inner.pid != 0) else {
            if key.released {
                inner.held.remove(&key.keycode);
            }
            return previous;
        };
        let owner = grants.owner.clone();
        let custom = grants.modifiers.contains(&key.keysym);
        let mut grabbed = previous
            || grants.all
            || custom
            || !inner.custom.is_empty()
            || grants.keys.contains(&(key.keysym, key.state));
        let watched = grants.watch || grabbed;
        // A release follows its press, even if SetKeyGrabs changed meanwhile.
        if key.released {
            grabbed = previous;
            inner.custom.remove(&key.keysym);
        }
        if custom {
            if key.released {
                if inner.passed_custom.remove(&key.keycode) {
                    grabbed = false;
                }
            } else if !previous {
                let now = Instant::now();
                if inner.last_custom.is_some_and(|(sym, at)| {
                    sym == key.keysym && now.saturating_duration_since(at) <= repeat_delay
                }) {
                    grabbed = false;
                    inner.passed_custom.insert(key.keycode);
                    inner.last_custom = None;
                } else {
                    inner.custom.insert(key.keysym);
                    inner.last_custom = Some((key.keysym, now));
                }
            }
        } else if !key.released {
            inner.last_custom = None;
        }
        if key.released {
            inner.held.remove(&key.keycode);
        } else if grabbed {
            inner.held.insert(key.keycode);
        }
        if watched {
            let event = Event {
                epoch: inner.epoch,
                owner,
                key: Some(key),
            };
            if self.events.try_send(event).is_err() {
                // Losing a press/release can strand Orca modifiers. Fail closed
                // until Orca explicitly establishes a fresh monitor connection.
                inner.invalidate();
            }
        }
        grabbed
    }
    fn vanished(&self, name: &str, old: &str, new: &str) {
        let mut inner = self.inner.lock().unwrap();
        let ours = inner
            .grants
            .as_ref()
            .is_some_and(|g| g.owner == name || g.owner == old);
        if !old.is_empty()
            && (name == MONITOR_NAME || ours && name.starts_with(':') && new.is_empty())
        {
            if inner.revoked.len() >= 256 {
                inner.pid = 0;
                inner.available = Availability::Unavailable;
            }
            inner.revoked.insert(old.to_owned());
        }
        if ours && (new.is_empty() || name == MONITOR_NAME || name == ORCA_NAME) {
            inner.invalidate();
        }
    }
    fn serve(&self, incoming: mpsc::Receiver<Event>) {
        let result = (|| -> zbus::Result<()> {
            let conn = zbus::blocking::connection::Builder::session()?.build()?;
            // Install revocation before exporting a callable interface.
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender("org.freedesktop.DBus")?
                .interface("org.freedesktop.DBus")?
                .member("NameOwnerChanged")?
                .build();
            let changes = zbus::blocking::MessageIterator::for_match_rule(rule, &conn, Some(1024))?;
            let watcher = self.clone();
            let watch_conn = conn.clone();
            std::thread::spawn(move || {
                for change in changes {
                    let Ok(change) = change else {
                        watcher.revoke_reader();
                        break;
                    };
                    if let Ok((name, old, new)) =
                        change.body().deserialize::<(String, String, String)>()
                    {
                        if name == ORCA_NAME {
                            let pid = if new.is_empty() {
                                None
                            } else {
                                zbus::blocking::fdo::DBusProxy::new(&watch_conn)
                                    .ok()
                                    .and_then(|dbus| {
                                        zbus::names::BusName::try_from(new.as_str()).ok().and_then(
                                            |owner| dbus.get_connection_unix_process_id(owner).ok(),
                                        )
                                    })
                            };
                            let mut inner = watcher.inner.lock().unwrap();
                            if pid != Some(inner.pid) {
                                inner.invalidate();
                            }
                        } else {
                            watcher.vanished(&name, &old, &new);
                        }
                    }
                }
            });
            if conn.request_name_with_flags(NAME, fdo::RequestNameFlags::DoNotQueue.into())?
                != fdo::RequestNameReply::PrimaryOwner
            {
                self.inner.lock().unwrap().available = Availability::Conflict;
                conn.close()?;
                return Ok(());
            }
            if let Err(error) = conn.object_server().at(PATH, self.clone()) {
                let _ = conn.close();
                return Err(error);
            }
            self.inner.lock().unwrap().available = Availability::Ready;
            let mut delivery = Ok(());
            for event in incoming {
                let Some(key) = event.key else {
                    break;
                };
                if self.availability() == Availability::Unavailable {
                    break;
                }
                if !self.inner.lock().unwrap().valid(event.epoch, &event.owner) {
                    continue;
                }
                if let Err(error) = conn.emit_signal(
                    Some(event.owner.as_str()),
                    PATH,
                    INTERFACE,
                    "KeyEvent",
                    &(
                        key.released,
                        key.state,
                        key.keysym,
                        key.unichar,
                        key.keycode,
                    ),
                ) {
                    delivery = Err(error);
                    break;
                }
            }
            conn.close()?;
            delivery
        })();
        if result.is_err() {
            let mut inner = self.inner.lock().unwrap();
            inner.available = Availability::Unavailable;
            inner.pid = 0;
            inner.invalidate();
            eprintln!("roost-compositor: accessibility keyboard bus unavailable");
        }
    }
    async fn authorize(
        &self,
        conn: &Connection,
        header: &Header<'_>,
    ) -> fdo::Result<(String, u64)> {
        let sender = header
            .sender()
            .ok_or_else(|| fdo::Error::AccessDenied("missing sender".into()))?;
        let (pid, epoch) = {
            let inner = self.inner.lock().unwrap();
            (inner.pid, inner.epoch)
        };
        if pid == 0 {
            return Err(fdo::Error::AccessDenied("owned reader required".into()));
        }
        let dbus = fdo::DBusProxy::new(conn).await?;
        let credentials = dbus
            .get_connection_credentials(sender.clone().into())
            .await?;
        if credentials.unix_user_id() != Some(rustix::process::geteuid().as_raw())
            || credentials.process_id() != Some(pid)
        {
            return Err(fdo::Error::AccessDenied(
                "owned reader credentials required".into(),
            ));
        }
        let name = zbus::names::BusName::try_from(MONITOR_NAME).unwrap();
        if dbus.get_name_owner(name).await?.as_str() != sender.as_str() {
            return Err(fdo::Error::AccessDenied(
                "original keyboard owner required".into(),
            ));
        }
        // Orca creates Atspi.Device before its remote controller. If present,
        // that separate connection must still belong to the same owned PID.
        let name = zbus::names::BusName::try_from(ORCA_NAME).unwrap();
        match dbus.get_name_owner(name).await {
            Ok(owner) => {
                if dbus.get_connection_unix_process_id(owner.into()).await? != pid {
                    return Err(fdo::Error::AccessDenied("foreign Orca owner".into()));
                }
            }
            Err(fdo::Error::NameHasNoOwner(_)) => {}
            Err(error) => return Err(error),
        }
        let inner = self.inner.lock().unwrap();
        if inner.pid != pid || inner.epoch != epoch || inner.revoked.contains(sender.as_str()) {
            return Err(fdo::Error::AccessDenied("reader lifetime changed".into()));
        }
        Ok((sender.to_string(), epoch))
    }
    async fn change(
        &self,
        conn: &Connection,
        header: &Header<'_>,
        update: impl FnOnce(&mut Grants),
    ) -> fdo::Result<()> {
        let (owner, epoch) = self.authorize(conn, header).await?;
        let mut inner = self.inner.lock().unwrap();
        if inner.epoch != epoch || inner.pid == 0 || inner.revoked.contains(&owner) {
            return Err(fdo::Error::AccessDenied("reader revoked".into()));
        }
        if inner.grants.as_ref().is_some_and(|g| g.owner != owner) {
            inner.invalidate();
        }
        let grants = inner.grants.get_or_insert_with(|| Grants {
            owner,
            ..Default::default()
        });
        update(grants);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn monitor(
        modifiers: Vec<u32>,
        keys: Vec<(u32, u32)>,
    ) -> (Monitor, mpsc::Receiver<Event>) {
        let (events, incoming) = mpsc::sync_channel(256);
        let inner = Inner {
            available: Availability::Ready,
            pid: 13,
            epoch: 1,
            locked: false,
            grants: Some(Grants {
                owner: ":1.23".into(),
                watch: true,
                all: false,
                modifiers,
                keys,
            }),
            held: HashSet::new(),
            custom: HashSet::new(),
            last_custom: None,
            passed_custom: HashSet::new(),
            revoked: HashSet::new(),
        };
        (
            Monitor {
                inner: Arc::new(Mutex::new(inner)),
                events,
            },
            incoming,
        )
    }
    fn key(keysym: u32, keycode: u16, released: bool, state: u32) -> Key {
        Key {
            keysym,
            keycode,
            released,
            state,
            unichar: 0,
        }
    }
    #[test]
    fn custom_modifier_single_is_grabbed_double_passes_balanced() {
        let (m, _events) = monitor(vec![0xffe5], vec![]);
        let delay = Duration::from_secs(1);
        assert!(m.key(key(0xffe5, 66, false, 0), delay));
        assert!(m.key(key(0xffe5, 66, true, 0), delay));
        assert!(!m.key(key(0xffe5, 66, false, 0), delay));
        assert!(!m.key(key(0xffe5, 66, true, 0), delay));
    }
    #[test]
    fn chord_and_shifted_grabs_cannot_leak_to_normal_shortcuts() {
        let (m, _events) = monitor(vec![0xffe5], vec![(0xffc1, 1)]);
        let delay = Duration::from_secs(1);
        assert!(!m.key(key(0xffc1, 68, false, 0), delay));
        assert!(!m.key(key(0xffc1, 68, true, 0), delay));
        assert!(m.key(key(0xffc1, 68, false, 1), delay));
        assert!(m.key(key(0xffc1, 68, true, 1), delay));
        assert!(m.key(key(0xffe5, 66, false, 0), delay));
        assert!(m.key(key(0xffe3, 37, false, 0), delay));
        assert!(m.key(key(0xffbe, 67, false, 4), delay));
        assert!(m.key(key(0xffbe, 67, true, 4), delay));
        assert!(m.key(key(0xffe3, 37, true, 4), delay));
        assert!(m.key(key(0xffe5, 66, true, 0), delay));
    }
    #[test]
    fn disconnect_revokes_pending_delivery_but_balances_captured_release() {
        let (m, events) = monitor(vec![0xffe5], vec![]);
        assert!(m.key(key(0xffe5, 66, false, 0), Duration::from_secs(1)));
        let pending = events.recv().unwrap();
        m.vanished(":1.23", ":1.23", "");
        assert!(!m.inner.lock().unwrap().valid(pending.epoch, &pending.owner));
        assert!(m.key(key(0xffe5, 66, true, 0), Duration::from_secs(1)));
        assert!(!m.key(key(0xffe5, 66, false, 0), Duration::from_secs(1)));
    }
    #[test]
    fn lock_never_queues_keys_and_invalidates_old_events() {
        let (m, events) = monitor(vec![], vec![]);
        m.key(key(0x61, 38, false, 0), Duration::from_secs(1));
        let pending = events.recv().unwrap();
        m.set_locked(true);
        assert!(!m.inner.lock().unwrap().valid(pending.epoch, &pending.owner));
        assert!(!m.key(key(0x73, 39, false, 0), Duration::from_secs(1)));
        assert!(events.try_recv().is_err());
        m.set_locked(false);
        assert!(m.inner.lock().unwrap().grants.as_ref().unwrap().watch);
        m.key(key(0x73, 39, true, 0), Duration::from_secs(1));
        let resumed = events.recv().unwrap();
        assert!(m.inner.lock().unwrap().valid(resumed.epoch, &resumed.owner));
        assert!(!m.inner.lock().unwrap().valid(resumed.epoch, ":1.24"));
    }
    #[test]
    fn changing_grabs_never_swallows_an_already_forwarded_release() {
        let (m, _events) = monitor(vec![], vec![]);
        assert!(!m.key(key(0x61, 38, false, 0), Duration::from_secs(1)));
        m.inner
            .lock()
            .unwrap()
            .grants
            .as_mut()
            .unwrap()
            .keys
            .push((0x61, 0));
        assert!(!m.key(key(0x61, 38, true, 0), Duration::from_secs(1)));
    }
}
#[interface(name = "org.freedesktop.a11y.KeyboardMonitor")]
impl Monitor {
    #[zbus(signal)]
    async fn key_event(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        released: bool,
        state: u32,
        keysym: u32,
        unichar: u32,
        keycode: u16,
    ) -> zbus::Result<()>;
    async fn watch_keyboard(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        self.change(conn, &header, |g| g.watch = true).await
    }
    async fn unwatch_keyboard(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        self.change(conn, &header, |g| g.watch = false).await
    }
    async fn grab_keyboard(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        self.change(conn, &header, |g| g.all = true).await
    }
    async fn ungrab_keyboard(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        self.change(conn, &header, |g| g.all = false).await
    }
    async fn set_key_grabs(
        &self,
        modifiers: Vec<u32>,
        keystrokes: Vec<(u32, u32)>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        if modifiers.len() > 32 || keystrokes.len() > 4096 {
            return Err(fdo::Error::InvalidArgs("too many key grabs".into()));
        }
        self.change(conn, &header, |g| {
            g.modifiers = modifiers;
            g.keys = keystrokes;
        })
        .await
    }
}
