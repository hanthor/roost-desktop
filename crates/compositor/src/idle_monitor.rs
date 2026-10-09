//! org.gnome.Mutter.IdleMonitor: how long the user has been idle, and
//! watches that fire after a stretch of idleness or on the next input.
//! gnome-settings-daemon's power plugin dims and blanks the screen
//! through it, and GNOME Shell's message tray keeps a banner up while the
//! user is away. Mirrors Mutter's meta-idle-monitor-dbus: one "Core"
//! monitor under an ObjectManager at /org/gnome/Mutter/IdleMonitor.

use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::interface;
use zbus::object_server::SignalEmitter;

pub const NAME: &str = "org.gnome.Mutter.IdleMonitor";
pub const ROOT: &str = "/org/gnome/Mutter/IdleMonitor";
pub const CORE: &str = "/org/gnome/Mutter/IdleMonitor/Core";
/// Watches held at once; the oldest go first past this.
const MAX_WATCHES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Fires once the user has been idle `interval`; again after the
    /// next activity and another `interval`.
    Idle { interval: Duration, fired: bool },
    /// Fires on the next activity, then goes away.
    Active,
}

#[derive(Debug)]
struct Watch {
    id: u32,
    kind: Kind,
}

#[derive(Debug)]
struct Inner {
    last_activity: Instant,
    next_id: u32,
    watches: Vec<Watch>,
}

impl Inner {
    fn add(&mut self, kind: Kind) -> u32 {
        // Ids are unique across all callers, as in Mutter, so a
        // broadcast WatchFired reaches only the watch it names.
        self.next_id = self.next_id.wrapping_add(1).max(1);
        if self.watches.len() >= MAX_WATCHES {
            self.watches.remove(0);
        }
        self.watches.push(Watch {
            id: self.next_id,
            kind,
        });
        self.next_id
    }

    /// Input arrived: active watches fire and go, idle watches re-arm.
    fn activity(&mut self, now: Instant) -> Vec<u32> {
        self.last_activity = now;
        let mut fired = Vec::new();
        self.watches.retain_mut(|w| match &mut w.kind {
            Kind::Active => {
                fired.push(w.id);
                false
            }
            Kind::Idle { fired: f, .. } => {
                *f = false;
                true
            }
        });
        fired
    }

    /// Idle watches whose interval has passed fire once.
    fn tick(&mut self, now: Instant) -> Vec<u32> {
        let idle = now.saturating_duration_since(self.last_activity);
        let mut fired = Vec::new();
        for w in &mut self.watches {
            if let Kind::Idle { interval, fired: f } = &mut w.kind {
                if !*f && idle >= *interval {
                    *f = true;
                    fired.push(w.id);
                }
            }
        }
        fired
    }
}

/// The runtime's handle: feed it input and ticks.
#[derive(Debug, Clone)]
pub struct IdleMonitor {
    inner: Arc<Mutex<Inner>>,
    fire: mpsc::Sender<u32>,
}

impl IdleMonitor {
    /// The user did something.
    pub fn activity(&self) {
        let fired = self
            .inner
            .lock()
            .map(|mut i| i.activity(Instant::now()))
            .unwrap_or_default();
        for id in fired {
            let _ = self.fire.send(id);
        }
    }

    /// Fire idle watches that are due; called once per loop tick.
    pub fn tick(&self) {
        let fired = self
            .inner
            .lock()
            .map(|mut i| i.tick(Instant::now()))
            .unwrap_or_default();
        for id in fired {
            let _ = self.fire.send(id);
        }
    }
}

struct Core {
    inner: Arc<Mutex<Inner>>,
}

#[interface(name = "org.gnome.Mutter.IdleMonitor")]
impl Core {
    /// Milliseconds since the last input.
    fn get_idletime(&self) -> u64 {
        self.inner
            .lock()
            .map(|i| i.last_activity.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }

    fn add_idle_watch(&self, interval: u64) -> zbus::fdo::Result<u32> {
        if interval == 0 {
            return Err(zbus::fdo::Error::InvalidArgs(
                "an idle watch needs a positive interval".into(),
            ));
        }
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("idle monitor poisoned".into()))?;
        Ok(inner.add(Kind::Idle {
            interval: Duration::from_millis(interval),
            fired: false,
        }))
    }

    fn add_user_active_watch(&self) -> zbus::fdo::Result<u32> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("idle monitor poisoned".into()))?;
        Ok(inner.add(Kind::Active))
    }

    fn remove_watch(&self, id: u32) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.watches.retain(|w| w.id != id);
        }
    }

    #[zbus(signal)]
    async fn watch_fired(emitter: &SignalEmitter<'_>, id: u32) -> zbus::Result<()>;
}

/// Serve the idle monitor on the session bus from a thread.
pub fn start() -> IdleMonitor {
    let inner = Arc::new(Mutex::new(Inner {
        last_activity: Instant::now(),
        next_id: 0,
        watches: Vec::new(),
    }));
    let (fire, fired) = mpsc::channel::<u32>();
    let core = Core {
        inner: inner.clone(),
    };
    let _ = std::thread::Builder::new()
        .name("tuna-idle-monitor".into())
        .spawn(move || {
            let conn = match zbus::blocking::connection::Builder::session()
                .and_then(|b| b.serve_at(ROOT, zbus::fdo::ObjectManager))
                .and_then(|b| b.serve_at(CORE, core))
                .and_then(|b| b.build())
            {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("tuna-compositor: idle monitor: no session bus: {e}");
                    return;
                }
            };
            let flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
            match conn.request_name_with_flags(NAME, flags) {
                Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                    eprintln!("tuna-compositor: serving {NAME}");
                }
                _ => {
                    eprintln!("tuna-compositor: {NAME} is taken");
                    return;
                }
            }
            let Ok(emitter) = SignalEmitter::new(conn.inner(), CORE) else {
                return;
            };
            for id in fired {
                let _ = zbus::block_on(Core::watch_fired(&emitter, id));
            }
        });
    IdleMonitor { inner, fire }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inner(at: Instant) -> Inner {
        Inner {
            last_activity: at,
            next_id: 0,
            watches: Vec::new(),
        }
    }

    #[test]
    fn idle_watches_fire_once_per_idle_stretch() {
        let t0 = Instant::now();
        let mut m = inner(t0);
        let id = m.add(Kind::Idle {
            interval: Duration::from_millis(500),
            fired: false,
        });
        assert!(m.tick(t0 + Duration::from_millis(400)).is_empty());
        assert_eq!(m.tick(t0 + Duration::from_millis(500)), vec![id]);
        assert!(m.tick(t0 + Duration::from_millis(900)).is_empty());
        // Activity re-arms it.
        let t1 = t0 + Duration::from_secs(1);
        assert!(m.activity(t1).is_empty());
        assert_eq!(m.tick(t1 + Duration::from_millis(600)), vec![id]);
    }

    #[test]
    fn user_active_watches_fire_on_the_next_input_and_go() {
        let t0 = Instant::now();
        let mut m = inner(t0);
        let a = m.add(Kind::Active);
        let b = m.add(Kind::Active);
        assert_ne!(a, b);
        assert!(m.tick(t0 + Duration::from_secs(5)).is_empty());
        assert_eq!(m.activity(t0 + Duration::from_secs(6)), vec![a, b]);
        assert!(m.activity(t0 + Duration::from_secs(7)).is_empty());
        assert!(m.watches.is_empty());
    }
}
