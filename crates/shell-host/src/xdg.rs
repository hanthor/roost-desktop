//! Fail-closed XDG base-directory resolution (#49).
//!
//! Every resolver returns `None` instead of falling back to a shared
//! directory. A missing `HOME` must never relocate app discovery,
//! favorites, notification history, prefs, or extension scripts under
//! world-writable `/tmp`, and a missing `XDG_RUNTIME_DIR` must never
//! resolve to another user's runtime dir. Callers treat `None` as "no
//! directory": nothing is read from or written to disk for that
//! concern, and the shell keeps running with in-memory state.
//!
//! The pure `*_from` variants take an environment lookup so tests can
//! exercise every branch without mutating the process environment.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_absolute())
}

/// `$XDG_DATA_HOME`, else `$HOME/.local/share`, else `None`.
pub fn data_home_from(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    absolute(env("XDG_DATA_HOME"))
        .or_else(|| absolute(env("HOME")).map(|home| home.join(".local/share")))
}

/// `$XDG_STATE_HOME`, else `$HOME/.local/state`, else `None`.
pub fn state_home_from(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    absolute(env("XDG_STATE_HOME"))
        .or_else(|| absolute(env("HOME")).map(|home| home.join(".local/state")))
}

/// `$XDG_RUNTIME_DIR` when absolute and private to this user, else
/// `None`. Never `/run/user/0` or `/tmp`.
pub fn runtime_dir_from(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    absolute(env("XDG_RUNTIME_DIR")).filter(|dir| is_private_dir(dir))
}

/// Data home from the live process environment.
pub fn data_home() -> Option<PathBuf> {
    data_home_from(|key| std::env::var_os(key))
}

/// State home from the live process environment.
pub fn state_home() -> Option<PathBuf> {
    state_home_from(|key| std::env::var_os(key))
}

/// Runtime dir from the live process environment.
pub fn runtime_dir() -> Option<PathBuf> {
    runtime_dir_from(|key| std::env::var_os(key))
}

/// A directory owned by the effective user with no group or other
/// permission bits: the XDG spec's requirement for the runtime dir.
pub fn is_private_dir(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(dir) {
        Ok(meta) => {
            meta.is_dir()
                && meta.uid() == rustix::process::geteuid().as_raw()
                && meta.mode() & 0o077 == 0
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn data_home_prefers_xdg_then_home() {
        assert_eq!(
            data_home_from(env(&[("XDG_DATA_HOME", "/x/data"), ("HOME", "/h")])),
            Some(PathBuf::from("/x/data"))
        );
        assert_eq!(
            data_home_from(env(&[("HOME", "/h")])),
            Some(PathBuf::from("/h/.local/share"))
        );
    }

    #[test]
    fn data_home_without_home_is_none_never_tmp() {
        assert_eq!(data_home_from(env(&[])), None);
        assert_eq!(
            data_home_from(env(&[
                ("XDG_DATA_HOME", "relative"),
                ("HOME", "also-relative")
            ])),
            None
        );
    }

    #[test]
    fn state_home_without_home_is_none_never_tmp() {
        assert_eq!(state_home_from(env(&[])), None);
        assert_eq!(
            state_home_from(env(&[("HOME", "/h")])),
            Some(PathBuf::from("/h/.local/state"))
        );
    }

    #[test]
    fn runtime_dir_unset_is_none_never_root_runtime() {
        assert_eq!(runtime_dir_from(env(&[])), None);
    }

    #[test]
    fn runtime_dir_rejects_world_writable_and_accepts_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_owned();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(runtime_dir_from(env(&[("XDG_RUNTIME_DIR", &path)])), None);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            runtime_dir_from(env(&[("XDG_RUNTIME_DIR", &path)])),
            Some(dir.path().to_owned())
        );
    }

    #[test]
    fn runtime_dir_rejects_shared_tmp() {
        assert_eq!(runtime_dir_from(env(&[("XDG_RUNTIME_DIR", "/tmp")])), None);
    }
}
