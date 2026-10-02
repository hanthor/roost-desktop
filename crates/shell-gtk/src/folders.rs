//! GNOME's app-grid folders (`org.gnome.desktop.app-folders`).
//!
//! GNOME keeps its app-grid folders in GSettings: `folder-children` lists
//! folder ids, and each folder (a relocatable schema at
//! `/org/gnome/desktop/app-folders/folders/<id>/`) names its apps
//! explicitly, by desktop category, or both, minus excluded apps. A name
//! with `translate` set is a `.directory` file whose Name is shown.
//! Reading the same keys makes Roost's grid match the user's GNOME grid.

use std::path::PathBuf;

use gio::prelude::*;
use roost_shell_host::apps::AppEntry;

const SCHEMA: &str = "org.gnome.desktop.app-folders";
const FOLDER_SCHEMA: &str = "org.gnome.desktop.app-folders.folder";

/// One folder's definition.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub apps: Vec<String>,
    pub categories: Vec<String>,
    pub excluded: Vec<String>,
}

fn same_app(a: &str, b: &str) -> bool {
    a.trim_end_matches(".desktop") == b.trim_end_matches(".desktop")
}

impl Folder {
    /// Whether `app` sits in this folder (GNOME's rule).
    pub fn contains(&self, app: &AppEntry) -> bool {
        if self.excluded.iter().any(|e| same_app(e, &app.app_id)) {
            return false;
        }
        self.apps.iter().any(|a| same_app(a, &app.app_id))
            || app
                .categories
                .iter()
                .any(|c| self.categories.iter().any(|f| f == c))
    }
}

