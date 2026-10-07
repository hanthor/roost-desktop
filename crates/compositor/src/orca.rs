//! Genuine distribution Orca, owned by the hardware compositor rather than a
//! global user-manager service. No --replace, global PID selection or bus-stop
//! request can affect another display's reader.
use roost_shell_control::ScreenReaderState as State;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub struct Reader {
    display: String,
    enabled: bool,
    child: Option<Child>,
    observed: Option<Receiver<(u32, State, &'static str)>>,
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
            let _ = child.kill();
            let until = Instant::now() + Duration::from_secs(1);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < until => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    _ => {
                        eprintln!("roost-compositor: owned Orca reap deadline exceeded");
                        break;
                    }
                }
            }
        }
    }
    pub fn poll(&mut self, diagnostics_visible: bool) -> Option<State> {
        let mut observed_running = false;
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if diagnostics_visible {
                        eprintln!("ROOST_ORCA_DIAGNOSTIC stage=child-exit pid={} attempt={} code={} signal={}", child.id(), self.attempts, status.code().unwrap_or(-1), status.signal().unwrap_or(0));
                    }
                    self.child = None;
                    self.observed = None;
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
                        eprintln!("ROOST_ORCA_DIAGNOSTIC stage=child-wait-error pid={} attempt={} kind={}", child.id(), self.attempts, error_kind(error.kind()));
                    }
                    self.stop();
                    self.state = State::Unavailable;
                    self.attempts = 3;
                }
                Ok(None) => observed_running = true,
            }
        }
        if let Some(receiver) = &self.observed {
            if let Ok((pid, state, reason)) = receiver.try_recv() {
                if self.pid() == Some(pid) {
                    if state == State::Active {
                        self.state = state;
                    } else {
                        if diagnostics_visible {
                            eprintln!(
                                "ROOST_ORCA_DIAGNOSTIC stage={} pid={} attempt={} observed_running={}",
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
                    self.child = Some(child);
                    self.state = State::Starting;
                    self.observed = Some(receiver);
                    std::thread::spawn(move || loop {
                        let (state, reason) = observe_orca_name(pid);
                        if sender.send((pid, state, reason)).is_err() || state != State::Active {
                            break;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    });
                }
                Err(error) => {
                    if diagnostics_visible {
                        eprintln!(
                            "ROOST_ORCA_DIAGNOSTIC stage=child-spawn pid=0 attempt={} kind={}",
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
fn observe_orca_name(pid: u32) -> (State, &'static str) {
    let mut last_reason = "observer-deadline";
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        let mut active = false;
        // Orca 50.3 and 51 expose different genuine remote-controller names.
        // A foreign owner of either contract is a conflict, never replacement.
        for name in ["org.gnome.Orca1.Service", "org.gnome.Orca.Service"] {
            let reply = match bus_call("org.freedesktop.DBus.GetNameOwner", name) {
                Ok(reply) => reply,
                Err(reason) => {
                    last_reason = reason;
                    continue;
                }
            };
            let Some(owner) = reply.split('\'').nth(1) else {
                return (State::Unavailable, "owner-malformed");
            };
            if !owner.starts_with(':')
                || !owner
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || b":.".contains(&byte))
            {
                return (State::Unavailable, "owner-malformed");
            }
            for (method, expected) in [
                ("org.freedesktop.DBus.GetConnectionUnixProcessID", pid),
                (
                    "org.freedesktop.DBus.GetConnectionUnixUser",
                    rustix::process::geteuid().as_raw(),
                ),
            ] {
                let reply = match bus_call(method, owner) {
                    Ok(reply) => reply,
                    Err(reason) => return (State::Unavailable, reason),
                };
                let actual = reply
                    .trim()
                    .strip_prefix("(uint32 ")
                    .and_then(|s| s.strip_suffix(",)"))
                    .and_then(|s| s.parse::<u32>().ok());
                match actual {
                    Some(actual) if actual == expected => {}
                    Some(_) => return (State::Conflict, "credentials-conflict"),
                    None => return (State::Unavailable, "credentials-malformed"),
                }
            }
            active = true;
        }
        if active {
            return (State::Active, "active");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    (
        State::Unavailable,
        match last_reason {
            "bus-spawn" => "deadline-bus-spawn",
            "bus-timeout" => "deadline-bus-timeout",
            "bus-nonzero" => "deadline-bus-nonzero",
            "bus-oversize" => "deadline-bus-oversize",
            "bus-malformed" => "deadline-bus-malformed",
            "bus-read" => "deadline-bus-read",
            "bus-wait-error" => "deadline-bus-wait-error",
            _ => "observer-deadline",
        },
    )
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
            bus_call_command(Command::new("/nonexistent/roost-orca-test")).unwrap_err(),
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
}
