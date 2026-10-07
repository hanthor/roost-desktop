//! Off-frame connector/EDID discovery only; no KMS ioctl, GBM or renderer access.
//! A process-wide slot also bounds cancelled workers across backend replacement.
use crate::monitor_edid::Observation;
use crate::monitor_refresh::Ticket;
use roost_shell_control::NativeOutputInfo;
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

pub const MAX_CONNECTORS: usize = 64;
static WORKER_OWNED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connector {
    pub name: String,
    pub id: u32,
}

pub struct Request {
    pub ticket: Ticket,
    /// Duplicate of the original parent char FD, used only by fstat.
    pub original_fd: OwnedFd,
    /// Actual compositor-thread KMS resource snapshot, never caller names.
    pub connectors: Vec<Connector>,
}

#[derive(Debug)]
pub struct Metadata {
    pub connector: Connector,
    pub owner: Option<NativeOutputInfo>,
    pub edid: Option<Observation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Busy,
    InvalidRequest,
    OriginalDevice,
    ConnectorChanged,
    Cancelled,
    Spawn,
    WorkerExited,
}

pub struct Completed {
    pub ticket: Ticket,
    pub result: Result<Vec<Metadata>, Failure>,
}

pub struct Worker {
    ticket: Ticket,
    cancel: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<Vec<Metadata>, Failure>>,
    handle: JoinHandle<()>,
}

fn validate(connectors: &[Connector]) -> Result<(), Failure> {
    if connectors.len() > MAX_CONNECTORS {
        return Err(Failure::InvalidRequest);
    }
    for (index, connector) in connectors.iter().enumerate() {
        if connector.id == 0
            || connector.name.is_empty()
            || connector.name.len() > 128
            || !connector
                .name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || connectors[..index]
                .iter()
                .any(|other| other.id == connector.id || other.name == connector.name)
        {
            return Err(Failure::InvalidRequest);
        }
    }
    Ok(())
}

fn acquire(request: Request, cancelled: &AtomicBool, sys: &Path) -> Result<Vec<Metadata>, Failure> {
    validate(&request.connectors)?;
    let original = || {
        if cancelled.load(Ordering::Acquire) {
            return Err(Failure::Cancelled);
        }
        if crate::native_output::device(request.original_fd.as_fd()).ok()
            != Some(request.ticket.device)
        {
            return Err(Failure::OriginalDevice);
        }
        Ok(())
    };
    original()?;
    let mut metadata = Vec::with_capacity(request.connectors.len());
    for connector in &request.connectors {
        original()?;
        let mut owner = crate::native_output::resolve(
            sys,
            request.ticket.device,
            &connector.name,
            connector.id,
        )
        .ok();
        let mut edid = owner.as_ref().and_then(|owner| {
            crate::monitor_edid::read_owned(request.original_fd.as_fd(), owner)
                .ok()
                .flatten()
        });
        original()?;
        if owner.as_ref().is_some_and(|owner| {
            crate::native_output::current(request.original_fd.as_fd(), owner).is_err()
        }) {
            owner = None;
            edid = None;
        }
        metadata.push(Metadata {
            connector: connector.clone(),
            owner,
            edid,
        });
    }
    // Earlier owners may change while later connectors are read. Validate the
    // entire accepted original binding set after the last observation; never
    // publish a partly revalidated batch as one coherent discovery generation.
    validate_bindings(request.original_fd.as_fd(), &metadata, cancelled)?;
    original()?;
    Ok(metadata)
}

fn validate_bindings(
    fd: std::os::fd::BorrowedFd<'_>,
    metadata: &[Metadata],
    cancelled: &AtomicBool,
) -> Result<(), Failure> {
    for item in metadata {
        if cancelled.load(Ordering::Acquire) {
            return Err(Failure::Cancelled);
        }
        if item
            .owner
            .as_ref()
            .is_some_and(|owner| crate::native_output::current(fd, owner).is_err())
        {
            return Err(Failure::ConnectorChanged);
        }
    }
    Ok(())
}

impl Worker {
    /// Cheap scheduling gate; start still claims the slot atomically.
    pub fn available() -> bool {
        !WORKER_OWNED.load(Ordering::Acquire)
    }

