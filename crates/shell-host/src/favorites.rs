//! Pinned favorites persisted under `$XDG_DATA_HOME` (002 T2).
//!
//! The overview grid shows favorites first; pins are desktop-entry
//! ids in user order. Storage is one JSON string list, written
//! atomically (temp file plus rename) so a crash mid-save never
//! leaves a half-written file. Corrupt or missing files load as
//! empty — fail-closed, never a startup error.

use std::fs;
use std::path::{Path, PathBuf};

/// Favorites shown in the overview grid.
pub const MAX_FAVORITES: usize = 32;
/// Shell data dir name under `$XDG_DATA_HOME`.
pub const DATA_DIR_NAME: &str = "roost-shell";
/// Favorites file name.
pub const FAVORITES_FILE: &str = "favorites.json";

/// Ordered pinned app ids with a backing file.
#[derive(Debug)]
pub struct Favorites {
    ids: Vec<String>,
    path: PathBuf,
}

impl Favorites {
    /// Load from `path`; missing or unreadable files start empty.
    /// A corrupt file also starts empty (its ids are user data, not
    /// configuration the shell may crash over).
    pub fn load(path: PathBuf) -> Self {
        let ids = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| {
                value.as_array().map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .filter(|id| !id.is_empty())
                        .take(MAX_FAVORITES)
                        .collect()
                })
            })
            .unwrap_or_default();
        Self { ids, path }
    }

    /// Load from the host data dir, creating it when absent.
    pub fn system() -> Self {
        let dir = data_dir();
        let _ = fs::create_dir_all(&dir);
        Self::load(dir.join(FAVORITES_FILE))
    }

    /// Backing file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Pinned ids in grid order.
    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    /// Whether `app_id` is pinned.
    pub fn contains(&self, app_id: &str) -> bool {
        self.ids.iter().any(|id| id == app_id)
    }

    /// Pin `app_id` at the end. Returns `false` (and changes nothing)
    /// when already pinned or the grid is full.
    pub fn pin(&mut self, app_id: &str) -> bool {
        if app_id.is_empty() || self.contains(app_id) || self.ids.len() >= MAX_FAVORITES {
            return false;
        }
        self.ids.push(app_id.to_owned());
        true
    }

    /// Remove `app_id`. Returns whether it was pinned.
    pub fn unpin(&mut self, app_id: &str) -> bool {
        let before = self.ids.len();
        self.ids.retain(|id| id != app_id);
        self.ids.len() != before
    }

    /// Persist atomically: write plus rename, so readers never see a
    /// torn file. Creates parent dirs; surfaces I/O errors to the
    /// caller (the in-memory pins stay regardless).
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&self.ids)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// `$XDG_DATA_HOME/roost-shell` (default `~/.local/share/roost-shell`).
pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|base| base.join(DATA_DIR_NAME))
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".local/share").join(DATA_DIR_NAME))
                .unwrap_or_else(|| PathBuf::from("/tmp").join(DATA_DIR_NAME))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_favorites() -> (Favorites, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let favs = Favorites::load(dir.path().join(FAVORITES_FILE));
        assert!(favs.ids().is_empty());
        (favs, dir)
    }

    #[test]
    fn pin_unpin_orders_and_dedupes() {
        let (mut favs, _dir) = temp_favorites();
        assert!(favs.pin("a.desktop"));
        assert!(favs.pin("b.desktop"));
        assert!(!favs.pin("a.desktop"), "re-pin is a no-op false");
        assert!(!favs.pin(""), "empty id never pins");
        assert_eq!(favs.ids(), &["a.desktop", "b.desktop"]);
        assert!(favs.contains("b.desktop"));
        assert!(favs.unpin("a.desktop"));
        assert!(!favs.unpin("a.desktop"));
        assert_eq!(favs.ids(), &["b.desktop"]);
    }

    #[test]
    fn save_roundtrips_through_json() {
        let (mut favs, dir) = temp_favorites();
        favs.pin("a.desktop");
        favs.pin("b.desktop");
        favs.save().expect("save");
        let again = Favorites::load(dir.path().join(FAVORITES_FILE));
        assert_eq!(again.ids(), &["a.desktop", "b.desktop"]);
    }

    #[test]
    fn corrupt_file_loads_empty_without_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FAVORITES_FILE);
        fs::write(&path, "{ not json [").expect("write corrupt");
        let favs = Favorites::load(path);
        assert!(favs.ids().is_empty());
    }

    #[test]
    fn non_string_entries_are_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FAVORITES_FILE);
        fs::write(&path, r#"["a.desktop", 42, null, "b.desktop", ""]"#).expect("write");
        let favs = Favorites::load(path);
        assert_eq!(favs.ids(), &["a.desktop", "b.desktop"]);
    }
}
