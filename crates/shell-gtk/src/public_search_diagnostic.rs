//! Opt-in CI observations of the original cached catalog used by show_results.
//! Same-user fixture provenance only; never an authentication boundary or a
//! replacement for physical focus, launch, presenter or audio evidence.
use roost_shell_host::apps::AppEntry;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;

type Key = (u64, u64, u32, u32, u64, i64, i64, i64, i64);
fn key(value: &fs::Metadata) -> Key {
    (
        value.dev(),
        value.ino(),
        value.uid(),
        value.mode(),
        value.size(),
        value.mtime(),
        value.mtime_nsec(),
        value.ctime(),
        value.ctime_nsec(),
    )
}
#[derive(Clone, PartialEq, Eq)]
struct Principal {
    pid: u32,
    uid: u32,
    ticks: u64,
    executable: Key,
}
fn principal() -> Option<Principal> {
    let pid = std::process::id();
    // SAFETY: getuid/geteuid have no arguments or ownership side effects.
    let (uid, euid) = unsafe { (libc::getuid(), libc::geteuid()) };
    if uid == 0 || uid != euid {
        return None;
    }
    let mapped = fs::metadata("/proc/self/exe").ok()?;
    let named = fs::symlink_metadata("/usr/bin/roost-shell-gtk").ok()?;
    if !named.is_file()
        || named.uid() != 0
        || named.mode() & 0o022 != 0
        || key(&mapped) != key(&named)
        || fs::read_link("/proc/self/exe").ok()?.as_os_str() != "/usr/bin/roost-shell-gtk"
    {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open("/proc/self/stat")
        .ok()?
        .take(4097)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 4096 {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let ticks = text
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()?;
    if ticks == 0
        || key(&fs::metadata("/proc/self/exe").ok()?) != key(&mapped)
        || key(&fs::symlink_metadata("/usr/bin/roost-shell-gtk").ok()?) != key(&named)
    {
        return None;
    }
    Some(Principal {
        pid,
        uid,
        ticks,
        executable: key(&mapped),
    })
}
fn expected(entry: &AppEntry) -> bool {
    matches!(
        entry.app_id.as_str(),
        "org.gnome.DiskUtility" | "org.gnome.DiskUtility.desktop"
    )
}
fn summary(apps: &[AppEntry], hits: &[&AppEntry]) -> Option<(usize, bool, usize)> {
    if apps.len() > 4096 || hits.len() > super::overview::MAX_RESULTS {
        return None;
    }
    let targets: Vec<_> = apps.iter().filter(|entry| expected(entry)).collect();
    Some((
        targets.len(),
        targets.len() == 1 && targets[0].name == "Disks",
        hits.iter().filter(|entry| expected(entry)).count(),
    ))
}
pub struct Capture {
    original: Principal,
    path: PathBuf,
    owned: Option<Key>,
    sequence: u64,
}
impl Capture {
    pub fn new() -> Option<Self> {
        if std::env::var_os("ROOST_VM_PUBLIC_SEARCH_DIAGNOSTICS").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        {
            return None;
        }
        let original = principal()?;
        let directory = PathBuf::from(format!("/run/user/{}", original.uid));
        let mode = fs::symlink_metadata(&directory).ok()?;
        if !mode.is_dir() || mode.uid() != original.uid || mode.mode() & 0o077 != 0 {
            return None;
        }
        Some(Self {
            original,
            path: directory.join("roost-vm-public-search.json"),
            owned: None,
            sequence: 0,
        })
    }
    pub fn withhold(&mut self) {
        if self.owned.take().is_some_and(|original| {
            fs::symlink_metadata(&self.path)
                .ok()
                .is_some_and(|value| key(&value) == original)
        }) {
            let _ = fs::remove_file(&self.path);
        }
    }
    pub fn sample(
        &mut self,
        query: &str,
        apps: &[AppEntry],
        hits: &[&AppEntry],
        visible: (bool, bool),
        unlocked: impl Fn() -> bool,
    ) {
        self.withhold();
        if query != "disks" || !unlocked() || principal().as_ref() != Some(&self.original) {
            return;
        }
        let Some((catalog_count, label_matches, ranked_count)) = summary(apps, hits) else {
            return;
        };
        let Some(sequence) = self.sequence.checked_add(1) else {
            return;
        };
        self.sequence = sequence;
        let mut clock = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock points to initialized writable timespec storage.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut clock) } != 0
            || clock.tv_sec < 0
            || !(0..1_000_000_000).contains(&clock.tv_nsec)
        {
            return;
        }
        let Some(sampled_ns) = (clock.tv_sec as u64)
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(clock.tv_nsec as u64))
        else {
            return;
        };
        let value = serde_json::json!({"schema":1,"pid":self.original.pid,"uid":self.original.uid,
            "start_ticks":self.original.ticks,"executed_key":self.original.executable,
            "sequence":sequence,"sampled_monotonic_ns":sampled_ns,"known_query_matches":true,
            "expected_catalog_count":catalog_count,"expected_label_matches":label_matches,
            "expected_ranked_count":ranked_count,"rendered_target_count":ranked_count,
            "search_window_visible":visible.0,"results_container_visible":visible.1,
            "controlled_source_provenance":true,"authentication_boundary":false});
        let Ok(bytes) = serde_json::to_vec(&value) else {
            return;
        };
        if bytes.len() > 4096 {
            return;
        }
        let temporary = self.path.with_file_name(format!(
            ".roost-vm-public-search-{}-{sequence}",
            self.original.pid
        ));
        let mut created = None;
        let result = (|| -> Option<Key> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&temporary)
                .ok()?;
            let initial = file.metadata().ok()?;
            created = Some((initial.dev(), initial.ino()));
            if !initial.is_file()
                || initial.uid() != self.original.uid
                || initial.mode() & 0o777 != 0o600
                || initial.nlink() != 1
            {
                return None;
            }
            file.write_all(&bytes).ok()?;
            file.sync_all().ok()?;
            if !unlocked() || principal().as_ref() != Some(&self.original) {
                return None;
            }
            let written = file.metadata().ok()?;
            if key(&written) != key(&fs::symlink_metadata(&temporary).ok()?) {
                return None;
            }
            fs::rename(&temporary, &self.path).ok()?;
            let published = file.metadata().ok()?;
            if key(&published) != key(&fs::symlink_metadata(&self.path).ok()?) {
                return None;
            }
            if !unlocked() || principal().as_ref() != Some(&self.original) {
                if fs::symlink_metadata(&self.path)
                    .ok()
                    .is_some_and(|current| key(&current) == key(&published))
                {
                    let _ = fs::remove_file(&self.path);
                }
                return None;
            }
            Some(key(&published))
        })();
        // Only our create_new temporary is used; a failed receipt is diagnostic
        // unavailability, never an input/launch error or a successful observation.
        if let Some(owned) = result {
            self.owned = Some(owned);
        }
        if created.is_some_and(|original| {
            fs::symlink_metadata(&temporary)
                .ok()
                .is_some_and(|value| (value.dev(), value.ino()) == original)
        }) {
            let _ = fs::remove_file(temporary);
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.withhold();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(id: &str, name: &str) -> AppEntry {
        AppEntry {
            app_id: id.into(),
            name: name.into(),
            generic_name: None,
            keywords: Vec::new(),
            argv: Vec::new(),
            icon: None,
            categories: Vec::new(),
        }
    }
    #[test]
    fn actual_cached_expected_identity_and_ranked_hits_are_distinct() {
        let target = entry("org.gnome.DiskUtility", "Disks");
        let other = entry("private", "Disks");
        assert_eq!(
            summary(&[target.clone(), other.clone()], &[&other]),
            Some((1, true, 0))
        );
        assert_eq!(
            summary(std::slice::from_ref(&target), &[&target]),
            Some((1, true, 1))
        );
        assert_eq!(
            summary(&[target.clone(), target.clone()], &[&target]),
            Some((2, false, 1))
        );
        assert_eq!(
            summary(&[entry("org.gnome.DiskUtility", "private")], &[]),
            Some((1, false, 0))
        );
    }
    fn capture(path: PathBuf) -> Capture {
        let original = Principal {
            pid: 1,
            uid: 1,
            ticks: 1,
            executable: (0, 0, 0, 0, 0, 0, 0, 0, 0),
        };
        Capture {
            original,
            owned: Some(key(&fs::symlink_metadata(&path).unwrap())),
            path,
            sequence: 0,
        }
    }
    #[test]
    fn unknown_query_and_locked_state_withhold_original_receipt_without_catalog_publish() {
        for query in ["private", "disks"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("receipt");
            fs::write(&path, b"original").unwrap();
            let mut capture = capture(path.clone());
            capture.sample(query, &[], &[], (false, false), || false);
            assert!(!path.exists());
            assert_eq!(capture.sequence, 0);
        }
    }
    #[test]
    fn substituted_diagnostic_path_is_never_removed_as_original() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipt");
        fs::write(&path, b"original").unwrap();
        let mut capture = capture(path.clone());
        fs::rename(&path, directory.path().join("old")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        capture.withhold();
        assert_eq!(fs::read(path).unwrap(), b"replacement");
    }
    #[test]
    fn oversized_actual_catalog_and_results_are_unavailable() {
        let app = entry("org.gnome.DiskUtility", "Disks");
        assert!(summary(&vec![app.clone(); 4097], &[]).is_none());
        assert!(summary(std::slice::from_ref(&app), &[&app; 7]).is_none());
    }
}
