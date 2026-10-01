//! Roost-owned prefs: shell knobs outside the shared desktop schema.
//!
//! Clock format, wallpaper, and icon theme live in the shared settings
//! the shell shares with GNOME apps ([`crate::settings`]); this file
//! is home to the small Roost-only remainder (bar clock extras the
//! shared schema never covered). Persistence mirrors the notification
//! queue: a versioned JSON file under the XDG state dir, written
//! atomically (temp file plus rename) on every change, so a restart
//! keeps the kept value without hand-edited files. Missing, corrupt,
//! or version-skewed files read as defaults — prefs are convenience,
//! never anything the shell may crash over.

use std::fs;
use std::path::{Path, PathBuf};

/// Schema version of the prefs file. A mismatch reads as defaults,
// like the notification queue: user data, never a crash.
const PREFS_VERSION: u64 = 1;

/// Prefs file name under the XDG state dir.
pub const PREFS_FILE: &str = "prefs.json";

/// Roost-only display knobs. Kept tiny on purpose: only options the
/// shared desktop schema does not cover belong here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoostPrefs {
    /// Prefix the bar clock with the two-letter weekday (`mo 12:34`).
    pub clock_show_weekday: bool,
}

impl Default for RoostPrefs {
    /// Plain clock, as before prefs existed.
    fn default() -> Self {
        Self {
            clock_show_weekday: false,
        }
    }
}

/// System prefs file path under the XDG state dir (same dir as the
/// notification queue).
pub fn system_path() -> Option<PathBuf> {
    crate::notifications::state_dir().map(|dir| dir.join(PREFS_FILE))
}

/// Load from the system prefs file, creating its dir when absent.
/// Missing, corrupt, or version-skewed files read as defaults.
pub fn load_system() -> RoostPrefs {
    let Some(path) = system_path() else {
        return RoostPrefs::default();
    };
    let _ = fs::create_dir_all(path.parent().expect("prefs file has a parent"));
    load(&path)
}

/// Load from `path`; missing or unreadable files read as defaults.
/// Unknown fields are ignored so a newer shell never breaks an
/// older file it can still honor.
pub fn load(path: &Path) -> RoostPrefs {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return RoostPrefs::default(),
    };
    match serde_json::from_str(&text) {
        Ok(doc) => decode(&doc),
        Err(_) => RoostPrefs::default(),
    }
}

/// Decode a prefs document; version skew reads as defaults.
fn decode(doc: &serde_json::Value) -> RoostPrefs {
    if doc.get("version").and_then(serde_json::Value::as_u64) != Some(PREFS_VERSION) {
        return RoostPrefs::default();
    }
    RoostPrefs {
        clock_show_weekday: doc
            .get("clock_show_weekday")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    }
}

/// Persist to `path` atomically: write plus rename, so readers never
/// see a torn file. Creates parent dirs; surfaces I/O errors to the
/// caller (the in-memory prefs stay regardless).
pub fn save(prefs: &RoostPrefs, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let doc = serde_json::json!({
        "version": PREFS_VERSION,
        "clock_show_weekday": prefs.clock_show_weekday,
    });
    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_hold_a_plain_clock() {
        assert!(!RoostPrefs::default().clock_show_weekday);
    }

    #[test]
    fn missing_file_reads_as_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(load(&dir.path().join(PREFS_FILE)), RoostPrefs::default());
    }

    #[test]
    fn corrupt_or_version_skewed_files_read_as_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(PREFS_FILE);
        fs::write(&path, "{not json").expect("write");
        assert_eq!(load(&path), RoostPrefs::default());
        fs::write(&path, r#"{"version": 999, "clock_show_weekday": true}"#).expect("write");
        assert_eq!(load(&path), RoostPrefs::default());
    }

    #[test]
    fn save_round_trips_and_writes_atomically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(PREFS_FILE);
        let prefs = RoostPrefs {
            clock_show_weekday: true,
        };
        save(&prefs, &path).expect("save");
        assert_eq!(load(&path), prefs);
        // The rename consumes the temp file: no torn-write residue.
        assert!(
            !dir.path().join("prefs.json.tmp").exists(),
            "rename must consume the temp file"
        );
        // Unknown future fields never break the current read.
        fs::write(
            &path,
            r#"{"version": 1, "clock_show_weekday": true, "future_knob": 7}"#,
        )
        .expect("write");
        assert_eq!(load(&path), prefs);
    }
}
