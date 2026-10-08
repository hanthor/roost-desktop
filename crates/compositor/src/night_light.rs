//! Genuine GNOME color policy, observed away from the rendering thread.
//! GNOME owns scheduling/preview/temporary disable. Only its effective
//! Temperature and libcolord's Planckian scales reach the display stage.
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::{fs::MetadataExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

const SERVICE: &str = "org.gnome.SettingsDaemon.Color";
const OBJECT: &str = "/org/gnome/SettingsDaemon/Color";
const DEADLINE: Duration = Duration::from_secs(2);
const FRESHNESS: Duration = Duration::from_secs(3);
const DAEMONS: [&str; 2] = ["/usr/lib/gsd-color", "/usr/libexec/gsd-color"];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    pub owner_epoch: u64,
    pub generation: u64,
    pub temperature: u32,
    pub rgb: [f32; 3],
    /// Genuine current service and valid scales, independent of GPU capability.
    pub supported: bool,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            owner_epoch: 0,
            generation: 0,
            temperature: 6500,
            rgb: [1.0; 3],
            supported: false,
        }
    }
}
struct State {
    snapshot: Snapshot,
    observed: Instant,
}
impl State {
    fn current(&self) -> Snapshot {
        self.current_at(Instant::now())
    }
    fn current_at(&self, now: Instant) -> Snapshot {
        if now
            .checked_duration_since(self.observed)
            .is_some_and(|age| age <= FRESHNESS)
        {
            self.snapshot
        } else {
            Snapshot {
                owner_epoch: self.snapshot.owner_epoch,
                generation: self.snapshot.generation.wrapping_add(1),
                ..Snapshot::default()
            }
        }
    }
    fn publish(&mut self, epoch: u64, value: Option<(u32, [f32; 3], Instant)>) {
        self.publish_at(epoch, value, Instant::now());
    }
    fn publish_at(&mut self, epoch: u64, value: Option<(u32, [f32; 3], Instant)>, now: Instant) {
        let (temperature, rgb, supported, observed) = value
            .filter(|(_, _, observed)| {
                now.checked_duration_since(*observed)
                    .is_some_and(|age| age <= FRESHNESS)
            })
            .map(|(t, rgb, observed)| (t, rgb, true, observed))
            .unwrap_or((6500, [1.0; 3], false, now));
        let next = Snapshot {
            owner_epoch: epoch,
            generation: self.snapshot.generation,
            temperature,
            rgb,
            supported,
        };
        if next != self.snapshot {
            self.snapshot = Snapshot {
                generation: self.snapshot.generation.wrapping_add(1),
                ..next
            };
        }
        self.observed = observed;
    }
}

/// Reads are bounded in-memory copies. Shutdown tells the worker to reap only
/// its owned child; an already running GNOME daemon is never stopped.
#[derive(Clone)]
pub struct Capability {
    state: Arc<Mutex<State>>,
    ready: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
}
impl Capability {
    pub fn supported(&self) -> bool {
        self.alive.load(Ordering::Acquire)
            && self.ready.load(Ordering::Acquire)
            && self
                .state
                .lock()
                .is_ok_and(|state| state.current().supported)
    }
}

pub struct Observer {
    state: Arc<Mutex<State>>,
    alive: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
}
impl Observer {
    pub fn start(native: bool, renderer_ready: Arc<AtomicBool>, socket_name: &str) -> Self {
        let session = SessionEnvironment {
            socket: socket_name.to_owned(),
            desktop: std::env::var("XDG_CURRENT_DESKTOP")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "Roost:GNOME".into()),
        };
        let state = Arc::new(Mutex::new(State {
            snapshot: Snapshot::default(),
            observed: Instant::now(),
        }));
        let alive = Arc::new(AtomicBool::new(true));
        let observer = Self {
            state: state.clone(),
            alive: alive.clone(),
            ready: renderer_ready.clone(),
        };
        let _ = std::thread::Builder::new()
            .name("roost-night-light".into())
            .spawn(move || {
                worker(native, session, state, alive);
            });
        observer
    }
    pub fn snapshot(&self) -> Snapshot {
        self.state
            .lock()
            .map(|state| state.current())
            .unwrap_or_default()
    }
    pub fn capability(&self) -> Capability {
        Capability {
            state: self.state.clone(),
            ready: self.ready.clone(),
            alive: self.alive.clone(),
        }
    }
    pub fn set_renderer_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.ready.store(false, Ordering::Release);
        self.alive.store(false, Ordering::Release);
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

