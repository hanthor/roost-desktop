//! Trusted logind sleep notifications, consumed on the compositor thread.
//!
//! The VT can remain active throughout S3: libseat activation alone does not
//! notify us that the kernel discarded scanout. A retained system-bus proxy
//! binds signals to logind's actual owner. Two flags bound work and storage.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Default)]
struct Pending {
    lock: AtomicBool,
    wake: AtomicBool,
}

pub struct Monitor {
    connection: zbus::blocking::Connection,
    pending: Arc<Pending>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Monitor {
    pub fn start(generation: Arc<AtomicU64>) -> zbus::Result<Self> {
        let connection = zbus::blocking::connection::Builder::system()?
            .method_timeout(Duration::from_secs(5))
            .build()?;
        Self::from_connection(connection, generation)
    }

    fn from_connection(
        connection: zbus::blocking::Connection,
        generation: Arc<AtomicU64>,
    ) -> zbus::Result<Self> {
        let proxy = zbus::blocking::Proxy::new(
            &connection,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )?;
        let signals = proxy.receive_signal("PrepareForSleep")?;
        let pending = Arc::new(Pending::default());
        let worker_pending = pending.clone();
        let worker = std::thread::Builder::new()
            .name("roost-logind-sleep".into())
            .spawn(move || {
                let mut prepared = false;
                for signal in signals {
                    let Ok((sleeping,)) = signal.body().deserialize::<(bool,)>() else {
                        continue;
                    };
                    if sleeping {
                        // Invalidate old PAM results on the signal thread,
                        // before the compositor can dequeue their callbacks.
                        generation.fetch_add(1, Ordering::AcqRel);
                        prepared = true;
                        worker_pending.lock.store(true, Ordering::Release);
                    } else if prepared {
                        prepared = false;
                        worker_pending.wake.store(true, Ordering::Release);
                    }
                }
            })?;
        Ok(Self {
            connection,
            pending,
            worker: Some(worker),
        })
    }

    /// Coalesce notifications; the caller locks before processing a wake.
    pub fn take(&self) -> (bool, bool) {
        (
            self.pending.lock.swap(false, Ordering::AcqRel),
            self.pending.wake.swap(false, Ordering::AcqRel),
        )
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        // Closing also ends the retained signal iterator/worker; no orphaned
        // subscription survives a runtime returning in this process.
        let _ = self.connection.clone().close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    struct Bus(Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn only_the_bus_name_owner_can_prepare_and_wake_and_monitor_releases_worker() {
        let mut daemon = Bus(Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("private test bus starts"));
        let mut address = String::new();
        BufReader::new(daemon.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let connect = || {
            zbus::blocking::connection::Builder::address(address.trim())
                .unwrap()
                .method_timeout(Duration::from_secs(2))
                .build()
                .unwrap()
        };
        let owner = connect();
        owner.request_name("org.freedesktop.login1").unwrap();
        let attacker = connect();
        let generation = Arc::new(AtomicU64::new(0));
        let monitor = Monitor::from_connection(connect(), generation.clone()).unwrap();
        let emit = |connection: &zbus::blocking::Connection, sleeping: bool| {
            connection
                .emit_signal(
                    None::<()>,
                    "/org/freedesktop/login1",
                    "org.freedesktop.login1.Manager",
                    "PrepareForSleep",
                    &(sleeping,),
                )
                .unwrap();
        };
        emit(&attacker, true);
        emit(&attacker, false);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            monitor.take(),
            (false, false),
            "unowned signal must not change compositor state"
        );
        assert_eq!(generation.load(Ordering::Acquire), 0);
        emit(&owner, true);
        emit(&owner, false);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut received = (false, false);
        while Instant::now() < deadline && received != (true, true) {
            let update = monitor.take();
            received.0 |= update.0;
            received.1 |= update.1;
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            received,
            (true, true),
            "trusted logind pair must reach compositor"
        );
        assert_eq!(
            generation.load(Ordering::Acquire),
            1,
            "trusted sleep invalidates old PAM generation"
        );
        let start = Instant::now();
        drop(monitor);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "subscription worker must stop on runtime exit"
        );
        drop(daemon);
    }
}
