//! Overview search providers (#57): GNOME Shell's remote search.
//!
//! Apps install `gnome-shell/search-providers/*.ini` under a data dir
//! and serve `org.gnome.Shell.SearchProvider2` on the session bus.
//! Discovery and ordering follow GNOME 51:
//!
//! - a provider counts only when its `DesktopId` app is installed;
//! - `org.gnome.desktop.search-providers`: `disable-external` turns all
//!   off, `disabled` drops one, `enabled` opts in a `DefaultDisabled`
//!   one, and `sort-order` ranks them (unlisted ones follow, by name);
//! - every query is asynchronous with a deadline, and a newer query
//!   cancels the older one, so a slow provider never delays app results.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::prelude::*;

/// Provider D-Bus interface (version 2, the only one GNOME 51 calls).
pub const IFACE: &str = "org.gnome.Shell.SearchProvider2";
/// GNOME's per-provider budget before results are dropped.
pub const CALL_TIMEOUT_MS: i32 = 5000;
/// Rows a list provider shows (GNOME 51's list results).
pub const MAX_ROWS: usize = 3;
const SETTINGS_SCHEMA: &str = "org.gnome.desktop.search-providers";

/// One provider from its `.ini` file.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderInfo {
    pub desktop_id: String,
    pub bus_name: String,
    pub object_path: String,
    pub default_disabled: bool,
}

/// Parse one provider file; `None` unless it is a complete version-2
/// provider.
pub fn parse_ini(text: &str) -> Option<ProviderInfo> {
    let key_file = glib::KeyFile::new();
    key_file
        .load_from_data(text, glib::KeyFileFlags::NONE)
        .ok()?;
    let group = "Shell Search Provider";
    let get = |k: &str| key_file.string(group, k).ok().map(|s| s.to_string());
    if key_file.integer(group, "Version").ok()? != 2 {
        return None;
    }
    Some(ProviderInfo {
        desktop_id: get("DesktopId")?,
        bus_name: get("BusName")?,
        object_path: get("ObjectPath")?,
        default_disabled: key_file.boolean(group, "DefaultDisabled").unwrap_or(false),
    })
}

/// The user's provider settings (GNOME's keys, defaults when absent).
#[derive(Debug, Default, Clone)]
pub struct ProviderSettings {
    pub disable_external: bool,
    pub disabled: Vec<String>,
    pub enabled: Vec<String>,
    pub sort_order: Vec<String>,
}

impl ProviderSettings {
    /// Read GNOME's keys; defaults when the schema is not installed.
    pub fn load() -> Self {
        let Some(source) = gio::SettingsSchemaSource::default() else {
            return Self::default();
        };
        if source.lookup(SETTINGS_SCHEMA, true).is_none() {
            return Self::default();
        }
        let s = gio::Settings::new(SETTINGS_SCHEMA);
        let list = |k: &str| s.strv(k).iter().map(|v| v.to_string()).collect();
        Self {
            disable_external: s.boolean("disable-external"),
            disabled: list("disabled"),
            enabled: list("enabled"),
            sort_order: list("sort-order"),
        }
    }
}

/// Filter and order providers as GNOME does. `installed` says whether
/// a desktop id names an installed app.
pub fn select(
    mut found: Vec<ProviderInfo>,
    settings: &ProviderSettings,
    installed: impl Fn(&str) -> bool,
) -> Vec<ProviderInfo> {
    if settings.disable_external {
        return Vec::new();
    }
    found.retain(|p| {
        installed(&p.desktop_id)
            && if p.default_disabled {
                settings.enabled.contains(&p.desktop_id)
            } else {
                !settings.disabled.contains(&p.desktop_id)
            }
    });
    // One provider per desktop id: the first data dir wins.
    let mut seen = std::collections::HashSet::new();
    found.retain(|p| seen.insert(p.desktop_id.clone()));
    let rank = |p: &ProviderInfo| {
        settings
            .sort_order
            .iter()
            .position(|id| *id == p.desktop_id)
            .unwrap_or(usize::MAX)
    };
    found.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.desktop_id.cmp(&b.desktop_id)));
    found
}

/// Data dirs to scan, user first, then the system ones.
pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![glib::user_data_dir()];
    dirs.extend(glib::system_data_dirs());
    dirs
}

