//! Read-only shell state snapshot for semantic journeys (#65).
//!
//! Proof harnesses used to assert "pixels changed". That cannot tell
//! "the overview opened" from "something repainted". When
//! `ROOST_INTROSPECT_FILE` names a path, the shell writes a small JSON
//! document there every time its state changes, so a journey can
//! assert *what* happened: overview open, result list, focused window,
//! active workspace, open popup, banner count, lock.
//!
//! Opt-in and development-only: unset (the default) writes nothing.
//! The file is written atomically (temp file plus rename) with
//! owner-only permissions. Window titles are never included, only
//! compositor ids and app ids, matching the log redaction rule in
//! `docs/nested-session.md`.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

/// Environment variable naming the snapshot path.
pub const ENV: &str = "ROOST_INTROSPECT_FILE";

/// Snapshot schema version; bump on any breaking field change.
pub const VERSION: u64 = 1;

/// Writes the snapshot when it changes.
#[derive(Debug, Default)]
pub struct IntrospectSink {
    path: Option<PathBuf>,
    last: String,
    /// Monotonic count of distinct snapshots written; journeys use it
    /// to wait for "a newer state than the one I saw".
    seq: u64,
}

impl IntrospectSink {
    /// Sink configured from [`ENV`]; inert when unset or not absolute.
    pub fn from_env() -> Self {
        Self::at(
            std::env::var_os(ENV)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
        )
    }

    /// Sink writing to `path` (`None`: inert).
    pub fn at(path: Option<PathBuf>) -> Self {
        Self {
            path,
            last: String::new(),
            seq: 0,
        }
    }

    /// Whether a path is configured.
    pub fn is_active(&self) -> bool {
        self.path.is_some()
    }

    /// Write `state` (plus `version` and `seq`) when it differs from the
    /// last write. I/O errors are swallowed: introspection must never
    /// break the shell it observes.
    pub fn publish(&mut self, mut state: serde_json::Value) {
        let Some(path) = &self.path else {
            return;
        };
        let body = state.to_string();
        if body == self.last {
            return;
        }
        self.last = body;
        self.seq += 1;
        if let Some(obj) = state.as_object_mut() {
            obj.insert("version".into(), VERSION.into());
            obj.insert("seq".into(), self.seq.into());
        }
        let tmp = path.with_extension("tmp");
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut file| file.write_all(state.to_string().as_bytes()));
        if written.is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inert_without_a_path() {
        let mut sink = IntrospectSink::at(None);
        assert!(!sink.is_active());
        sink.publish(json!({"overview": {"open": true}}));
    }

    #[test]
    fn writes_on_change_only_with_version_and_seq() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell.json");
        let mut sink = IntrospectSink::at(Some(path.clone()));
        let read = || -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
        };
        sink.publish(json!({"overview": {"open": false}}));
        assert_eq!(read()["seq"], 1);
        assert_eq!(read()["version"], VERSION);
        sink.publish(json!({"overview": {"open": false}}));
        assert_eq!(read()["seq"], 1, "unchanged state is not rewritten");
        sink.publish(json!({"overview": {"open": true}}));
        assert_eq!(read()["seq"], 2);
        assert_eq!(read()["overview"]["open"], true);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
