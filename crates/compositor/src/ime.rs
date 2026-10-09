//! The IBus bridge (`tuna-ibus-bridge`): GNOME Shell is IBus's client
//! for text input, and Tuna Desktop's bridge plays that part. It joins the
//! session as the input method (input-method-v2), sends the keys typed
//! into a focused text field to IBus, and hands IBus's preedit and
//! commits to the app; keys IBus does not take go back through a
//! virtual keyboard only this client may hold. The compositor starts it
//! on a private socket when IBus is installed and restarts it a few
//! times if it exits.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use smithay::reexports::wayland_server::DisplayHandle;

/// Restarts after the first start, as for the shell.
const MAX_RESTARTS: u32 = 3;
/// Wait before a restart.
const RESTART_DELAY_MS: u64 = 2_000;

/// The supervised bridge process.
pub struct ImeBridge {
    bin: PathBuf,
    wayland_display: String,
    child: Option<Child>,
    starts: u32,
    next_ms: u64,
    x11_display: Option<u32>,
    xim: Option<Child>,
    retired_xim: Option<Child>,
    xim_address_probe: Option<mpsc::Receiver<Option<String>>>,
    xim_starts: u32,
    xim_next_ms: u64,
}

impl ImeBridge {
    /// The bridge to run for this session, if any: `TUNA_IBUS=0` turns
    /// it off; otherwise it runs when IBus is installed (`ibus-daemon`
    /// on `PATH`) and the bridge binary is found (`TUNA_IBUS_BRIDGE_BIN`,
    /// then next to this executable, then `PATH`).
    pub fn configured(wayland_display: &str) -> Option<Self> {
        if std::env::var_os("TUNA_IBUS").is_some_and(|v| v == "0") {
            return None;
        }
        let path = std::env::var_os("PATH")?;
        let on_path = |name: &str| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|p| p.is_file())
        };
        on_path("ibus-daemon")?;
        let bin = std::env::var_os("TUNA_IBUS_BRIDGE_BIN")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|d| d.join("tuna-ibus-bridge")))
                    .filter(|p| p.is_file())
            })
            .or_else(|| on_path("tuna-ibus-bridge"))?;
        Some(Self {
            bin,
            wayland_display: wayland_display.to_owned(),
            child: None,
            starts: 0,
            next_ms: 0,
            x11_display: None,
            xim: None,
            retired_xim: None,
            xim_address_probe: None,
            xim_starts: 0,
            xim_next_ms: 0,
        })
    }

    /// Start, or restart after an exit (bounded), never blocking.
    pub fn poll(&mut self, now_ms: u64, dh: &mut DisplayHandle) {
        self.poll_xim(now_ms);
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(None) => return,
                Ok(Some(status)) => {
                    eprintln!("tuna-compositor: ime: bridge exited ({status})");
                    self.child = None;
                    self.next_ms = now_ms + RESTART_DELAY_MS;
                }
                Err(_) => return,
            }
        }
        if self.starts > MAX_RESTARTS || now_ms < self.next_ms {
            return;
        }
        self.starts += 1;
        match spawn(&self.bin, &self.wayland_display, dh) {
            Ok(child) => {
                eprintln!("tuna-compositor: ime: IBus bridge started");
                self.child = Some(child);
            }
            Err(e) => {
                eprintln!("tuna-compositor: ime: bridge failed to start: {e}");
                self.next_ms = now_ms + RESTART_DELAY_MS;
            }
        }
    }

    /// The XIM helper may connect only after the compositor's XWM is ready.
    /// Advertising reserved sockets alone must not activate XWayland.
    pub fn set_ready_x11_display(&mut self, display: Option<u32>) {
        // try_wait never waits for an external process. This runs even while
        // locked, when normal IBus polling is suspended.
        if self
            .retired_xim
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))))
        {
            self.retired_xim = None;
        }
        if self.x11_display == display {
            return;
        }
        if let Some(mut child) = self.xim.take() {
            let _ = child.kill();
            self.retired_xim = Some(child);
        }
        self.x11_display = display;
        self.xim_starts = 0;
        self.xim_next_ms = 0;
    }

    fn poll_xim(&mut self, now_ms: u64) {
        // Keep at most one live helper and one killed child awaiting reaping.
        if self.retired_xim.is_some() {
            return;
        }
        let Some(display) = self.x11_display else {
            return;
        };
        if let Some(child) = &mut self.xim {
            if matches!(child.try_wait(), Ok(None)) {
                return;
            }
            self.xim = None;
            self.xim_next_ms = now_ms + RESTART_DELAY_MS;
        }
        if self.xim_address_probe.is_none()
            && (now_ms < self.xim_next_ms || self.xim_starts > MAX_RESTARTS)
        {
            return;
        }
        let bin = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|path| {
                std::env::split_paths(&path)
                    .map(|p| p.join("ibus-x11"))
                    .collect::<Vec<_>>()
            })
            .chain([
                PathBuf::from("/usr/libexec/ibus-x11"),
                PathBuf::from("/usr/lib/ibus/ibus-x11"),
                PathBuf::from("/usr/lib64/ibus/ibus-x11"),
            ])
            .find(|path| path.is_file());
        let Some(bin) = bin else { return };
        // A single worker owns the private address lookup and its pipe. The
        // compositor tick only receives a bounded result without waiting.
        let Some(probe) = &self.xim_address_probe else {
            let mut command = Command::new("ibus");
            command
                .arg("address")
                .env("WAYLAND_DISPLAY", &self.wayland_display)
                .env_remove("DISPLAY")
                .env_remove("IBUS_ADDRESS")
                .env_remove("WAYLAND_SOCKET");
            let (send, receive) = mpsc::sync_channel(1);
            self.xim_starts += 1;
            self.xim_next_ms = now_ms + RESTART_DELAY_MS;
            if std::thread::Builder::new()
                .name("tuna-xim-address".into())
                .stack_size(256 * 1024)
                .spawn(move || {
                    let _ = send.send(bounded_address_probe(command));
                })
                .is_ok()
            {
                self.xim_address_probe = Some(receive);
            }
            return;
        };
        let address = match probe.try_recv() {
            Ok(address) => address,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.xim_address_probe = None;
        let Some(address) = address else { return };
        match Command::new(bin)
            .env("DISPLAY", format!(":{display}"))
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env("IBUS_ADDRESS", address)
            .env_remove("WAYLAND_SOCKET")
            .spawn()
        {
            Ok(child) => {
                self.xim = Some(child);
                eprintln!("tuna-compositor: ime: XIM started on :{display}");
            }
            Err(error) => eprintln!("tuna-compositor: ime: XIM failed: {error}"),
        }
    }
}