/// Every parseable provider file under `dirs`, in dir then name order.
pub fn discover(dirs: &[PathBuf]) -> Vec<ProviderInfo> {
    let mut out = Vec::new();
    for dir in dirs {
        let dir: &Path = &dir.join("gnome-shell/search-providers");
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "ini"))
            .collect();
        files.sort();
        for file in files {
            if let Some(info) = std::fs::read_to_string(&file)
                .ok()
                .and_then(|t| parse_ini(&t))
            {
                out.push(info);
            }
        }
    }
    out
}

/// One displayable result.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultMeta {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<gio::Icon>,
}

/// Decode one `GetResultMetas` entry (`a{sv}`); `None` without id/name.
pub fn parse_meta(dict: &glib::VariantDict) -> Option<ResultMeta> {
    let string = |k: &str| dict.lookup_value(k, None).and_then(|v| v.get::<String>());
    let icon = dict
        .lookup_value("icon", None)
        .and_then(|v| gio::Icon::deserialize(&v))
        .or_else(|| string("gicon").and_then(|s| gio::Icon::for_string(&s).ok()));
    Some(ResultMeta {
        id: string("id")?,
        name: string("name")?,
        description: string("description").filter(|d| !d.is_empty()),
        icon,
    })
}

/// A provider bound to the session bus.
#[derive(Clone)]
pub struct Remote {
    pub info: ProviderInfo,
    conn: gio::DBusConnection,
}

impl Remote {
    pub fn new(info: ProviderInfo, conn: &gio::DBusConnection) -> Self {
        Self {
            info,
            conn: conn.clone(),
        }
    }

    fn call(
        &self,
        method: &str,
        args: glib::Variant,
        reply: Option<&str>,
        cancel: &gio::Cancellable,
        done: impl FnOnce(Option<glib::Variant>) + 'static,
    ) {
        let reply = reply.and_then(|r| glib::VariantTy::new(r).ok());
        self.conn.call(
            Some(&self.info.bus_name),
            &self.info.object_path,
            IFACE,
            method,
            Some(&args),
            reply,
            gio::DBusCallFlags::NONE,
            CALL_TIMEOUT_MS,
            Some(cancel),
            move |r| done(r.ok()),
        );
    }

    /// Result ids and their metas for `terms`; `done` gets an empty list
    /// on any failure, cancellation, or timeout.
    pub fn search(
        &self,
        terms: Vec<String>,
        cancel: &gio::Cancellable,
        done: impl FnOnce(Vec<ResultMeta>) + 'static,
    ) {
        let me = self.clone();
        let cancel2 = cancel.clone();
        let done = Rc::new(RefCell::new(Some(done)));
        let finish = {
            let done = done.clone();
            move |metas: Vec<ResultMeta>| {
                if let Some(f) = done.borrow_mut().take() {
                    f(metas);
                }
            }
        };
        let finish = Rc::new(finish);
        let f1 = finish.clone();
        self.call(
            "GetInitialResultSet",
            (terms,).to_variant(),
            Some("(as)"),
            cancel,
            move |reply| {
                let ids: Vec<String> = reply
                    .and_then(|v| v.child_value(0).get::<Vec<String>>())
                    .unwrap_or_default();
                let ids: Vec<String> = ids.into_iter().take(MAX_ROWS).collect();
                if ids.is_empty() {
                    return f1(Vec::new());
                }
                let f2 = f1.clone();
                me.call(
                    "GetResultMetas",
                    (ids,).to_variant(),
                    Some("(aa{sv})"),
                    &cancel2,
                    move |reply| {
                        let metas = reply
                            .map(|v| {
                                v.child_value(0)
                                    .iter()
                                    .filter_map(|d| parse_meta(&glib::VariantDict::new(Some(&d))))
                                    .collect()
                            })
                            .unwrap_or_default();
                        f2(metas);
                    },
                );
            },
        );
    }

    /// Open one result (the provider's app decides how).
    pub fn activate(&self, id: &str, terms: Vec<String>) {
        let args = (id, terms, timestamp()).to_variant();
        self.call(
            "ActivateResult",
            args,
            None,
            &gio::Cancellable::new(),
            |_| {},
        );
    }

    /// Open the provider's app on these terms (its icon in the list).
    pub fn launch_search(&self, terms: Vec<String>) {
        let args = (terms, timestamp()).to_variant();
        self.call("LaunchSearch", args, None, &gio::Cancellable::new(), |_| {});
    }
}

