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
        })
    }

    /// Start, or restart after an exit (bounded), never blocking.
    pub fn poll(&mut self, now_ms: u64, dh: &mut DisplayHandle) {
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
}

impl Drop for ImeBridge {
    fn drop(&mut self) {
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
