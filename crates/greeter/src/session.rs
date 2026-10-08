//! Session enumeration: parse installed `.desktop` session files into
//! picker entries, with Roost preselected when present. Tolerant by
//! design — malformed entries are skipped with a count, never failing
//! the whole read. Live system paths stay behind [`SESSION_DIRS`]
//! so tests inject fixture directories.

use std::path::{Path, PathBuf};

/// System session directories, in lookup order.
pub const SESSION_DIRS: &[&str] = &["/usr/share/wayland-sessions", "/usr/share/xsessions"];

/// One launchable session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    /// Display name from `Name=`.
    pub name: String,
    /// Command from `Exec=`, split on whitespace.
    pub command: Vec<String>,
    /// Source file (for diagnostics, not identity).
    pub source: PathBuf,
    /// True when this is the Roost session.
    pub is_default: bool,
}

/// Parse result: entries plus skipped-file count.
#[derive(Debug, Default)]
pub struct Enumeration {
    pub entries: Vec<SessionEntry>,
    pub skipped: usize,
}

fn parse_entry(path: &Path, text: &str) -> Option<SessionEntry> {
    let mut in_entry = false;
    let mut name: Option<String> = None;
    let mut exec: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(v) = line.strip_prefix("Name=") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Exec=") {
            exec = Some(v.trim().to_string());
        }
    }
    let (name, exec) = (name?, exec?);
    if name.is_empty() || exec.is_empty() {
        return None;
    }
    let command: Vec<String> = exec.split_whitespace().map(str::to_string).collect();
    if command.is_empty() {
        return None;
    }
    let lowered = name.to_lowercase();
    let is_default = lowered.contains("roost")
        || lowered.contains("rust wayland")
        // Legacy working-title entry; keep matching old installs.
        || lowered.contains("rwd")
        || command.first().is_some_and(|c| {
            c.starts_with("roost-") || c.starts_with("rwd-")
        });
    Some(SessionEntry {
        name,
        command,
        source: path.to_path_buf(),
        is_default,
    })
}

/// True when the entry asks to stay out of session pickers
/// (`NoDisplay=true` or `Hidden=true` in `[Desktop Entry]`), as GDM's
/// picker does: compatibility entries such as the old session name's
/// (#505) still launch when remembered but are never listed twice.
fn hidden_entry(text: &str) -> bool {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if in_entry && matches!(line, "NoDisplay=true" | "Hidden=true") {
            return true;
        }
    }
    false
}

/// Enumerate `dirs`, returning entries sorted by name with the Roost
/// default first when present.
pub fn enumerate_dirs(dirs: &[&Path]) -> Enumeration {
    let mut out = Enumeration::default();
    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "desktop") {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(text) if hidden_entry(&text) => {}
                Ok(text) => match parse_entry(&path, &text) {
                    Some(entry) => out.entries.push(entry),
                    None => out.skipped += 1,
                },
                Err(_) => out.skipped += 1,
            }
        }
    }
    out.entries.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Enumerate the live system session directories.
pub fn enumerate_system() -> Enumeration {
    let dirs: Vec<&Path> = SESSION_DIRS.iter().map(Path::new).collect();
    enumerate_dirs(&dirs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            let mut f = std::fs::File::create(dir.path().join(name)).unwrap();
            f.write_all(body.as_bytes()).unwrap();
        }
        dir
    }

    const ROOST: &str = "[Desktop Entry]\nName=Tuna Desktop\nExec=roost-session\n";
    const LEGACY: &str = "[Desktop Entry]\nName=RWD\nExec=rwd-session\n";
    const SWAY: &str = "[Desktop Entry]\nName=Sway\nExec=sway\n";
    const BAD: &str = "[Desktop Entry]\nName=Broken\n";
    const NOT_DESKTOP: &str = "[Desktop Entry]\nName=X\nExec=x\n";

    #[test]
    fn lists_sessions_with_roost_default_first() {
        let dir = fixture_dir(&[("sway.desktop", SWAY), ("roost.desktop", ROOST)]);
        let out = enumerate_dirs(&[dir.path()]);
        assert_eq!(out.entries.len(), 2);
        assert_eq!(out.entries[0].name, "Tuna Desktop");
        assert!(out.entries[0].is_default);
        assert_eq!(out.entries[0].command, vec!["roost-session"]);
        assert_eq!(out.skipped, 0);
    }

    #[test]
    fn legacy_rwd_entry_still_detected_as_default() {
        let dir = fixture_dir(&[("sway.desktop", SWAY), ("rwd.desktop", LEGACY)]);
        let out = enumerate_dirs(&[dir.path()]);
        assert_eq!(out.entries.len(), 2);
        assert_eq!(out.entries[0].name, "RWD");
        assert!(out.entries[0].is_default);
        assert_eq!(out.entries[0].command, vec!["rwd-session"]);
    }

    #[test]
    fn hidden_entries_are_not_listed() {
        // The old session name's compatibility entry (#505).
        const COMPAT: &str = include_str!("../../../share/compat/wayland-sessions/roost.desktop");
        const GONE: &str = "[Desktop Entry]\nName=Gone\nExec=gone\nHidden=true\n";
        const OTHER_GROUP: &str =
            "[Desktop Entry]\nName=Kept\nExec=kept\n[Desktop Action x]\nNoDisplay=true\n";
        let dir = fixture_dir(&[
            ("sway.desktop", SWAY),
            ("compat.desktop", COMPAT),
            ("gone.desktop", GONE),
            ("kept.desktop", OTHER_GROUP),
        ]);
        let out = enumerate_dirs(&[dir.path()]);
        let names: Vec<&str> = out.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Kept", "Sway"]);
        assert_eq!(out.skipped, 0, "hidden is not malformed");
        // Remembered by a display manager, it still starts the session.
        let entry = parse_entry(Path::new("compat.desktop"), COMPAT).unwrap();
        assert_eq!(entry.command, vec!["tuna-session"]);
        assert!(entry.is_default);
    }

    #[test]
    fn malformed_entries_skipped_not_fatal() {
        let dir = fixture_dir(&[("ok.desktop", SWAY), ("bad.desktop", BAD)]);
        let out = enumerate_dirs(&[dir.path()]);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.skipped, 1);
    }

    #[test]
    fn non_desktop_files_and_missing_dirs_ignored() {
        let dir = fixture_dir(&[("note.txt", NOT_DESKTOP)]);
        let out = enumerate_dirs(&[dir.path(), Path::new("/nonexistent-xyz")]);
        assert!(out.entries.is_empty());
        assert_eq!(out.skipped, 0);
    }

    #[test]
    fn empty_name_or_exec_rejected() {
        assert!(parse_entry(Path::new("a.desktop"), "[Desktop Entry]\nName=\nExec=x\n").is_none());
        assert!(
            parse_entry(Path::new("a.desktop"), "[Desktop Entry]\nName=X\nExec=  \n").is_none()
        );
    }
}