    pub fn start(request: Request) -> Result<Self, Failure> {
        validate(&request.connectors)?;
        if WORKER_OWNED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Failure::Busy);
        }
        let ticket = request.ticket;
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let (send, result) = mpsc::sync_channel(1);
        let handle = match std::thread::Builder::new()
            .name("roost-monitor-discovery".into())
            .spawn(move || {
                struct Slot;
                impl Drop for Slot {
                    fn drop(&mut self) {
                        WORKER_OWNED.store(false, Ordering::Release);
                    }
                }
                let _slot = Slot;
                let _ = send.send(acquire(request, &cancelled, Path::new("/sys")));
            }) {
            Ok(handle) => handle,
            Err(_) => {
                WORKER_OWNED.store(false, Ordering::Release);
                return Err(Failure::Spawn);
            }
        };
        Ok(Self {
            ticket,
            cancel,
            result,
            handle,
        })
    }

    /// Caller polls on the event loop; only atomics/channel state, no file I/O.
    /// The actual thread must have exited before its owned slot is retired.
    pub fn ready(&self) -> bool {
        self.handle.is_finished()
    }

    pub fn take(self) -> Result<Completed, Self> {
        if !self.ready() {
            return Err(self);
        }
        let result = self.result.try_recv().unwrap_or(Err(Failure::WorkerExited));
        Ok(Completed {
            ticket: self.ticket,
            result,
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        // Never block the compositor joining a kernel file read. The original
        // duplicate FD belongs to the worker until return; process-wide slot
        // remains occupied even across backend replacement. Deadline does not
        // cancel a syscall or authorize a second orphan worker.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    #[test]
    fn request_bound_and_duplicate_aliases_refuse_before_discovery() {
        let connector = Connector {
            name: "eDP-1".into(),
            id: 1,
        };
        assert!(validate(&[connector.clone()]).is_ok());
        assert_eq!(
            validate(&vec![connector.clone(); MAX_CONNECTORS + 1]),
            Err(Failure::InvalidRequest)
        );
        assert_eq!(
            validate(&[connector.clone(), connector]),
            Err(Failure::InvalidRequest)
        );
        for name in ["", "../eDP-1", "card0/eDP-1", "eDP-1\n"] {
            assert_eq!(
                validate(&[Connector {
                    name: name.into(),
                    id: 1
                }]),
                Err(Failure::InvalidRequest)
            );
        }
    }
    #[test]
    fn whole_batch_revalidation_refuses_early_owner_changed_after_later_read() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let node = tmp.path().join("devices/card0");
        std::fs::create_dir_all(&node).unwrap();
        std::fs::write(node.join("dev"), "1:3").unwrap();
        let dev = tmp.path().join("dev/char");
        std::fs::create_dir_all(&dev).unwrap();
        symlink(&node, dev.join("1:3")).unwrap();
        let mut metadata = Vec::new();
        for (name, id) in [("eDP-1", 1), ("DP-2", 2)] {
            let path = node.join(format!("card0-{name}"));
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("connector_id"), id.to_string()).unwrap();
            std::fs::write(path.join("status"), "connected").unwrap();
            let owner =
                crate::native_output::resolve(tmp.path(), libc::makedev(1, 3), name, id).unwrap();
            metadata.push(Metadata {
                connector: Connector {
                    name: name.into(),
                    id,
                },
                owner: Some(owner),
                edid: None,
            });
        }
        let fd = std::fs::File::open("/dev/null").unwrap();
        let cancelled = AtomicBool::new(false);
        assert_eq!(validate_bindings(fd.as_fd(), &metadata, &cancelled), Ok(()));
        let early = metadata[0].owner.as_ref().unwrap();
        std::fs::write(
            Path::new(&early.connector_sysfs).join("status"),
            "disconnected",
        )
        .unwrap();
        assert_eq!(
            validate_bindings(fd.as_fd(), &metadata, &cancelled),
            Err(Failure::ConnectorChanged)
        );
    }

    #[test]
    fn original_device_and_cancellation_checked_before_sysfs() {
        let make = || Request {
            ticket: Ticket {
                device: libc::makedev(226, 0),
                epoch: 1,
                started: Instant::now(),
            },
            original_fd: std::fs::File::open("/dev/null").unwrap().into(),
            connectors: vec![Connector {
                name: "eDP-1".into(),
                id: 1,
            }],
        };
        assert!(matches!(
            acquire(make(), &AtomicBool::new(false), Path::new("/nonexistent")),
            Err(Failure::OriginalDevice)
        ));
        assert!(matches!(
            acquire(make(), &AtomicBool::new(true), Path::new("/nonexistent")),
            Err(Failure::Cancelled)
        ));
    }
}
