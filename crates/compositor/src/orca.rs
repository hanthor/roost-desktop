//! Genuine distribution Orca, owned by the hardware compositor rather than a
//! global user-manager service. No --replace, global PID selection or bus-stop
//! request can affect another display's reader.
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tuna_shell_control::ScreenReaderState as State;

/// Orca 50.3 and 51 expose different genuine remote-controller names.
const NAMES: [&str; 2] = ["org.gnome.Orca1.Service", "org.gnome.Orca.Service"];
/// GNOME starts Orca from session autostart and never kills it for being slow
/// to claim its bus name; on a cold VM speech-dispatcher alone can take several
/// seconds. Past this grace we only report the slow start, and keep watching.
const READINESS_GRACE: Duration = Duration::from_secs(10);
/// A stopped reader's names drop as soon as the bus daemon processes its
/// disconnect. This bound only guards against a wedged bus.
const DEPARTURE_BOUND: Duration = Duration::from_secs(5);

/// Observer for one owned child. Dropping it cancels the watcher thread.
struct Observation {
    receiver: Receiver<(u32, State, &'static str)>,
    cancel: Arc<AtomicBool>,
}
impl Drop for Observation {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub struct Reader {
    display: String,
    enabled: bool,
    child: Option<Child>,
    observed: Option<Observation>,
    /// Pending until the last stopped child's bus names have vanished; a new
    /// Orca started earlier would lose the name race to its own predecessor.
    departing: Option<Receiver<()>>,
    state: State,
    sent: State,
    attempts: u8,
    next_start: Instant,
}
impl Reader {
    pub fn new(display: &str) -> Self {
        Self {
            display: display.into(),
            enabled: false,
            child: None,
            observed: None,
            departing: None,
            state: State::Disabled,
            sent: State::Disabled,
            attempts: 0,
            next_start: Instant::now(),
        }
    }
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }
    pub fn status(&self) -> State {
        self.state
    }
    pub fn state(&self) -> &'static str {
        match self.state {
            State::Disabled => "disabled",
            State::Starting => "starting",
            State::Active => "active",
            State::Unavailable => "unavailable",
            State::Conflict => "conflict",
        }
    }
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.attempts = 0;
        self.next_start = Instant::now();
        if !enabled {
            self.stop();
            self.state = State::Disabled;
        }
    }
    fn stop(&mut self) {
        self.observed = None;
        if let Some(mut child) = self.child.take() {
            // This unreaped Child handle is authority over precisely our process.
            // It cannot designate a recycled PID or another display's reader.
            let pid = child.id();
            let _ = child.kill();
            let until = Instant::now() + Duration::from_secs(1);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < until => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    _ => {
                        eprintln!("tuna-compositor: owned Orca reap deadline exceeded");
                        break;
                    }
                }
            }
            self.await_departure(pid);
        }
    }
    fn await_departure(&mut self, pid: u32) {
        let (sender, receiver) = mpsc::channel();
        self.departing = Some(receiver);
        std::thread::spawn(move || {
            let until = Instant::now() + DEPARTURE_BOUND;
            while departing_owner(pid, &bus_call) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = sender.send(());
        });
    }
    /// True while a stopped child may still hold an Orca name.
    fn departure_pending(&mut self) -> bool {
        match self.departing.as_ref().map(Receiver::try_recv) {
            Some(Err(TryRecvError::Empty)) => true,
            Some(_) => {
                self.departing = None;
                false
            }
            None => false,
        }
    }
    pub fn poll(&mut self, diagnostics_visible: bool) -> Option<State> {
        let mut observed_running = false;
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if diagnostics_visible {
                        eprintln!("TUNA_ORCA_DIAGNOSTIC stage=child-exit pid={} attempt={} code={} signal={}", child.id(), self.attempts, status.code().unwrap_or(-1), status.signal().unwrap_or(0));
                    }
                    let pid = child.id();
                    self.child = None;
                    self.observed = None;
                    self.await_departure(pid);
                    self.state = State::Unavailable;
                    // Startup exit 1 includes the genuine singleton conflict.
                    // Do not repeatedly race another session for its reader.
                    if status.code() == Some(1) {
                        self.attempts = 3;
                    }
                    self.next_start = Instant::now() + Duration::from_secs(2);
                }
                Err(error) => {
                    if diagnostics_visible {
                        eprintln!(
                            "TUNA_ORCA_DIAGNOSTIC stage=child-wait-error pid={} attempt={} kind={}",
                            child.id(),
                            self.attempts,
                            error_kind(error.kind())
                        );
                    }
                    self.stop();
                    self.state = State::Unavailable;
                    self.attempts = 3;
                }
                Ok(None) => observed_running = true,
            }
        }
        if let Some(observation) = &self.observed {
            if let Ok((pid, state, reason)) = observation.receiver.try_recv() {
                if self.pid() == Some(pid) {
                    if state == State::Active {
                        self.state = state;
                    } else if state == State::Starting {
                        // Slow or re-registering, but alive: GNOME keeps it.
                        if diagnostics_visible {
                            eprintln!(
                                "TUNA_ORCA_DIAGNOSTIC stage={} pid={} attempt={} observed_running={}",
                                reason, pid, self.attempts, observed_running
                            );
                        }
                        self.state = state;
                    } else {
                        if diagnostics_visible {
                            eprintln!(
                                "TUNA_ORCA_DIAGNOSTIC stage={} pid={} attempt={} observed_running={}",
                                reason, pid, self.attempts, observed_running
                            );
                        }
                        self.stop();
                        self.state = state;
                        self.attempts = 3;
                    }
                }
            }
        }
        if self.enabled
            && self.child.is_none()
            && self.attempts < 3
            && Instant::now() >= self.next_start
            && !self.departure_pending()
        {
            self.attempts += 1;
            let mut command = Command::new("/usr/bin/orca");
            command
                .env("WAYLAND_DISPLAY", &self.display)
                .env("XDG_SESSION_TYPE", "wayland")
                // Orca's setproctitle must preserve /proc environ so the native
                // lifecycle probe can verify this owned child's actual display.
                .env("SPT_NOENV", "1")
                .env_remove("DISPLAY")
                .env_remove("NOTIFY_SOCKET")
                .env_remove("WATCHDOG_USEC")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit());
            parent_lifetime(&mut command);
            match command.spawn() {
                Ok(child) => {
                    let pid = child.id();
                    let (sender, receiver) = mpsc::channel();
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.child = Some(child);
                    self.state = State::Starting;
                    self.observed = Some(Observation {
                        receiver,
                        cancel: cancel.clone(),
                    });
                    std::thread::spawn(move || {
                        let report = |state, reason| sender.send((pid, state, reason)).is_ok();
                        watch_orca_name(pid, &bus_call, &cancel, READINESS_GRACE, report)
                    });
                }
                Err(error) => {
                    if diagnostics_visible {
                        eprintln!(
                            "TUNA_ORCA_DIAGNOSTIC stage=child-spawn pid=0 attempt={} kind={}",
                            self.attempts,
                            error_kind(error.kind())
                        );
                    }
                    self.state = State::Unavailable;
                    self.next_start = Instant::now() + Duration::from_secs(2);
                }
            }
        }
        if self.sent != self.state {
            self.sent = self.state;
            Some(self.state)
        } else {
            None
        }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.stop();
    }
}