fn timestamp() -> u32 {
    (glib::monotonic_time() / 1000) as u32
}

/// The installed app behind a provider's `DesktopId` (app ids are
/// stored without the `.desktop` suffix).
pub fn provider_app<'a>(
    apps: &'a tuna_shell_host::apps::AppProvider,
    desktop_id: &str,
) -> Option<&'a tuna_shell_host::apps::AppEntry> {
    apps.entry(desktop_id.trim_end_matches(".desktop"))
        .or_else(|| apps.entry(desktop_id))
}

/// Split a query into terms as GNOME does (whitespace, lowercase).
pub fn terms(query: &str) -> Vec<String> {
    query.split_whitespace().map(|t| t.to_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILES: &str = "[Shell Search Provider]\nDesktopId=org.gnome.Nautilus.desktop\nBusName=org.gnome.Nautilus\nObjectPath=/org/gnome/Nautilus/SearchProvider\nVersion=2\n";

    fn info(id: &str, default_disabled: bool) -> ProviderInfo {
        ProviderInfo {
            desktop_id: id.into(),
            bus_name: format!("bus.{id}"),
            object_path: "/p".into(),
            default_disabled,
        }
    }

    #[test]
    fn ini_needs_version_2_and_every_key() {
        let p = parse_ini(FILES).unwrap();
        assert_eq!(p.desktop_id, "org.gnome.Nautilus.desktop");
        assert!(!p.default_disabled);
        assert!(parse_ini(&FILES.replace("Version=2", "Version=1")).is_none());
        assert!(parse_ini(&FILES.replace("BusName=org.gnome.Nautilus\n", "")).is_none());
        assert!(parse_ini("not an ini").is_none());
        let off = parse_ini(&format!("{FILES}DefaultDisabled=true\n")).unwrap();
        assert!(off.default_disabled);
    }

    #[test]
    fn selection_follows_gnome_settings() {
        let found = vec![
            info("b.desktop", false),
            info("a.desktop", false),
            info("c.desktop", true),
            info("gone.desktop", false),
            info("a.desktop", false),
        ];
        let installed = |id: &str| id != "gone.desktop";
        let ids = |v: Vec<ProviderInfo>| v.into_iter().map(|p| p.desktop_id).collect::<Vec<_>>();

        let defaults = ProviderSettings::default();
        assert_eq!(
            ids(select(found.clone(), &defaults, installed)),
            ["a.desktop", "b.desktop"]
        );

        let tuned = ProviderSettings {
            disabled: vec!["a.desktop".into()],
            enabled: vec!["c.desktop".into()],
            sort_order: vec!["c.desktop".into()],
            ..Default::default()
        };
        assert_eq!(
            ids(select(found.clone(), &tuned, installed)),
            ["c.desktop", "b.desktop"]
        );

        let off = ProviderSettings {
            disable_external: true,
            ..Default::default()
        };
        assert!(select(found, &off, installed).is_empty());
    }

    #[test]
    fn metas_need_id_and_name() {
        let d = glib::VariantDict::new(None);
        d.insert("id", "doc-1");
        d.insert("name", "Report.odt");
        d.insert("description", "");
        let m = parse_meta(&d).unwrap();
        assert_eq!(m.name, "Report.odt");
        assert_eq!(m.description, None);
        let bare = glib::VariantDict::new(None);
        bare.insert("id", "x");
        assert!(parse_meta(&bare).is_none());
    }

    #[test]
    fn desktop_ids_match_with_or_without_the_suffix() {
        let entry = tuna_shell_host::apps::AppEntry {
            app_id: "org.gnome.Nautilus".to_owned(),
            name: "Files".to_owned(),
            generic_name: None,
            keywords: Vec::new(),
            argv: Vec::new(),
            icon: None,
            categories: Vec::new(),
        };
        let apps = tuna_shell_host::apps::AppProvider::new(vec![entry]);
        // GNOME's favorite-apps and DesktopId keys carry ".desktop".
        assert!(provider_app(&apps, "org.gnome.Nautilus.desktop").is_some());
        assert!(provider_app(&apps, "org.gnome.Nautilus").is_some());
        assert!(provider_app(&apps, "org.gnome.Other.desktop").is_none());
    }

    #[test]
    fn terms_split_and_lowercase() {
        assert_eq!(terms("  Quarterly  REPORT "), ["quarterly", "report"]);
        assert!(terms("   ").is_empty());
    }
}
