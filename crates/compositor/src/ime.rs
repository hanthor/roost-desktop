//! The IBus bridge (`roost-ibus-bridge`): GNOME Shell is IBus's client
//! for text input, and Roost's bridge plays that part. It joins the
//! session as the input method (input-method-v2), sends the keys typed
//! into a focused text field to IBus, and hands IBus's preedit and
//! commits to the app; keys IBus does not take go back through a
//! virtual keyboard only this client may hold. The compositor starts it
//! on a private socket when IBus is installed and restarts it a few
//! times if it exits.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;

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
    xim_starts: u32,
    xim_next_ms: u64,
}

impl ImeBridge {
    /// The bridge to run for this session, if any: `ROOST_IBUS=0` turns
    /// it off; otherwise it runs when IBus is installed (`ibus-daemon`
    /// on `PATH`) and the bridge binary is found (`ROOST_IBUS_BRIDGE_BIN`,
    /// then next to this executable, then `PATH`).
    pub fn configured(wayland_display: &str) -> Option<Self> {
        if std::env::var_os("ROOST_IBUS").is_some_and(|v| v == "0") {
            return None;
        }
        let path = std::env::var_os("PATH")?;
        let on_path = |name: &str| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|p| p.is_file())
        };
        on_path("ibus-daemon")?;
        let bin = std::env::var_os("ROOST_IBUS_BRIDGE_BIN")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|d| d.join("roost-ibus-bridge")))
                    .filter(|p| p.is_file())
            })
            .or_else(|| on_path("roost-ibus-bridge"))?;
        Some(Self {
            bin,
            wayland_display: wayland_display.to_owned(),
            child: None,
            starts: 0,
            next_ms: 0,
            x11_display: None,
            xim: None,
            retired_xim: None,
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
                    eprintln!("roost-compositor: ime: bridge exited ({status})");
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
                eprintln!("roost-compositor: ime: IBus bridge started");
                self.child = Some(child);
            }
            Err(e) => {
                eprintln!("roost-compositor: ime: bridge failed to start: {e}");
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
        if now_ms < self.xim_next_ms || self.xim_starts > MAX_RESTARTS {
            return;
        }
        self.xim_next_ms = now_ms + RESTART_DELAY_MS;
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
        // Resolve the bridge's own IBus bus, never the host X display's bus.
        let address = Command::new("ibus")
            .arg("address")
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env_remove("DISPLAY")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty() && s != "(null)");
        let Some(address) = address else { return };
        self.xim_starts += 1;
        match Command::new(bin)
            .env("DISPLAY", format!(":{display}"))
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env("IBUS_ADDRESS", address)
            .env_remove("WAYLAND_SOCKET")
            .spawn()
        {
            Ok(child) => {
                self.xim = Some(child);
                eprintln!("roost-compositor: ime: XIM started on :{display}");
            }
            Err(error) => eprintln!("roost-compositor: ime: XIM failed: {error}"),
        }
    }
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
        .env("WAYLAND_DISPLAY", wayland_display);
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