/// Runs on the one address worker, never the compositor thread. A deadline and
/// fixed output cap bound the probe; retaining its receiver until completion
/// also bounds workers if an external child cannot finish being reaped.
fn bounded_address_probe(mut command: Command) -> Option<String> {
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(target_os = "linux")]
    {
        let owner = rustix::process::getpid();
        // SAFETY: these are only async-signal-safe prctl/getppid syscalls and
        // raw OS error construction between fork and exec.
        unsafe {
            command.pre_exec(move || {
                rustix::process::set_parent_process_death_signal(Some(
                    rustix::process::Signal::KILL,
                ))?;
                // The owner may have exited before prctl installed the signal.
                if rustix::process::getppid() != Some(owner) {
                    return Err(std::io::Error::from_raw_os_error(
                        rustix::io::Errno::CHILD.raw_os_error(),
                    ));
                }
                Ok(())
            });
        }
    }
    let mut child = command.spawn().ok()?;
    let result = (|| {
        let mut output = child.stdout.take()?;
        let flags = rustix::fs::fcntl_getfl(&output).ok()?;
        rustix::fs::fcntl_setfl(&output, flags | rustix::fs::OFlags::NONBLOCK).ok()?;
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut bytes = Vec::with_capacity(4096);
        let mut buffer = [0u8; 1024];
        loop {
            if Instant::now() >= deadline {
                return None;
            }
            match output.read(&mut buffer) {
                Ok(0) => {
                    if let Some(status) = child.try_wait().ok()? {
                        if !status.success() {
                            return None;
                        }
                        return String::from_utf8(bytes)
                            .ok()
                            .map(|s| s.trim().to_owned())
                            .filter(|s| !s.is_empty() && s != "(null)");
                    }
                }
                Ok(n) => {
                    if bytes.len() + n > 4096 {
                        return None;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return None,
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })();
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
        // This wait is confined to the worker. The tick cannot create another
        // worker until its result arrives, even if reaping itself stalls.
        let _ = child.wait();
    }
    result
}

impl Drop for ImeBridge {
    fn drop(&mut self) {
        if let Some(child) = &mut self.retired_xim {
            let _ = child.kill();
            let _ = child.try_wait();
        }
        if let Some(child) = &mut self.xim {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Spawn the bridge with its end of a fresh socket pair as
/// `WAYLAND_SOCKET`; the compositor's end becomes the trusted client.
fn spawn(bin: &Path, wayland_display: &str, dh: &mut DisplayHandle) -> std::io::Result<Child> {
    let (ours, theirs) = UnixStream::pair()?;
    let fd = theirs.as_raw_fd();
    let mut command = Command::new(bin);
    command
        .env("WAYLAND_SOCKET", fd.to_string())
        // IBus names its bus after the display.
        .env("WAYLAND_DISPLAY", wayland_display)
        .env_remove("DISPLAY")
        .env_remove("IBUS_ADDRESS");
    // SAFETY: one async-signal-safe fcntl between fork and exec, so the
    // socket survives exec (clears FD_CLOEXEC).
    unsafe {
        command.pre_exec(move || {
            let fd = std::os::fd::BorrowedFd::borrow_raw(fd);
            rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::empty()).map_err(std::io::Error::from)
        });
    }
    let child = command.spawn()?;
    drop(theirs);
    dh.insert_client(ours, Arc::new(crate::ClientState::ime_bridge()))
        .map_err(std::io::Error::other)?;
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_probe_reads_a_bounded_result_and_rejects_oversized_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf ' unix:path=/tmp/ibus\\n'"]);
        assert_eq!(
            bounded_address_probe(command).as_deref(),
            Some("unix:path=/tmp/ibus")
        );
        let mut command = Command::new("sh");
        command.args(["-c", "printf '%05000d' 0"]);
        assert!(bounded_address_probe(command).is_none());
    }

    #[test]
    fn address_probe_kills_a_process_that_does_not_finish() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        let started = Instant::now();
        assert!(bounded_address_probe(command).is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn address_probe_owner_fixture() {
        let Some(path) = std::env::var_os("TUNA_XIM_PROBE_OWNER_FIXTURE") else {
            return;
        };
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                r#"printf '%s' "$$" > "$TUNA_XIM_PROBE_PID_FILE"; exec sleep 30"#,
            ])
            .env("TUNA_XIM_PROBE_PID_FILE", path);
        let _ = bounded_address_probe(command);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn owner_exit_terminates_an_in_flight_address_probe() {
        let path = std::env::temp_dir().join(format!(
            "tuna-xim-owner-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut owner = Command::new(std::env::current_exe().unwrap())
            .args(["address_probe_owner_fixture", "--nocapture"])
            .env("TUNA_XIM_PROBE_OWNER_FIXTURE", &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let pid = loop {
            if let Ok(pid) = std::fs::read_to_string(&path) {
                if let Ok(pid) = pid.parse::<u32>() {
                    break Some(pid);
                }
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let owner_was_running = owner.try_wait().unwrap().is_none();
        let _ = owner.kill();
        let _ = owner.wait();
        let _ = std::fs::remove_file(path);
        let pid = pid.expect("address probe never started in its isolated owner");
        assert!(
            owner_was_running,
            "owner exited before the abrupt-exit fixture"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            // A dead orphan may await init's reaping; it must never keep running.
            let running = stat.is_ok_and(|s| {
                !s.rsplit_once(") ")
                    .is_some_and(|(_, fields)| fields.starts_with('Z'))
            });
            if !running {
                break;
            }
            if Instant::now() >= deadline {
                if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                }
                panic!("probe outlived its owner");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn losing_xwm_reaps_xim_and_resets_its_restart_budget() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let mut bridge = ImeBridge {
            bin: PathBuf::new(),
            wayland_display: "test".into(),
            child: None,
            starts: 1,
            next_ms: 123,
            x11_display: Some(5),
            xim: Some(child),
            retired_xim: None,
            xim_address_probe: None,
            xim_starts: MAX_RESTARTS + 1,
            xim_next_ms: 456,
        };
        bridge.set_ready_x11_display(Some(5));
        assert_eq!(bridge.xim.as_ref().unwrap().id(), pid);
        bridge.set_ready_x11_display(None);
        assert!(bridge.xim.is_none());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while bridge.retired_xim.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "killed XIM not reaped"
            );
            bridge.set_ready_x11_display(None);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
        assert_eq!((bridge.starts, bridge.next_ms), (1, 123));
        bridge.set_ready_x11_display(Some(5));
        assert_eq!((bridge.xim_starts, bridge.xim_next_ms), (0, 0));
    }
}