/// Package queries have a hard deadline and capped output, including a child
/// that holds stdout open or emits more than the receipt budget.
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
fn package(path: &Path, daemon: bool) -> Option<String> {
    let path = path.to_str()?;
    let receipt = if Path::new("/usr/bin/pacman").exists() {
        let name = bounded_command("/usr/bin/pacman", &["-Qoq", path])?;
        bounded_command("/usr/bin/pacman", &["-Q", name.trim()])?
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
    valid_package_receipt(&receipt, daemon).then_some(receipt)
}
fn valid_package_receipt(receipt: &str, daemon: bool) -> bool {
    let allowed = if daemon {
        receipt.starts_with("gnome-settings-daemon 51.")
    } else {
        ["colord ", "colord-libs ", "libcolord "]
            .iter()
            .any(|prefix| receipt.starts_with(prefix))
    };
    allowed && receipt.lines().count() == 1
}

#[repr(C)]
#[derive(Default)]
struct CdRgb {
    red: f64,
    green: f64,
    blue: f64,
}
type Blackbody = unsafe extern "C" fn(f64, *mut CdRgb, libc::c_uint) -> libc::c_int;
struct ColorLibrary {
    library: libloading::Library,
    identity: FileIdentity,
}
impl ColorLibrary {
    fn load() -> Option<Self> {
        for name in ["/usr/lib/libcolord.so.2", "/usr/lib64/libcolord.so.2"] {
            let Some(identity) = installed(Path::new(name)) else {
                continue;
            };
            let Some(receipt) = package(&identity.path, false) else {
                continue;
            };
            // SAFETY: immutable installed library; its ABI is checked below and
            // remains loaded while the borrowed symbol is invoked.
            let Ok(library) = (unsafe { libloading::Library::new(&identity.path) }) else {
                continue;
            };
            let result = Self { library, identity };
            result.scales(6500)?;
            eprintln!(
                "roost-compositor: night light: color backend {}",
                receipt.trim()
            );
            return Some(result);
        }
        None
    }
    fn scales(&self, temperature: u32) -> Option<[f32; 3]> {
        if !(1000..=10000).contains(&temperature) {
            return None;
        }
        // ABI: CdColorRGB is three gdouble fields; gboolean is int; the enum's
        // USE_PLANCKIAN is 1 (not a guessed formula/table or default flag 0).
        let function = unsafe {
            self.library
                .get::<Blackbody>(b"cd_color_get_blackbody_rgb_full\0")
        }
        .ok()?;
        let mut result = CdRgb::default();
        if unsafe { function(f64::from(temperature), &mut result, 1) } == 0 {
            return None;
        }
        validated_rgb([result.red, result.green, result.blue])
    }
}
fn validated_rgb(rgb: [f64; 3]) -> Option<[f32; 3]> {
    rgb.iter()
        .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        .then(|| rgb.map(|v| v as f32))
}

struct Owner {
    unique: String,
    pid: u32,
    start: String,
    executable: FileIdentity,
    credentials: zbus::fdo::ConnectionCredentials,
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
        let actual = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        let executable = DAEMONS
            .iter()
            .filter_map(|p| installed(Path::new(p)))
            .find(|exe| exe.path == actual)?;
        let receipt = package(&executable.path, true)?;
        let start = process_start(pid)?;
        let owner = Self {
            unique,
            pid,
            start,
            executable,
            credentials,
        };
        if !owner.alive() {
            return None;
        }
        eprintln!(
            "roost-compositor: night light: genuine daemon {} pid {}",
            receipt.trim(),
            pid
        );
        Some(owner)
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
/// Explicit child's own session, captured before spawning the observer. Do
/// not inherit a parent display or race later activation-environment import.
struct SessionEnvironment {
    socket: String,
    desktop: String,
}
impl SessionEnvironment {
    fn apply(&self, command: &mut Command) {
        command
            .env("WAYLAND_DISPLAY", &self.socket)
            .env("GDK_BACKEND", "wayland")
            .env("XDG_SESSION_TYPE", "wayland")
            .env("XDG_CURRENT_DESKTOP", &self.desktop)
            .env_remove("DISPLAY");
    }
}
struct OwnedDaemon(Child);
impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn launch(session: &SessionEnvironment) -> Option<OwnedDaemon> {
    if session.socket.is_empty() || session.socket.len() > 255 || session.desktop.len() > 255 {
        return None;
    }
    let executable = DAEMONS
        .iter()
        .filter_map(|p| installed(Path::new(p)))
        .find(|exe| package(&exe.path, true).is_some())?;
    let mut command = Command::new(&executable.path);
    session.apply(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let original_parent = std::process::id() as libc::pid_t;
    // SAFETY: only async-signal-safe system calls run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                || libc::getppid() != original_parent
            {
                return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
            }
            Ok(())
        });
    }
    command.spawn().ok().map(OwnedDaemon)
}
fn name_owner(conn: &zbus::blocking::Connection) -> Option<String> {
    zbus::blocking::Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .ok()?
    .call("GetNameOwner", &(SERVICE,))
    .ok()
}
fn worker(
    native: bool,
    session: SessionEnvironment,
    state: Arc<Mutex<State>>,
    alive: Arc<AtomicBool>,
) {
    let Some(colors) = ColorLibrary::load() else {
        return;
    };
    let Ok(conn) = zbus::blocking::connection::Builder::session()
        .map(|b| b.method_timeout(DEADLINE))
        .and_then(|b| b.build())
    else {
        return;
    };
    let mut owner: Option<Owner> = None;
    let mut epoch = 0u64;
    let mut child: Option<OwnedDaemon> = None;
    let mut attempts = 0;
    let mut last_attempt = Instant::now()
        .checked_sub(Duration::from_secs(5))
        .unwrap_or_else(Instant::now);
    while alive.load(Ordering::Acquire) {
        let unique = name_owner(&conn);
        if owner
            .as_ref()
            .is_some_and(|o| unique.as_ref() != Some(&o.unique) || !o.alive())
        {
            owner = None;
            epoch = epoch.wrapping_add(1);
        }
        if owner.is_none() {
            if let Some(unique) = unique {
                owner = Owner::identify(&conn, unique);
                if owner.is_some() {
                    epoch = epoch.wrapping_add(1);
                }
            } else if native
                && child.is_none()
                && attempts < 3
                && last_attempt.elapsed() >= Duration::from_secs(5)
            {
                attempts += 1;
                last_attempt = Instant::now();
                child = launch(&session);
            }
        }
        let value = owner.as_ref().and_then(|owner| {
            if installed(&colors.identity.path).as_ref() != Some(&colors.identity) {
                return None;
            }
            let properties = zbus::blocking::Proxy::new(
                &conn,
                owner.unique.as_str(),
                OBJECT,
                "org.freedesktop.DBus.Properties",
            )
            .ok()?;
            // Freshness starts before the actual property read, not after
            // subsequent credential/name checks finish.
            let observed = Instant::now();
            let value: zbus::zvariant::OwnedValue =
                properties.call("Get", &(SERVICE, "Temperature")).ok()?;
            let temperature = u32::try_from(value).ok()?;
            let rgb = colors.scales(temperature)?;
            // Refuse a completion from a replaced/executed original daemon.
            (owner.alive() && name_owner(&conn).as_ref() == Some(&owner.unique)).then_some((
                temperature,
                rgb,
                observed,
            ))
        });
        if !alive.load(Ordering::Acquire) {
            break;
        }
        if let Ok(mut state) = state.lock() {
            state.publish(epoch, value);
        }
        if child
            .as_mut()
            .is_some_and(|c| c.0.try_wait().ok().flatten().is_some())
        {
            child = None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn effective_value_and_owner_epoch_invalidate_cached_transform() {
        let mut state = State {
            snapshot: Snapshot::default(),
            observed: Instant::now(),
        };
        state.publish(1, Some((4000, [1.0, 0.8, 0.5], Instant::now())));
        let first = state.current();
        state.publish(1, Some((4000, [1.0, 0.8, 0.5], Instant::now())));
        assert_eq!(first.generation, state.current().generation);
        state.publish(2, Some((4000, [1.0, 0.8, 0.5], Instant::now())));
        assert_ne!(first.generation, state.current().generation);
        state.publish(3, None);
        assert!(!state.current().supported);
        assert_eq!(state.current().rgb, [1.0; 3]);
    }
    #[test]
    fn stale_observation_is_neutral_and_cannot_advertise_support() {
        let state = State {
            snapshot: Snapshot {
                supported: true,
                temperature: 4000,
                rgb: [1.0, 0.8, 0.5],
                owner_epoch: 4,
                generation: 7,
            },
            observed: Instant::now() - FRESHNESS - Duration::from_millis(1),
        };
        assert_eq!(
            state.current(),
            Snapshot {
                owner_epoch: 4,
                generation: 8,
                ..Snapshot::default()
            }
        );
    }
    #[test]
    fn capability_requires_live_service_and_all_output_pipeline() {
        let state = Arc::new(Mutex::new(State {
            snapshot: Snapshot {
                supported: true,
                ..Snapshot::default()
            },
            observed: Instant::now(),
        }));
        let ready = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let cap = Capability {
            state: state.clone(),
            ready: ready.clone(),
            alive: alive.clone(),
        };
        assert!(!cap.supported());
        ready.store(true, Ordering::Release);
        assert!(cap.supported());
        state.lock().unwrap().observed = Instant::now() - FRESHNESS - Duration::from_millis(1);
        assert!(!cap.supported());
        state
            .lock()
            .unwrap()
            .publish(2, Some((4000, [1.0, 0.8, 0.5], Instant::now())));
        assert!(cap.supported());
        alive.store(false, Ordering::Release);
        assert!(!cap.supported());
    }
    #[test]
    fn expiry_and_recovery_both_change_visible_transform_signature() {
        let mut state = State {
            snapshot: Snapshot::default(),
            observed: Instant::now(),
        };
        state.publish(1, Some((4000, [1.0, 0.8, 0.5], Instant::now())));
        let warm = state.current();
        state.observed = Instant::now() - FRESHNESS - Duration::from_millis(1);
        let stale = state.current();
        assert_ne!((warm.generation, warm.rgb), (stale.generation, stale.rgb));
        state.publish(1, Some((4000, [1.0, 0.8, 0.5], Instant::now())));
        let recovered = state.current();
        assert_ne!(
            (stale.generation, stale.rgb),
            (recovered.generation, recovered.rgb)
        );
        assert_eq!(warm.rgb, recovered.rgb);
    }
    #[test]
    fn delayed_read_completion_is_rejected_without_a_fresh_warm_timestamp() {
        let now = Instant::now();
        let mut state = State {
            snapshot: Snapshot::default(),
            observed: now,
        };
        let original = now - FRESHNESS - Duration::from_nanos(1);
        state.publish_at(1, Some((4000, [1.0, 0.8, 0.5], original)), now);
        assert!(!state.current_at(now).supported);
        assert_eq!(state.current_at(now).rgb, [1.0; 3]);
        assert_eq!(state.observed, now);
        let neutral_generation = state.snapshot.generation;
        state.publish_at(1, Some((4000, [1.0, 0.8, 0.5], now)), now);
        assert!(state.current_at(now).supported);
        assert_ne!(state.snapshot.generation, neutral_generation);
        assert_eq!(state.observed, now);
    }
    #[test]
    fn completion_preserves_read_time_and_exact_freshness_boundary() {
        let now = Instant::now();
        let mut state = State {
            snapshot: Snapshot::default(),
            observed: now,
        };
        let original = now - FRESHNESS;
        state.publish_at(2, Some((4000, [1.0, 0.8, 0.5], original)), now);
        assert_eq!(state.observed, original);
        assert!(state.current_at(now).supported);
        assert!(!state.current_at(now + Duration::from_nanos(1)).supported);
        let future = now + Duration::from_nanos(1);
        state.publish_at(2, Some((4000, [1.0, 0.8, 0.5], future)), now);
        assert!(!state.current_at(now).supported);
    }
    #[test]
    fn owned_daemon_drop_reaps_original_child_without_stopping_other_processes() {
        let owned = Command::new("/usr/bin/sleep").arg("30").spawn().unwrap();
        let pid = owned.id();
        let mut other = Command::new("/usr/bin/sleep").arg("30").spawn().unwrap();
        drop(OwnedDaemon(owned));
        assert!(process_start(pid).is_none());
        assert!(other.try_wait().unwrap().is_none());
        other.kill().unwrap();
        other.wait().unwrap();
    }
    #[test]
    fn installed_package_contract_rejects_other_major_and_wrong_owner() {
        assert!(valid_package_receipt(
            "gnome-settings-daemon 51.0-1\n",
            true
        ));
        assert!(!valid_package_receipt(
            "gnome-settings-daemon 52.0-1\n",
            true
        ));
        assert!(!valid_package_receipt("fake-color-service 51.0\n", true));
        assert!(!valid_package_receipt(
            "gnome-settings-daemon 51.0\nother 1.0\n",
            true
        ));
        assert!(valid_package_receipt("libcolord 1.4.8-1\n", false));
        assert!(valid_package_receipt(
            "colord-libs 1.4.8-2.fc45.x86_64\n",
            false
        ));
        assert!(!valid_package_receipt("unrelated-loader 1.4.8\n", false));
    }
    #[test]
    fn malformed_native_scales_never_enter_display_state() {
        for bad in [f64::NAN, f64::INFINITY, -0.001, 1.001] {
            assert!(validated_rgb([1.0, bad, 0.5]).is_none());
        }
        assert_eq!(validated_rgb([1.0, 0.5, 0.0]), Some([1.0, 0.5, 0.0]));
    }
    #[test]
    fn child_command_pins_original_session_without_mutating_parent_environment() {
        let before = std::env::var_os("WAYLAND_DISPLAY");
        let session = SessionEnvironment {
            socket: "wayland-original-roost".into(),
            desktop: "Roost:GNOME".into(),
        };
        let mut command = Command::new("/usr/lib/gsd-color");
        command
            .env("DISPLAY", ":parent")
            .env("WAYLAND_DISPLAY", "parent-wayland");
        session.apply(&mut command);
        let env: std::collections::HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            env.get(std::ffi::OsStr::new("WAYLAND_DISPLAY"))
                .copied()
                .flatten(),
            Some(std::ffi::OsStr::new("wayland-original-roost"))
        );
        assert_eq!(env.get(std::ffi::OsStr::new("DISPLAY")), Some(&None));
        assert_eq!(
            env.get(std::ffi::OsStr::new("GDK_BACKEND"))
                .copied()
                .flatten(),
            Some(std::ffi::OsStr::new("wayland"))
        );
        assert_eq!(std::env::var_os("WAYLAND_DISPLAY"), before);
    }
    #[test]
    fn arbitrary_process_is_not_a_genuine_color_daemon() {
        assert!(!DAEMONS
            .iter()
            .filter_map(|p| installed(Path::new(p)))
            .any(
                |exe| std::fs::read_link(format!("/proc/{}/exe", std::process::id())).ok()
                    == Some(exe.path)
            ));
    }
}