/// The grid's layout: folders that hold at least one installed app (with
/// those apps), and the apps no folder took, each sorted by name. Apps in
/// the dash (`favorites`) are left out of both, as GNOME 40 and later do.
pub fn arrange<'a>(
    folders: &[Folder],
    apps: &'a [AppEntry],
    favorites: &[String],
) -> (Vec<(Folder, Vec<&'a AppEntry>)>, Vec<&'a AppEntry>) {
    let shown: Vec<&AppEntry> = apps
        .iter()
        .filter(|a| !favorites.iter().any(|f| same_app(f, &a.app_id)))
        .collect();
    let mut filled: Vec<(Folder, Vec<&AppEntry>)> = folders
        .iter()
        .map(|f| {
            let mut members: Vec<&AppEntry> =
                shown.iter().copied().filter(|a| f.contains(a)).collect();
            members.sort_by_key(|a| a.name.to_lowercase());
            (f.clone(), members)
        })
        .filter(|(_, members)| !members.is_empty())
        .collect();
    filled.sort_by_key(|(f, _)| f.name.to_lowercase());
    let mut loose: Vec<&AppEntry> = shown
        .iter()
        .copied()
        .filter(|a| {
            !filled
                .iter()
                .any(|(_, members)| members.iter().any(|m| m.app_id == a.app_id))
        })
        .collect();
    loose.sort_by_key(|a| a.name.to_lowercase());
    (filled, loose)
}

/// A translated folder name: the Name of `<data dir>/desktop-directories/
/// <file>`, else the file stem.
fn directory_name(file: &str) -> String {
    let dirs = std::iter::once(glib::user_data_dir()).chain(glib::system_data_dirs());
    for dir in dirs {
        let path: PathBuf = dir.join("desktop-directories").join(file);
        let key_file = glib::KeyFile::new();
        if key_file
            .load_from_file(&path, glib::KeyFileFlags::NONE)
            .is_ok()
        {
            if let Ok(name) = key_file.locale_string("Desktop Entry", "Name", None) {
                return name.to_string();
            }
        }
    }
    file.trim_end_matches(".directory").to_owned()
}

/// GNOME 51's default folders (appDisplay.js DEFAULT_FOLDERS, with the
/// app lists from its build configuration): id, `.directory` name,
/// categories, apps.
const DEFAULT_FOLDERS: &[(&str, &str, &[&str], &[&str])] = &[
    (
        "System",
        "X-GNOME-Shell-System.directory",
        &[],
        &[
            "nm-connection-editor.desktop",
            "org.gnome.DejaDup.desktop",
            "org.gnome.baobab.desktop",
            "org.gnome.DiskUtility.desktop",
            "org.gnome.Logs.desktop",
            "org.freedesktop.MalcontentControl.desktop",
            "org.freedesktop.GnomeAbrt.desktop",
            "org.gnome.Sysprof.desktop",
            "org.gnome.SystemMonitor.desktop",
            "org.gnome.tweaks.desktop",
        ],
    ),
    (
        "Utilities",
        "X-GNOME-Shell-Utilities.directory",
        &[],
        &[
            "org.gnome.Decibels.desktop",
            "org.gnome.Connections.desktop",
            "org.gnome.Papers.desktop",
            "org.gnome.FileRoller.desktop",
            "org.gnome.font-viewer.desktop",
            "org.gnome.Loupe.desktop",
            "org.gnome.seahorse.Application.desktop",
            "org.gnome.Seahorse.desktop",
            "org.gnome.Showtime.desktop",
        ],
    ),
    ("YaST", "suse-yast.directory", &["X-SuSE-YaST"], &[]),
    ("Pardus", "X-Pardus-Apps.directory", &["X-Pardus-Apps"], &[]),
];

/// GNOME's first-run folders for these installed apps: each default
/// folder, with only the listed apps that are installed.
pub fn default_folders(installed: &[AppEntry]) -> Vec<Folder> {
    DEFAULT_FOLDERS
        .iter()
        .map(|(id, name, categories, apps)| Folder {
            id: (*id).to_owned(),
            name: (*name).to_owned(),
            apps: apps
                .iter()
                .filter(|a| installed.iter().any(|i| same_app(a, &i.app_id)))
                .map(|a| (*a).to_owned())
                .collect(),
            categories: categories.iter().map(|c| (*c).to_owned()).collect(),
            excluded: Vec::new(),
        })
        .collect()
}

/// GNOME's `_ensureDefaultFolders`: an account that never set
/// `folder-children` gets the default folders written, as GNOME Shell
/// writes them on its first run.
pub fn ensure_defaults(installed: &[AppEntry]) {
    let Some(source) = gio::SettingsSchemaSource::default() else {
        return;
    };
    if source.lookup(SCHEMA, true).is_none() || source.lookup(FOLDER_SCHEMA, true).is_none() {
        return;
    }
    let root = gio::Settings::new(SCHEMA);
    if root.user_value("folder-children").is_some() || !root.strv("folder-children").is_empty() {
        return;
    }
    let folders = default_folders(installed);
    let ids: Vec<&str> = folders.iter().map(|f| f.id.as_str()).collect();
    let _ = root.set_strv("folder-children", ids.as_slice());
    for folder in &folders {
        let path = format!("/org/gnome/desktop/app-folders/folders/{}/", folder.id);
        let s = gio::Settings::with_path(FOLDER_SCHEMA, &path);
        let _ = s.set_string("name", &folder.name);
        let _ = s.set_boolean("translate", true);
        if !folder.categories.is_empty() {
            let c: Vec<&str> = folder.categories.iter().map(String::as_str).collect();
            let _ = s.set_strv("categories", c.as_slice());
        }
        if !folder.apps.is_empty() {
            let a: Vec<&str> = folder.apps.iter().map(String::as_str).collect();
            let _ = s.set_strv("apps", a.as_slice());
        }
    }
    gio::Settings::sync();
}

/// The user's folders from GSettings (none when the schema is missing).
pub fn load() -> Vec<Folder> {
    let Some(source) = gio::SettingsSchemaSource::default() else {
        return Vec::new();
    };
    if source.lookup(SCHEMA, true).is_none() || source.lookup(FOLDER_SCHEMA, true).is_none() {
        return Vec::new();
    }
    let root = gio::Settings::new(SCHEMA);
    root.strv("folder-children")
        .iter()
        .map(|id| {
            let path = format!("/org/gnome/desktop/app-folders/folders/{id}/");
            let s = gio::Settings::with_path(FOLDER_SCHEMA, &path);
            let list = |key: &str| s.strv(key).iter().map(|v| v.to_string()).collect();
            let raw = s.string("name").to_string();
            let name = if s.boolean("translate") {
                directory_name(&raw)
            } else if raw.is_empty() {
                id.to_string()
            } else {
                raw
            };
            Folder {
                id: id.to_string(),
                name,
                apps: list("apps"),
                categories: list("categories"),
                excluded: list("excluded-apps"),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, categories: &[&str]) -> AppEntry {
        AppEntry {
            app_id: id.into(),
            name: name.into(),
            generic_name: None,
            keywords: Vec::new(),
            argv: Vec::new(),
            icon: None,
            categories: categories.iter().map(|c| (*c).to_owned()).collect(),
        }
    }

    #[test]
    fn folders_take_apps_by_id_and_category_minus_exclusions() {
        let utilities = Folder {
            id: "Utilities".into(),
            name: "Utilities".into(),
            apps: vec!["org.gnome.Logs.desktop".into()],
            categories: vec!["X-GNOME-Utilities".into()],
            excluded: vec!["org.gnome.Console.desktop".into()],
        };
        let empty = Folder {
            id: "Empty".into(),
            name: "Empty".into(),
            apps: vec!["missing.desktop".into()],
            ..Default::default()
        };
        let apps = vec![
            app("org.gnome.Logs", "Logs", &[]),
            app("org.gnome.Calculator", "Calculator", &["X-GNOME-Utilities"]),
            app("org.gnome.Console", "Console", &["X-GNOME-Utilities"]),
            app("firefox", "Firefox", &["Network"]),
        ];
        let (folders, loose) = arrange(&[utilities, empty], &apps, &[]);
        assert_eq!(folders.len(), 1, "a folder with no installed app is hidden");
        let names: Vec<&str> = folders[0].1.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Calculator", "Logs"]);
        let loose: Vec<&str> = loose.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            loose,
            ["Console", "Firefox"],
            "excluded and unfiled apps stay loose"
        );
    }

    #[test]
    fn first_run_folders_follow_gnome_51() {
        let installed = vec![
            app(
                "org.freedesktop.MalcontentControl",
                "Parental Controls",
                &[],
            ),
            app("org.gnome.Loupe", "Image Viewer", &[]),
        ];
        let folders = default_folders(&installed);
        let ids: Vec<&str> = folders.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["System", "Utilities", "YaST", "Pardus"]);
        assert_eq!(
            folders[0].apps,
            ["org.freedesktop.MalcontentControl.desktop"]
        );
        assert_eq!(folders[1].apps, ["org.gnome.Loupe.desktop"]);
        assert_eq!(folders[2].categories, ["X-SuSE-YaST"]);
        assert!(folders[2].apps.is_empty());
        let (filled, loose) = arrange(&folders, &installed, &[]);
        assert_eq!(filled.len(), 2);
        assert!(loose.is_empty());
    }

    #[test]
    fn dash_favorites_leave_the_grid() {
        let utilities = Folder {
            id: "Utilities".into(),
            name: "Utilities".into(),
            categories: vec!["X-GNOME-Utilities".into()],
            ..Default::default()
        };
        let apps = vec![
            app("org.gnome.Calculator", "Calculator", &["X-GNOME-Utilities"]),
            app("org.gnome.Nautilus", "Files", &[]),
            app("org.gnome.Settings", "Settings", &[]),
        ];
        let favorites = vec![
            "org.gnome.Calculator.desktop".to_owned(),
            "org.gnome.Nautilus.desktop".to_owned(),
        ];
        let (folders, loose) = arrange(&[utilities], &apps, &favorites);
        assert!(folders.is_empty(), "a folder of favorites alone is hidden");
        let loose: Vec<&str> = loose.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(loose, ["Settings"]);
    }
}
