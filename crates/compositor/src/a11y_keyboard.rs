//! GNOME's native KeyboardMonitor, restricted to our unreaped Orca child.
//! Bus credentials are resolved off the seat thread. Signals are unicast and
//! carry a lifetime/lock epoch; raw keys never cross the locked-session route.
use std::collections::{HashMap, HashSet};
use std::process::{Child, Command};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use zbus::{fdo, interface, message::Header, Connection};

pub const NAME: &str = "org.freedesktop.a11y.Manager";
const PATH: &str = "/org/freedesktop/a11y/Manager";
const INTERFACE: &str = "org.freedesktop.a11y.KeyboardMonitor";
const MONITOR_NAME: &str = "org.gnome.Orca.KeyboardMonitor";
const ORCA_NAMES: [&str; 2] = ["org.gnome.Orca1.Service", "org.gnome.Orca.Service"];

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
#[derive(Clone)]
struct Reset {
    lifetime: u64,
    generation: u64,
    owner: String,
    deadline: Instant,
}
struct Inner {
    available: Availability,
    pid: u32,
    epoch: u64,
    lifetime: u64,
    recipient: Option<String>,
    recipient_generation: u64,
    normal_events: u64,
    reset_events: u64,
    reset_generation: u64,
    reset: Option<Reset>,
    locked: bool,
    grants: Option<Grants>,
    held: HashSet<u16>,
    protected: HashSet<u16>,
    custom: HashMap<u16, u32>,
    last_custom: Option<(u32, Instant)>,
    passed_custom: HashSet<u16>,
    revoked: HashSet<String>,
}
impl Inner {
    fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        // A brief revoke/re-admit of the identical bus owner must permanently
        // discard disclosed modifier metadata, even if the emitter misses it.
        self.recipient_generation = self.recipient_generation.wrapping_add(1);
        self.grants = None;
        self.recipient = None;
        self.reset = None;
        self.custom.clear();
        self.last_custom = None;
        self.passed_custom.clear();
        // Held grabbed releases remain suppressed until physically released.
    }
    fn request_reset(&mut self) {
        if self.pid == 0 {
            return;
        }
        if let Some(owner) = self.recipient.clone() {
            self.reset_generation = self.reset_generation.wrapping_add(1);
            let deadline = self
                .reset
                .as_ref()
                .map_or(Instant::now() + Duration::from_secs(2), |reset| {
                    reset.deadline
                });
            self.reset = Some(Reset {
                lifetime: self.lifetime,
                generation: self.reset_generation,
                owner,
                deadline,
            });
        }
    }
    fn reset_current(&self, reset: &Reset) -> bool {
        self.pid != 0
            && self.lifetime == reset.lifetime
            && self.recipient.as_deref() == Some(reset.owner.as_str())
            && !self.revoked.contains(&reset.owner)
            && self
                .reset
                .as_ref()
                .is_some_and(|current| current.generation == reset.generation)
    }
    fn fail(&mut self) {
        self.available = Availability::Unavailable;
        self.pid = 0;
        self.invalidate();
    }
    fn suspend_holds(&mut self) {
        let captured = std::mem::take(&mut self.held);
        self.protected.extend(captured);
        self.custom.clear();
        self.last_custom = None;
        self.passed_custom.clear();
    }
    fn valid(&self, epoch: u64, owner: &str) -> bool {
        self.pid != 0
            && !self.locked
            && self.reset.is_none()
            && self.epoch == epoch
            && self.grants.as_ref().is_some_and(|g| g.owner == owner)
    }
}
pub(crate) struct Event {
    epoch: u64,
    owner: String,
    key: Option<Key>,
    lifetime: u64,
    custom_modifier: bool,
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
            lifetime: 0,
            recipient: None,
            recipient_generation: 0,
            normal_events: 0,
            reset_events: 0,
            reset_generation: 0,
            reset: None,
            locked: false,
            grants: None,
            held: HashSet::new(),
            protected: HashSet::new(),
            custom: HashMap::new(),
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
            "normal_events": inner.normal_events, "reset_events": inner.reset_events,
            "resume_state": if inner.locked { "locked" } else if inner.reset.is_some() { "resetting" } else if inner.pid == 0 { "unavailable" } else if grants.is_none() { "unwatched" } else { "ready" },
            "modifier_count": grants.map_or(0, |g| g.modifiers.len()),
            "keystroke_count": grants.map_or(0, |g| g.keys.len()) })
    }
    pub fn availability(&self) -> Availability {
        let mut inner = self.inner.lock().unwrap();
        if inner
            .reset
            .as_ref()
            .is_some_and(|reset| Instant::now() >= reset.deadline)
        {
            inner.fail();
        }
        inner.available
    }
    /// Hold admission closed across fork until the owned child identity exists.
    pub fn spawn(&self, command: &mut Command) -> std::io::Result<Child> {
        let mut inner = self.inner.lock().unwrap();
        let child = command.spawn()?;
        inner.invalidate();
        inner.revoked.clear();
        inner.lifetime = inner.lifetime.wrapping_add(1);
        inner.pid = child.id();
        Ok(child)
    }
    pub fn revoke_reader(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.pid = 0;
        inner.lifetime = inner.lifetime.wrapping_add(1);
        inner.invalidate();
    }
    pub fn shutdown(&self) {
        self.revoke_reader();
        self.inner.lock().unwrap().available = Availability::Unavailable;
        let _ = self.events.try_send(Event {
            epoch: 0,
            owner: String::new(),
            key: None,
            lifetime: 0,
            custom_modifier: false,
        });
    }
    pub fn set_locked(&self, locked: bool) {
        let mut inner = self.inner.lock().unwrap();
        if inner.locked != locked {
            inner.locked = locked;
            if locked {
                inner.suspend_holds();
                inner.request_reset();
            }
            inner.epoch = inner.epoch.wrapping_add(1);
            inner.custom.clear();
            inner.last_custom = None;
            inner.passed_custom.clear();
        }
    }
    /// Pure local seat decision; no credential lookup or D-Bus roundtrip.
    pub fn key(&self, key: Key, repeat_delay: Duration) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.locked || inner.reset.is_some() {
            if key.released {
                inner.protected.remove(&key.keycode);
            } else {
                inner.protected.insert(key.keycode);
            }
            inner.held.remove(&key.keycode);
            return false;
        }
        // A key originating in the lock/PAM route remains private through its
        // release, even when verified unlock resumes the dormant monitor.
        if inner.protected.contains(&key.keycode) {
            if key.released {
                inner.protected.remove(&key.keycode);
            }
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
            inner.custom.remove(&key.keycode);
            if inner.passed_custom.remove(&key.keycode) {
                grabbed = false;
            }
        }
        if custom {
            if !key.released && !previous {
                let now = Instant::now();
                if inner.last_custom.is_some_and(|(sym, at)| {
                    sym == key.keysym && now.saturating_duration_since(at) <= repeat_delay
                }) {
                    grabbed = false;
                    inner.passed_custom.insert(key.keycode);
                    inner.last_custom = None;
                } else {
                    inner.custom.insert(key.keycode, key.keysym);
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
                lifetime: inner.lifetime,
                custom_modifier: custom,
            };
            if self.events.try_send(event).is_err() {
                // Losing a press/release can strand Orca modifiers. Fail closed
                // until the genuine owner explicitly reestablishes its grants.
                // Reliable reset is separate from the full raw-event queue.
                inner.request_reset();
                inner.grants = None;
                inner.epoch = inner.epoch.wrapping_add(1);
                inner.suspend_holds();
            }
        }
        grabbed
    }
    fn vanished(&self, name: &str, old: &str, new: &str) {
        let mut inner = self.inner.lock().unwrap();
        let ours = inner
            .recipient
            .as_ref()
            .is_some_and(|owner| owner == name || owner == old);
        if !old.is_empty()
            && (name == MONITOR_NAME || ours && name.starts_with(':') && new.is_empty())
        {
            if inner.revoked.len() >= 256 {
                inner.pid = 0;
                inner.available = Availability::Unavailable;
            }
            inner.revoked.insert(old.to_owned());
        }
        if ours && (new.is_empty() || name == MONITOR_NAME || ORCA_NAMES.contains(&name)) {
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
                        if ORCA_NAMES.contains(&name.as_str()) {
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
            let exported = conn.object_server().at(PATH, self.clone());
            if let Err(error) = exported {
                let _ = conn.close();
                return Err(error);
            }
            self.inner.lock().unwrap().available = Availability::Ready;
            let mut emitter = Emitter::default();
            let mut delivery = Ok(());
            loop {
                if self.availability() == Availability::Unavailable {
                    break;
                }
                if let Err(error) = emitter.reset(self, |owner, key| emit_key(&conn, owner, key)) {
                    delivery = Err(error);
                    break;
                }
                match incoming.recv_timeout(Duration::from_millis(10)) {
                    Ok(event) if event.key.is_none() => break,
                    Ok(event) => {
                        if let Err(error) =
                            emitter.normal(self, event, |owner, key| emit_key(&conn, owner, key))
                        {
                            delivery = Err(error);
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
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
        for name in ORCA_NAMES {
            let name = zbus::names::BusName::try_from(name).unwrap();
            match dbus.get_name_owner(name).await {
                Ok(owner) => {
                    let credentials = dbus.get_connection_credentials(owner.into()).await?;
                    if credentials.process_id() != Some(pid)
                        || credentials.unix_user_id() != Some(rustix::process::geteuid().as_raw())
                    {
                        return Err(fdo::Error::AccessDenied("foreign Orca owner".into()));
                    }
                }
                Err(fdo::Error::NameHasNoOwner(_)) => {}
                Err(error) => return Err(error),
            }
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
        if inner
            .recipient
            .as_ref()
            .is_some_and(|current| current != &owner)
        {
            inner.invalidate();
        }
        inner.recipient = Some(owner.clone());
        let grants = inner.grants.get_or_insert_with(|| Grants {
            owner,
            ..Default::default()
        });
        update(grants);
        Ok(())
    }
}

fn emit_key(conn: &zbus::blocking::Connection, owner: &str, key: Key) -> zbus::Result<()> {
    conn.emit_signal(
        Some(owner),
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
    )
}
/// One serialized emitter records only successfully disclosed custom presses.
/// Reset is a reliable slot, not a raw queued event. Send completion/FIFO order
/// precedes resumed key delivery; this API has no client acknowledgement.
#[derive(Default)]
struct Emitter {
    lifetime: u64,
    owner: String,
    recipient_generation: u64,
    modifiers: HashMap<u16, u32>,
}
impl Emitter {
    fn retain(&mut self, inner: &Inner) {
        if inner.pid == 0
            || self.lifetime != inner.lifetime
            || self.recipient_generation != inner.recipient_generation
            || inner.recipient.as_deref() != Some(self.owner.as_str())
        {
            self.modifiers.clear();
            self.lifetime = inner.lifetime;
            self.recipient_generation = inner.recipient_generation;
            self.owner = inner.recipient.clone().unwrap_or_default();
        }
    }
    fn normal(
        &mut self,
        monitor: &Monitor,
        event: Event,
        mut send: impl FnMut(&str, Key) -> zbus::Result<()>,
    ) -> zbus::Result<()> {
        {
            let inner = monitor.inner.lock().unwrap();
            self.retain(&inner);
            if inner.lifetime != event.lifetime || !inner.valid(event.epoch, &event.owner) {
                return Ok(());
            }
        }
        let mut key = event.key.unwrap();
        if let Some(original) = self.modifiers.get(&key.keycode) {
            // AT-SPI clears virtual bits by keysym, not physical code. Repeats
            // and release keep a disclosed custom hold's original symbol even
            // across a layout change. Ordinary physical keys are unaffected.
            key.keysym = *original;
            key.unichar = 0;
        }
        // Off-seat send can already be in flight when lock starts. Its successful
        // custom press is recorded before the same emitter services that reset.
        if let Err(error) = send(&event.owner, key) {
            monitor.inner.lock().unwrap().fail();
            return Err(error);
        }
        {
            let mut inner = monitor.inner.lock().unwrap();
            inner.normal_events = inner.normal_events.saturating_add(1);
        }
        if key.released {
            self.modifiers.remove(&key.keycode);
        } else if event.custom_modifier {
            self.modifiers.entry(key.keycode).or_insert(key.keysym);
        }
        Ok(())
    }
    fn reset(
        &mut self,
        monitor: &Monitor,
        mut send: impl FnMut(&str, Key) -> zbus::Result<()>,
    ) -> zbus::Result<()> {
        let reset = {
            let inner = monitor.inner.lock().unwrap();
            self.retain(&inner);
            inner.reset.clone()
        };
        let Some(reset) = reset else {
            return Ok(());
        };
        let keys = self
            .modifiers
            .iter()
            .map(|(&code, &sym)| (code, sym))
            .collect::<Vec<_>>();
        for (keycode, keysym) in keys {
            if !monitor.inner.lock().unwrap().reset_current(&reset) {
                return Ok(());
            }
            let key = Key {
                released: true,
                state: 0,
                keysym,
                unichar: 0,
                keycode,
            };
            if let Err(error) = send(&reset.owner, key) {
                monitor.inner.lock().unwrap().fail();
                return Err(error);
            }
            {
                let mut inner = monitor.inner.lock().unwrap();
                inner.reset_events = inner.reset_events.saturating_add(1);
            }
            self.modifiers.remove(&keycode);
        }
        let mut inner = monitor.inner.lock().unwrap();
        if inner.reset_current(&reset) {
            inner.reset = None;
        }
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
            lifetime: 1,
            recipient: Some(":1.23".into()),
            recipient_generation: 0,
            normal_events: 0,
            reset_events: 0,
            reset_generation: 0,
            reset: None,
            locked: false,
            grants: Some(Grants {
                owner: ":1.23".into(),
                watch: true,
                all: false,
                modifiers,
                keys,
            }),
            held: HashSet::new(),
            protected: HashSet::new(),
            custom: HashMap::new(),
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
    fn layout_change_release_clears_physical_custom_hold() {
        let (m, _events) = monitor(vec![0xffe5], vec![]);
        let delay = Duration::from_secs(1);
        assert!(m.key(key(0xffe5, 66, false, 0), delay));
        // The physical release survives a layout/keymap keysym change.
        assert!(m.key(key(0xffe6, 66, true, 0), delay));
        assert!(!m.key(key(0x61, 38, false, 0), delay));
        assert!(!m.key(key(0x61, 38, true, 0), delay));
        // The second standalone tap must also balance after a layout change.
        assert!(m.key(key(0xffe5, 66, false, 0), delay));
        assert!(m.key(key(0xffe5, 66, true, 0), delay));
        assert!(!m.key(key(0xffe5, 66, false, 0), delay));
        assert!(!m.key(key(0xffe6, 66, true, 0), delay));
        assert!(m.inner.lock().unwrap().passed_custom.is_empty());
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
        Emitter::default().reset(&m, |_, _| Ok(())).unwrap();
        assert!(m.inner.lock().unwrap().grants.as_ref().unwrap().watch);
        m.key(key(0x73, 39, true, 0), Duration::from_secs(1));
        assert!(events.try_recv().is_err());
        m.key(key(0x73, 39, false, 0), Duration::from_secs(1));
        let resumed = events.recv().unwrap();
        assert!(m.inner.lock().unwrap().valid(resumed.epoch, &resumed.owner));
        assert!(!m.inner.lock().unwrap().valid(resumed.epoch, ":1.24"));
    }
    #[test]
    fn prelock_captured_hold_does_not_resume_with_a_release_only_event() {
        let (m, events) = monitor(vec![0xffe5], vec![]);
        let delay = Duration::from_secs(1);
        assert!(m.key(key(0xffe5, 66, false, 0), delay));
        let pending = events.recv().unwrap();
        m.set_locked(true);
        assert!(!m.inner.lock().unwrap().valid(pending.epoch, &pending.owner));
        // Ordinary seat key-up routing remains available while locked.
        assert!(!m.key(key(0xffe5, 66, true, 0), delay));
        assert!(events.try_recv().is_err());
        m.set_locked(false);
        Emitter::default().reset(&m, |_, _| Ok(())).unwrap();
        assert!(m.key(key(0xffe5, 66, false, 0), delay));
        events.recv().unwrap();
        m.set_locked(true);
        m.set_locked(false);
        // This time the old captured physical hold survived unlock.
        assert!(!m.key(key(0xffe5, 66, true, 0), delay));
        assert!(events.try_recv().is_err());
        Emitter::default().reset(&m, |_, _| Ok(())).unwrap();
        assert!(m.key(key(0xffe5, 66, false, 0), delay));
        assert!(events.recv().unwrap().key.is_some());
    }
    #[test]
    fn protected_press_never_leaks_repeat_or_release_after_unlock_or_reauthorization() {
        let (m, events) = monitor(vec![], vec![]);
        m.set_locked(true);
        assert!(!m.key(key(0x70, 33, false, 0), Duration::from_secs(1)));
        m.set_locked(false);
        // Revocation/reestablishment must not discard the lock-origin identity.
        let grants = m.inner.lock().unwrap().grants.take();
        m.inner.lock().unwrap().invalidate();
        {
            let mut inner = m.inner.lock().unwrap();
            inner.recipient = grants.as_ref().map(|g| g.owner.clone());
            inner.grants = grants;
        }
        assert!(!m.key(key(0x70, 33, false, 0), Duration::from_secs(1)));
        assert!(!m.key(key(0x70, 33, true, 0), Duration::from_secs(1)));
        assert!(events.try_recv().is_err());
        assert!(!m.key(key(0x70, 33, false, 0), Duration::from_secs(1)));
        assert!(events.recv().unwrap().key.is_some());
    }
    // These closure-based tests qualify transport policy, never genuine speech.
    #[test]
    fn disclosed_custom_reset_survives_unlock_and_balances_changed_layout_release() {
        let (m, events) = monitor(vec![0xffe5], vec![]);
        let delay = Duration::from_secs(1);
        let mut emitter = Emitter::default();
        assert!(m.key(key(0xffe5, 66, false, 0), delay));
        emitter
            .normal(&m, events.recv().unwrap(), |_, _| Ok(()))
            .unwrap();
        // Ordinary/chord letters are deliberately excluded from reset metadata.
        m.key(key(0x61, 38, false, 0), delay);
        emitter
            .normal(&m, events.recv().unwrap(), |_, _| Ok(()))
            .unwrap();
        m.set_locked(true);
        m.set_locked(false);
        assert_eq!(m.snapshot()["resume_state"], "resetting");
        assert!(!m.key(key(0x70, 33, false, 0), delay));
        let mut resets = Vec::new();
        emitter
            .reset(&m, |owner, key| {
                assert_eq!(owner, ":1.23");
                resets.push(key);
                Ok(())
            })
            .unwrap();
        assert_eq!(resets.len(), 1);
        assert!(resets[0].released);
        assert_eq!(
            (
                resets[0].keysym,
                resets[0].keycode,
                resets[0].state,
                resets[0].unichar
            ),
            (0xffe5, 66, 0, 0)
        );
        assert_eq!(m.snapshot()["resume_state"], "ready");
        assert_eq!(m.snapshot()["normal_events"], 2);
        assert_eq!(m.snapshot()["reset_events"], 1);
        // Both lock/reset-origin repeats and their releases remain private.
        assert!(!m.key(key(0x70, 33, false, 0), delay));
        assert!(!m.key(key(0x70, 33, true, 0), delay));
        assert!(!m.key(key(0xffe5, 66, true, 0), delay));
        assert!(events.try_recv().is_err());
        assert!(m.key(key(0xffe5, 66, false, 0), delay));
        emitter
            .normal(&m, events.recv().unwrap(), |_, _| Ok(()))
            .unwrap();
        assert!(m.key(key(0xffe6, 66, false, 0), delay));
        emitter
            .normal(&m, events.recv().unwrap(), |_, repeat| {
                assert_eq!(repeat.keysym, 0xffe5);
                Ok(())
            })
            .unwrap();
        assert!(m.key(key(0xffe6, 66, true, 0), delay));
        emitter
            .normal(&m, events.recv().unwrap(), |_, release| {
                assert_eq!(release.keysym, 0xffe5);
                assert_eq!(release.unichar, 0);
                Ok(())
            })
            .unwrap();
        assert!(emitter.modifiers.is_empty());
    }
    #[test]
    fn in_flight_custom_press_is_disclosed_before_serialized_lock_reset() {
        let (m, events) = monitor(vec![0xffe5], vec![]);
        let mut emitter = Emitter::default();
        m.key(key(0xffe5, 66, false, 0), Duration::from_secs(1));
        emitter
            .normal(&m, events.recv().unwrap(), |_, _| {
                m.set_locked(true);
                Ok(())
            })
            .unwrap();
        let mut sent = 0;
        emitter
            .reset(&m, |_, reset| {
                assert_eq!(reset.keysym, 0xffe5);
                sent += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(sent, 1);
        assert!(m.inner.lock().unwrap().reset.is_none());
    }
    #[test]
    fn overlapping_lock_generation_cannot_resume_before_latest_reset() {
        let (m, events) = monitor(vec![0xffe5], vec![]);
        let mut emitter = Emitter::default();
        m.key(key(0xffe5, 66, false, 0), Duration::from_secs(1));
        emitter
            .normal(&m, events.recv().unwrap(), |_, _| Ok(()))
            .unwrap();
        m.set_locked(true);
        let first = m.inner.lock().unwrap().reset.as_ref().unwrap().generation;
        emitter
            .reset(&m, |_, _| {
                m.set_locked(false);
                m.set_locked(true);
                Ok(())
            })
            .unwrap();
        assert_ne!(
            m.inner.lock().unwrap().reset.as_ref().unwrap().generation,
            first
        );
        m.set_locked(false);
        assert_eq!(m.snapshot()["resume_state"], "resetting");
        emitter
            .reset(&m, |_, _| panic!("already reset disclosed bit"))
            .unwrap();
        assert_eq!(m.snapshot()["resume_state"], "ready");
    }
    #[test]
    fn full_queue_cannot_drop_reset_and_stale_keys_cannot_resume() {
        let (m, events) = monitor(vec![0xffe5], vec![]);
        let delay = Duration::from_secs(1);
        let mut emitter = Emitter::default();
        m.key(key(0xffe5, 66, false, 0), delay);
        emitter
            .normal(&m, events.recv().unwrap(), |_, _| Ok(()))
            .unwrap();
        for _ in 0..256 {
            m.key(key(0x61, 38, false, 0), delay);
        }
        m.set_locked(true);
        m.set_locked(false);
        let mut resets = 0;
        emitter
            .reset(&m, |_, _| {
                resets += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(resets, 1);
        while let Ok(event) = events.try_recv() {
            emitter
                .normal(&m, event, |_, _| panic!("stale queued key"))
                .unwrap();
        }
        assert!(!m.key(key(0xffe5, 66, true, 0), delay));
        assert!(m.key(key(0xffe5, 66, false, 0), delay));
        emitter
            .normal(&m, events.recv().unwrap(), |_, _| Ok(()))
            .unwrap();
        assert_eq!(emitter.modifiers.len(), 1);
    }
    #[test]
    fn queue_overflow_preserves_reset_but_requires_explicit_grant_reestablishment() {
        let (m, events) = monitor(vec![0xffe5], vec![]);
        let delay = Duration::from_secs(1);
        let mut emitter = Emitter::default();
        m.key(key(0xffe5, 66, false, 0), delay);
        emitter
            .normal(&m, events.recv().unwrap(), |_, _| Ok(()))
            .unwrap();
        for _ in 0..257 {
            m.key(key(0x61, 38, false, 0), delay);
        }
        assert!(m.inner.lock().unwrap().reset.is_some());
        assert!(m.inner.lock().unwrap().grants.is_none());
        let mut resets = 0;
        emitter
            .reset(&m, |_, _| {
                resets += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(resets, 1);
        assert_eq!(m.snapshot()["resume_state"], "unwatched");
    }
    #[test]
    fn recipient_invalidation_discards_metadata_before_identical_owner_readmission() {
        for in_flight in [false, true] {
            let (m, events) = monitor(vec![0xffe5], vec![]);
            let mut emitter = Emitter::default();
            m.key(key(0xffe5, 66, false, 0), Duration::from_secs(1));
            let readmit = || {
                let mut inner = m.inner.lock().unwrap();
                let grants = inner.grants.take();
                inner.invalidate();
                inner.recipient = Some(":1.23".into());
                inner.grants = grants;
            };
            emitter
                .normal(&m, events.recv().unwrap(), |_, _| {
                    if in_flight {
                        readmit();
                    }
                    Ok(())
                })
                .unwrap();
            assert_eq!(emitter.modifiers.len(), 1);
            if !in_flight {
                readmit();
            }
            m.set_locked(true);
            emitter
                .reset(&m, |_, _| panic!("metadata from revoked admission"))
                .unwrap();
            assert!(emitter.modifiers.is_empty());
            assert!(m.inner.lock().unwrap().reset.is_none());
        }
    }
    #[test]
    fn owner_loss_cancels_reset_and_cannot_target_replacement_connection() {
        let (m, events) = monitor(vec![0xffe5], vec![]);
        let mut emitter = Emitter::default();
        m.key(key(0xffe5, 66, false, 0), Duration::from_secs(1));
        emitter
            .normal(&m, events.recv().unwrap(), |_, _| Ok(()))
            .unwrap();
        m.set_locked(true);
        m.vanished(":1.23", ":1.23", "");
        emitter
            .reset(&m, |_, _| panic!("revoked destination"))
            .unwrap();
        assert!(emitter.modifiers.is_empty());
        assert!(m.inner.lock().unwrap().reset.is_none());
        assert!(m.inner.lock().unwrap().revoked.contains(":1.23"));
    }
    #[test]
    fn reset_send_failure_or_deadline_refuses_native_resume() {
        let (m, events) = monitor(vec![0xffe5], vec![]);
        let mut emitter = Emitter::default();
        m.key(key(0xffe5, 66, false, 0), Duration::from_secs(1));
        emitter
            .normal(&m, events.recv().unwrap(), |_, _| Ok(()))
            .unwrap();
        m.set_locked(true);
        assert!(emitter
            .reset(&m, |_, _| Err(std::io::Error::other("send failed").into()))
            .is_err());
        assert_eq!(m.snapshot()["normal_events"], 1);
        assert_eq!(m.snapshot()["reset_events"], 0);
        assert!(m.availability() == Availability::Unavailable);
        assert_eq!(m.inner.lock().unwrap().pid, 0);
        let (m, _events) = monitor(vec![], vec![]);
        m.set_locked(true);
        m.inner.lock().unwrap().reset.as_mut().unwrap().deadline =
            Instant::now() - Duration::from_secs(1);
        m.set_locked(false);
        assert!(m.availability() == Availability::Unavailable);
        assert_eq!(m.snapshot()["resume_state"], "unavailable");
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
