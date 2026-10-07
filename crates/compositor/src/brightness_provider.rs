//! Read-only, off-frame admission of the original installed GNOME Power owner.
//! No service activation, process replacement or hardware mutation occurs here.
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
const NAME: &str = "org.gnome.SettingsDaemon.Power";
const DEADLINE: Duration = Duration::from_secs(2);
const DAEMONS: [&str; 2] = ["/usr/lib/gsd-power", "/usr/libexec/gsd-power"];

#[derive(Clone, Debug)]
pub struct Fact {
    pub unique: String,
    pub epoch: u64,
    pub uid: u32,
    pub pid: u32,
    pub start: String,
    checked: Instant,
}
impl Fact {
    pub fn fresh(&self, now: Instant) -> bool {
        self.epoch != 0 && admission_fresh(self.checked, now)
    }
}

fn admission_fresh(checked: Instant, now: Instant) -> bool {
    now.checked_duration_since(checked)
        .is_some_and(|v| v <= DEADLINE)
}

pub struct Observer {
    state: Arc<Mutex<Option<Fact>>>,
    stop: Arc<AtomicBool>,
}
impl Observer {
    /// Nested sessions never acquire hardware policy authority from host GSD.
    /// All bus setup and package-process waits stay on this single worker;
    /// method deadlines do not promise a bound on bus connection establishment
    /// or an uninterruptible kernel child wait. Cached facts expire regardless.
    pub fn start(native: bool) -> Self {
        let state = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        if native {
            let worker_state = state.clone();
            let worker_stop = stop.clone();
            if std::thread::Builder::new()
                .name("brightness-provider".into())
                .spawn(move || observe(worker_state, worker_stop))
                .is_err()
            {
                stop.store(true, Ordering::Release);
            }
        }
        Self { state, stop }
    }
    /// In-memory only. Worker stalls expire authority instead of extending it.
    pub fn fact(&self) -> Option<Fact> {
        self.state
            .lock()
            .ok()?
            .as_ref()
            .filter(|v| v.fresh(Instant::now()))
            .cloned()
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(mut state) = self.state.lock() {
            *state = None;
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    path: PathBuf,
    dev: u64,
    ino: u64,
    size: u64,
    modified: (i64, i64),
}
fn installed(path: &Path) -> Option<FileIdentity> {
    let path = path.canonicalize().ok()?;
    if !path.starts_with("/usr") {
        return None;
    }
    for parent in path.ancestors() {
        let m = parent.metadata().ok()?;
        if m.uid() != 0 || m.mode() & 0o022 != 0 {
            return None;
        }
    }
    let m = path.metadata().ok()?;
    if !m.is_file() || m.len() == 0 || m.len() > 128 * 1024 * 1024 {
        return None;
    }
    Some(FileIdentity {
        path,
        dev: m.dev(),
        ino: m.ino(),
        size: m.len(),
        modified: (m.mtime(), m.mtime_nsec()),
    })
}

/// Package query collection has a deadline and capped output. Owned kill/reap
/// remains off-frame; an uninterruptible kernel wait is not a hard time bound.
fn bounded_command(program: &str, args: &[&str]) -> Option<String> {
    let executable = installed(Path::new(program))?;
    let mut child = Command::new(executable.path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let result = (|| {
        let mut output = child.stdout.take()?;
        let fd = output.as_raw_fd();
        // SAFETY: F_GETFL/F_SETFL operate on the borrowed live pipe descriptor.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return None;
        }
        let start = Instant::now();
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 4096];
            match output.read(&mut chunk) {
                Ok(0) => {
                    if let Some(status) = child.try_wait().ok()? {
                        return status
                            .success()
                            .then(|| String::from_utf8(bytes).ok())
                            .flatten();
                    }
                }
                Ok(n) => {
                    if bytes.len() + n > 16 * 1024 {
                        return None;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return None,
            }
            if start.elapsed() >= DEADLINE {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn process_start(pid: u32) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(format!("/proc/{pid}/stat"))
        .ok()?
        .take(8193)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 8192 {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    text.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}

fn package(path: &Path) -> Option<String> {
    let path = path.to_str()?;
    let receipt = if Path::new("/usr/bin/pacman").exists() {
        let name = bounded_command("/usr/bin/pacman", &["-Qoq", path])?;
        // One fixed known installed package; arbitrary query arguments denied.
        if name.trim() != "gnome-settings-daemon" || name.lines().count() != 1 {
            return None;
        }
        bounded_command("/usr/bin/pacman", &["-Q", "gnome-settings-daemon"])?
    } else {
        bounded_command(
            "/usr/bin/rpm",
            &[
                "-qf",
                "--qf",
                "%{NAME} %{VERSION}-%{RELEASE}.%{ARCH}\n",
                path,
            ],
        )?
    };
    valid_package(&receipt).then_some(receipt)
}
fn valid_package(receipt: &str) -> bool {
    receipt.starts_with("gnome-settings-daemon 51.") && receipt.lines().count() == 1
}
struct Owner {
    unique: String,
    pid: u32,
    start: String,
    executable: FileIdentity,
    credentials: zbus::fdo::ConnectionCredentials,
}
impl Owner {
    fn identify(conn: &zbus::blocking::Connection, unique: String) -> Option<Self> {
        if !unique.starts_with(':') || unique.len() > 255 {
            return None;
        }
        let bus = zbus::blocking::Proxy::new(
            conn,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .ok()?;
        let credentials: zbus::fdo::ConnectionCredentials = bus
            .call("GetConnectionCredentials", &(unique.as_str(),))
            .ok()?;
        if credentials.unix_user_id() != Some(rustix::process::geteuid().as_raw()) {
            return None;
        }
        let pid = crate::capture_security::pinned_process_id(&credentials)?;
        let start = process_start(pid)?;
        let actual = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        let executable = DAEMONS
            .iter()
            .filter_map(|p| installed(Path::new(p)))
            .find(|exe| exe.path == actual)?;
        package(&executable.path)?;
        let owner = Self {
            unique,
            pid,
            start,
            executable,
            credentials,
        };
        owner.alive().then_some(owner)
    }
    fn alive(&self) -> bool {
        crate::capture_security::pinned_process_id(&self.credentials) == Some(self.pid)
            && process_start(self.pid).as_ref() == Some(&self.start)
            && std::fs::read_link(format!("/proc/{}/exe", self.pid))
                .ok()
                .as_ref()
                == Some(&self.executable.path)
            && installed(&self.executable.path).as_ref() == Some(&self.executable)
    }
}
fn unique_owner(conn: &zbus::blocking::Connection) -> Option<String> {
    zbus::blocking::Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .ok()?
    .call("GetNameOwner", &(NAME,))
    .ok()
}
fn observe(state: Arc<Mutex<Option<Fact>>>, stop: Arc<AtomicBool>) {
    let Ok(conn) = zbus::blocking::connection::Builder::session()
        .map(|b| b.method_timeout(DEADLINE))
        .and_then(|b| b.build())
    else {
        return;
    };
    let mut owner: Option<Owner> = None;
    let mut epoch = 0u64;
    while !stop.load(Ordering::Acquire) {
        let unique = unique_owner(&conn);
        let unchanged = owner
            .as_ref()
            .is_some_and(|v| Some(&v.unique) == unique.as_ref() && v.alive());
        if !unchanged {
            // Revoke before potentially slow proc/package/new-owner admission.
            if let Ok(mut current) = state.lock() {
                *current = None;
            }
            let Some(next) = epoch.checked_add(1) else {
                return;
            };
            epoch = next;
            owner = unique.and_then(|v| Owner::identify(&conn, v));
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        // Pin both original process and current unique bus owner after receipt.
        let admission = Instant::now();
        let fact = owner
            .as_ref()
            .filter(|v| {
                v.alive()
                    && unique_owner(&conn).as_ref() == Some(&v.unique)
                    && v.alive()
                    && admission_fresh(admission, Instant::now())
            })
            .map(|v| Fact {
                unique: v.unique.clone(),
                epoch,
                uid: rustix::process::geteuid().as_raw(),
                pid: v.pid,
                start: v.start.clone(),
                // A delayed bus response must never renew an old process fact.
                checked: admission,
            });
        if let Ok(mut current) = state.lock() {
            *current = if stop.load(Ordering::Acquire) {
                None
            } else {
                fact
            };
        }
        // Finite one worker; cancellation checked without spawning generations.
        for _ in 0..25 {
            if stop.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if let Ok(mut current) = state.lock() {
        *current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_identity_is_fixed_and_versioned() {
        assert!(valid_package("gnome-settings-daemon 51.0-2\n"));
        for receipt in [
            "forged 51.0-2\n",
            "gnome-settings-daemon 50.3-1\n",
            "gnome-settings-daemon 51.0-2\nother 1\n",
        ] {
            assert!(!valid_package(receipt));
        }
    }
    #[test]
    fn stale_and_future_facts_are_not_authority() {
        let now = Instant::now();
        let mut fact = Fact {
            unique: ":1.2".into(),
            epoch: 1,
            uid: 1000,
            pid: 1,
            start: "1".into(),
            checked: now,
        };
        assert!(fact.fresh(now));
        assert!(!fact.fresh(now + Duration::from_secs(3)));
        assert!(!fact.fresh(now - Duration::from_millis(1)));
        fact.epoch = 0;
        assert!(!fact.fresh(now));
    }
    #[test]
    fn nested_observer_never_admits_host_provider() {
        let observer = Observer::start(false);
        assert!(observer.fact().is_none());
    }
    #[test]
    fn admission_uses_original_time_through_delayed_owner_response() {
        let original = Instant::now();
        assert!(admission_fresh(original, original + DEADLINE));
        assert!(!admission_fresh(
            original,
            original + DEADLINE + Duration::from_nanos(1)
        ));
        assert!(!admission_fresh(
            original,
            original - Duration::from_nanos(1)
        ));
        let delayed_response = original + Duration::from_secs(3);
        let fact = Fact {
            unique: ":1.2".into(),
            epoch: 1,
            uid: 1000,
            pid: 42,
            start: "7".into(),
            checked: original,
        };
        assert!(!fact.fresh(delayed_response));
        // Publishing original checked time cannot grant another two seconds.
        assert!(!fact.fresh(delayed_response + Duration::from_millis(1)));
    }
}
