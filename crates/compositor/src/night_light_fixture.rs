//! CI-only failure injection. This module is absent from default/shipping builds.
//! A new immutable root-owned marker for this original process arms one failure
//! after the real scene's intermediate draw and before the optional final pass.
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
const CONTENT: &[u8] = b"final-pass-once\n";

pub(crate) struct Fault {
    pending: AtomicBool,
    accepted: AtomicBool,
    injected: AtomicU64,
}

fn valid_marker(uid: u32, mode: u32, links: u64, size: u64, regular: bool) -> bool {
    uid == 0 && mode & 0o7777 == 0o444 && links == 1 && size == CONTENT.len() as u64 && regular
}
fn read_marker(path: &PathBuf) -> bool {
    let Ok(mut file) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    else {
        return false;
    };
    let Ok(before) = file.metadata() else {
        return false;
    };
    if !valid_marker(
        before.uid(),
        before.mode(),
        before.nlink(),
        before.len(),
        before.is_file(),
    ) {
        return false;
    }
    let mut bytes = Vec::new();
    if (&mut file).take(32).read_to_end(&mut bytes).is_err() || bytes != CONTENT {
        return false;
    }
    let Ok(after) = file.metadata() else {
        return false;
    };
    let Ok(named) = path.symlink_metadata() else {
        return false;
    };
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.dev() == named.dev()
        && before.ino() == named.ino()
        && valid_marker(
            after.uid(),
            after.mode(),
            after.nlink(),
            after.len(),
            after.is_file(),
        )
        && valid_marker(
            named.uid(),
            named.mode(),
            named.nlink(),
            named.len(),
            named.is_file(),
        )
}
impl Fault {
    pub(crate) fn start() -> Arc<Self> {
        let fault = Arc::new(Self {
            pending: AtomicBool::new(false),
            accepted: AtomicBool::new(false),
            injected: AtomicU64::new(0),
        });
        let path = PathBuf::from(format!(
            "/run/roost-vm-night-light-fault-{}",
            std::process::id()
        ));
        // Refuse pre-existing or untrusted controls. The fixture must establish
        // its genuine warm baseline before creating this process-specific file.
        let directory = std::fs::metadata("/run").ok();
        if directory.is_none_or(|m| m.uid() != 0 || m.mode() & 0o022 != 0 || !m.is_dir())
            || path.symlink_metadata().is_ok()
        {
            return fault;
        }
        let weak = Arc::downgrade(&fault);
        let _ = std::thread::Builder::new()
            .name("roost-night-light-fixture".into())
            .spawn(move || loop {
                let Some(fault) = weak.upgrade() else {
                    break;
                };
                if read_marker(&path) {
                    fault.accepted.store(true, Ordering::Release);
                    fault.pending.store(true, Ordering::Release);
                    break;
                }
                drop(fault);
                std::thread::sleep(Duration::from_millis(100));
            });
        fault
    }
    pub(crate) fn consume(&self) -> bool {
        if self.pending.swap(false, Ordering::AcqRel) {
            self.injected.fetch_add(1, Ordering::AcqRel);
            true
        } else {
            false
        }
    }
    pub(crate) fn receipt(&self) -> (bool, u64) {
        (
            self.accepted.load(Ordering::Acquire),
            self.injected.load(Ordering::Acquire),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marker_requires_exact_root_readonly_single_regular_file() {
        assert!(valid_marker(0, 0o100444, 1, 16, true));
        for (uid, mode, links, size, regular) in [
            (1000, 0o100444, 1, 16, true),
            (0, 0o100644, 1, 16, true),
            (0, 0o104444, 1, 16, true),
            (0, 0o100444, 2, 16, true),
            (0, 0o100444, 1, 17, true),
            (0, 0o100444, 1, 16, false),
        ] {
            assert!(!valid_marker(uid, mode, links, size, regular));
        }
    }
    #[test]
    fn one_accepted_marker_cannot_reinject_on_later_frames() {
        let fault = Fault {
            pending: AtomicBool::new(true),
            accepted: AtomicBool::new(true),
            injected: AtomicU64::new(0),
        };
        assert!(fault.consume());
        for _ in 0..100 {
            assert!(!fault.consume());
        }
        assert_eq!(fault.receipt(), (true, 1));
    }
    #[test]
    fn marker_symlink_never_admits_a_control() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, CONTENT).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(target, &link).unwrap();
        assert!(!read_marker(&link));
    }
}