fn parent_lifetime(command: &mut Command) {
    let parent = unsafe { libc::getpid() };
    // Only async-signal-safe libc calls run after fork. Linux clears this setting
    // for a forked child, so speech-dispatcher is not accidentally bound to us.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(127);
            }
            Ok(())
        });
    }
}

/// Bounded observation only. A competing owner can make us stop OUR Child,
/// never another process; process ownership does not come from these snapshots.
fn error_kind(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::NotFound => "not-found",
        std::io::ErrorKind::PermissionDenied => "permission-denied",
        std::io::ErrorKind::Interrupted => "interrupted",
        std::io::ErrorKind::InvalidData => "invalid-data",
        _ => "other",
    }
}
fn bus_call(method: &str, arg: &str) -> Result<String, &'static str> {
    let mut command = Command::new("gdbus");
    command
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            method,
            arg,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    bus_call_command(command)
}
fn bus_call_command(mut command: Command) -> Result<String, &'static str> {
    let mut child = command.spawn().map_err(|_| "bus-spawn")?;
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut data = Vec::new();
                child
                    .stdout
                    .take()
                    .ok_or("bus-read")?
                    .take(1025)
                    .read_to_end(&mut data)
                    .map_err(|_| "bus-read")?;
                if data.len() > 1024 {
                    return Err("bus-oversize");
                }
                return String::from_utf8(data).map_err(|_| "bus-malformed");
            }
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            result => {
                let reason = match result {
                    Ok(Some(_)) => "bus-nonzero",
                    Ok(None) => "bus-timeout",
                    Err(_) => "bus-wait-error",
                };
                let _ = child.kill();
                let _ = child.wait();
                return Err(reason);
            }
        }
    }
}
/// One snapshot of the Orca names against the owned child's credentials.
#[derive(Debug, PartialEq)]
enum Probe {
    Active,
    /// No name yet, or an owner vanished between bus calls.
    Absent(&'static str),
    Failed(State, &'static str),
}
fn probe_orca_name<B>(pid: u32, euid: u32, bus: &B) -> Probe
where
    B: Fn(&str, &str) -> Result<String, &'static str>,
{
    let mut last_reason = "name-absent";
    let mut active = false;
    // A foreign owner of either contract is a conflict, never replacement.
    'names: for name in NAMES {
        let reply = match bus("org.freedesktop.DBus.GetNameOwner", name) {
            Ok(reply) => reply,
            Err(reason) => {
                if last_reason != "owner-vanished" {
                    last_reason = reason;
                }
                continue;
            }
        };
        let Some(owner) = parse_owner(&reply) else {
            return Probe::Failed(State::Unavailable, "owner-malformed");
        };
        for (method, expected) in [
            ("org.freedesktop.DBus.GetConnectionUnixProcessID", pid),
            ("org.freedesktop.DBus.GetConnectionUnixUser", euid),
        ] {
            let actual = match bus(method, owner) {
                Ok(reply) => parse_uint(&reply),
                // The owner disconnected after GetNameOwner answered.
                Err("bus-nonzero") => {
                    last_reason = "owner-vanished";
                    continue 'names;
                }
                Err(reason) => return Probe::Failed(State::Unavailable, reason),
            };
            match actual {
                Some(actual) if actual == expected => {}
                Some(_) => return Probe::Failed(State::Conflict, "credentials-conflict"),
                None => return Probe::Failed(State::Unavailable, "credentials-malformed"),
            }
        }
        active = true;
    }
    if active {
        Probe::Active
    } else {
        Probe::Absent(last_reason)
    }
}
fn parse_owner(reply: &str) -> Option<&str> {
    let owner = reply.split('\'').nth(1)?;
    (owner.starts_with(':')
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_digit() || b":.".contains(&byte)))
    .then_some(owner)
}
fn parse_uint(reply: &str) -> Option<u32> {
    reply
        .trim()
        .strip_prefix("(uint32 ")
        .and_then(|s| s.strip_suffix(",)"))
        .and_then(|s| s.parse::<u32>().ok())
}
/// Follows the owned child's bus registration until cancelled. Reports
/// Active when the name owner appears, Starting once if registration outlasts
/// the grace or the name is later lost, and a terminal state on conflict.
fn watch_orca_name<B, R>(pid: u32, bus: &B, cancel: &AtomicBool, grace: Duration, mut report: R)
where
    B: Fn(&str, &str) -> Result<String, &'static str>,
    R: FnMut(State, &'static str) -> bool,
{
    let euid = rustix::process::geteuid().as_raw();
    let started = Instant::now();
    let mut reported: Option<State> = None;
    while !cancel.load(Ordering::Relaxed) {
        let (state, reason, interval) = match probe_orca_name(pid, euid, bus) {
            Probe::Active => (State::Active, "active", Duration::from_secs(1)),
            Probe::Failed(state, reason) => {
                report(state, reason);
                return;
            }
            Probe::Absent(reason) => {
                let lost = reported == Some(State::Active);
                if !lost && started.elapsed() < grace {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let reason = if lost {
                    "name-lost"
                } else {
                    slow_reason(reason)
                };
                (State::Starting, reason, Duration::from_millis(100))
            }
        };
        if reported != Some(state) {
            if !report(state, reason) {
                return;
            }
            reported = Some(state);
        }
        std::thread::sleep(interval);
    }
}
fn slow_reason(reason: &str) -> &'static str {
    match reason {
        "bus-spawn" => "slow-start-bus-spawn",
        "bus-timeout" => "slow-start-bus-timeout",
        "bus-oversize" => "slow-start-bus-oversize",
        "bus-malformed" => "slow-start-bus-malformed",
        "bus-read" => "slow-start-bus-read",
        "bus-wait-error" => "slow-start-bus-wait-error",
        "owner-vanished" => "slow-start-owner-vanished",
        _ => "slow-start",
    }
}
/// True while either name is still held by the stopped child `pid`, or by a
/// connection that is mid-disconnect. A live foreign owner does not delay the
/// restart; conflict detection handles it once the new child starts.
fn departing_owner<B>(pid: u32, bus: &B) -> bool
where
    B: Fn(&str, &str) -> Result<String, &'static str>,
{
    NAMES.iter().any(|name| {
        let Ok(reply) = bus("org.freedesktop.DBus.GetNameOwner", name) else {
            return false;
        };
        let Some(owner) = parse_owner(&reply) else {
            return false;
        };
        match bus("org.freedesktop.DBus.GetConnectionUnixProcessID", owner) {
            Ok(reply) => parse_uint(&reply) == Some(pid),
            Err("bus-nonzero") => true,
            Err(_) => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_bus_failures_use_actual_production_command_path() {
        for (script, expected) in [
            ("exit 9", "bus-nonzero"),
            ("printf '\\377'", "bus-malformed"),
            ("head -c 1025 /dev/zero", "bus-oversize"),
        ] {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", script])
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            assert_eq!(bus_call_command(command).unwrap_err(), expected);
        }
        assert_eq!(
            bus_call_command(Command::new("/nonexistent/tuna-orca-test")).unwrap_err(),
            "bus-spawn"
        );
    }
    #[test]
    fn finite_error_kinds_never_emit_error_content() {
        assert_eq!(
            error_kind(std::io::ErrorKind::PermissionDenied),
            "permission-denied"
        );
        assert_eq!(error_kind(std::io::ErrorKind::Other), "other");
    }
    #[test]
    fn child_has_parent_death_signal_before_exec() {
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-c", "import ctypes; p=ctypes.c_int(); assert ctypes.CDLL(None).prctl(2,ctypes.byref(p)) == 0; assert p.value == 15"]);
        parent_lifetime(&mut command);
        assert!(command.status().unwrap().success());
    }
    #[test]
    fn disable_reaps_only_owned_child() {
        let child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let mut other = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let mut reader = Reader::new("test-display");
        reader.enabled = true;
        reader.child = Some(child);
        reader.set_enabled(false);
        assert!(reader.pid().is_none());
        assert!(other.try_wait().unwrap().is_none());
        other.kill().unwrap();
        other.wait().unwrap();
    }

    /// Fake session bus: `owners` maps a well-known name to (unique name, pid).
    fn fake_bus(
        owners: Vec<(&'static str, &'static str, u32)>,
        vanished: &'static [&'static str],
    ) -> impl Fn(&str, &str) -> Result<String, &'static str> {
        let uid = rustix::process::geteuid().as_raw();
        move |method, arg| match method {
            "org.freedesktop.DBus.GetNameOwner" => owners
                .iter()
                .find(|(name, _, _)| *name == arg)
                .map(|(_, owner, _)| format!("('{owner}',)\n"))
                .ok_or("bus-nonzero"),
            _ if vanished.contains(&arg) => Err("bus-nonzero"),
            "org.freedesktop.DBus.GetConnectionUnixProcessID" => owners
                .iter()
                .find(|(_, owner, _)| *owner == arg)
                .map(|(_, _, pid)| format!("(uint32 {pid},)\n"))
                .ok_or("bus-nonzero"),
            "org.freedesktop.DBus.GetConnectionUnixUser" => Ok(format!("(uint32 {uid},)\n")),
            _ => Err("bus-nonzero"),
        }
    }
    #[test]
    fn owner_vanishing_mid_probe_is_not_a_failure() {
        let euid = rustix::process::geteuid().as_raw();
        let bus = fake_bus(vec![("org.gnome.Orca1.Service", ":1.9", 40)], &[":1.9"]);
        assert_eq!(
            probe_orca_name(41, euid, &bus),
            Probe::Absent("owner-vanished")
        );
        let bus = fake_bus(vec![("org.gnome.Orca1.Service", ":1.9", 41)], &[]);
        assert_eq!(probe_orca_name(41, euid, &bus), Probe::Active);
        let bus = fake_bus(vec![("org.gnome.Orca1.Service", ":1.9", 40)], &[]);
        assert_eq!(
            probe_orca_name(41, euid, &bus),
            Probe::Failed(State::Conflict, "credentials-conflict")
        );
        assert_eq!(
            probe_orca_name(41, euid, &fake_bus(vec![], &[])),
            Probe::Absent("bus-nonzero")
        );
    }
    #[test]
    fn slow_registration_is_reported_but_never_abandoned() {
        let appeared = Instant::now() + Duration::from_millis(400);
        let late = fake_bus(vec![("org.gnome.Orca1.Service", ":1.4", 41)], &[]);
        let bus = |method: &str, arg: &str| {
            if Instant::now() < appeared {
                Err("bus-nonzero")
            } else {
                late(method, arg)
            }
        };
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        watch_orca_name(
            41,
            &bus,
            &cancel,
            Duration::from_millis(100),
            |state, reason| {
                seen.push((state, reason));
                state != State::Active
            },
        );
        assert_eq!(
            seen,
            [(State::Starting, "slow-start"), (State::Active, "active")]
        );
    }
    #[test]
    fn cancelled_watch_reports_nothing() {
        let cancel = AtomicBool::new(true);
        let bus = fake_bus(vec![], &[]);
        watch_orca_name(41, &bus, &cancel, Duration::ZERO, |_, _| panic!("reported"));
    }
    #[test]
    fn restart_waits_only_for_the_stopped_childs_names() {
        let old = fake_bus(vec![("org.gnome.Orca1.Service", ":1.7", 41)], &[]);
        assert!(departing_owner(41, &old));
        let closing = fake_bus(vec![("org.gnome.Orca.Service", ":1.7", 41)], &[":1.7"]);
        assert!(departing_owner(41, &closing));
        let foreign = fake_bus(vec![("org.gnome.Orca1.Service", ":1.8", 77)], &[]);
        assert!(!departing_owner(41, &foreign));
        assert!(!departing_owner(41, &fake_bus(vec![], &[])));
    }
    #[test]
    fn no_new_child_while_predecessor_departs() {
        let (sender, receiver) = mpsc::channel();
        let mut reader = Reader::new("test-display");
        reader.enabled = true;
        reader.departing = Some(receiver);
        reader.poll(false);
        assert!(reader.pid().is_none());
        assert_eq!(reader.attempts, 0);
        sender.send(()).unwrap();
        assert!(!reader.departure_pending());
        assert!(reader.departing.is_none());
    }
}
