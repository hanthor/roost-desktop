//! Desktop-entry discovery, search, and launching (002 T2).
//!
//! The freedesktop reader is the [`freedesktop-desktop-entry`] crate
//! (MPL-2.0, recorded here per the plan: parsing `.desktop` files is
//! locale/escape/field-code work we do not reimplement). This module
//! resolves its borrowed types into owned [`AppEntry`] records at
//! discovery, filters to launchable entries, and serves them through
//! [`AppProvider`] plus [`launch`] with visible feedback.
//!
//! Launching is fail-closed and shell-free: entries without a usable
//! `Exec` line never load, argument vectors come from the crate's
//! field-code parser (never a shell string), and stdio is nulled so a
//! launched app can outlive the shell as an orphan.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use freedesktop_desktop_entry::{DesktopEntry as CrateEntry, Iter};

use crate::search::{SearchProvider, SearchResult};

/// Locales retained from desktop entries (matches the greeter's pick:
/// full environment locale handling is later work).
const LOCALES: &[&str] = &["en_US.UTF-8", "en_US", "en", "C"];
/// How long a freshly launched app counts as "starting" for feedback.
const STARTING_WINDOW: Duration = Duration::from_secs(10);
/// Pending launched children remembered for feedback polling.
const MAX_TRACKED_LAUNCHES: usize = 16;

/// One launchable application resolved at discovery.
#[derive(Debug, Clone)]
pub struct AppEntry {
    /// Desktop-entry id (`org.gnome.Terminal.desktop`).
    pub app_id: String,
    /// Display name (localized, falling back to the entry id stem).
    pub name: String,
    /// Generic name (`Terminal`, `Web Browser`), if any.
    pub generic_name: Option<String>,
    /// Search keywords, if any.
    pub keywords: Vec<String>,
    /// Parsed `Exec` argument vector (program plus field-code args).
    pub argv: Vec<OsString>,
    /// Icon name or path, if any (rendering resolves it later).
    pub icon: Option<String>,
    /// Desktop-entry categories (`Utility`, `X-GNOME-Utilities`), which
    /// GNOME's app folders group by.
    pub categories: Vec<String>,
}

impl AppEntry {
    /// Lowercased haystack for substring search.
    fn haystack(&self) -> String {
        let mut hay = String::new();
        hay.push_str(&self.name.to_lowercase());
        hay.push(' ');
        hay.push_str(&self.app_id.to_lowercase());
        if let Some(generic) = &self.generic_name {
            hay.push(' ');
            hay.push_str(&generic.to_lowercase());
        }
        for keyword in &self.keywords {
            hay.push(' ');
            hay.push_str(&keyword.to_lowercase());
        }
        hay
    }
}

/// Application directories in precedence order:
///
/// 1. `$XDG_DATA_HOME/applications` (default `~/.local/share`; omitted
///    entirely when neither is set, see [`crate::xdg`]),
/// 2. each `$XDG_DATA_DIRS/applications` (default
///    `/usr/local/share`, `/usr/share`).
pub fn default_app_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // Fail closed (#49): no HOME means no user dir, never /tmp.
    if let Some(home) = crate::xdg::data_home() {
        dirs.push(home.join("applications"));
    }
    for dir in data_dirs() {
        dirs.push(dir.join("applications"));
    }
    dirs
}

/// Whether a desktop file named `<id>.desktop` exists in any
/// application directory, NoDisplay and Hidden ones included: GNOME's
/// WindowTracker matches windows to those apps too.
pub fn desktop_file_exists(id: &str) -> bool {
    let file = format!("{id}.desktop");
    default_app_dirs()
        .iter()
        .any(|dir| dir.join(&file).is_file())
}

fn data_dirs() -> Vec<PathBuf> {
    std::env::var_os("XDG_DATA_DIRS")
        .map(|dirs| {
            std::env::split_paths(&dirs)
                .filter(|p| p.is_absolute())
                .collect()
        })
        .unwrap_or_else(|| {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        })
}

/// Discover launchable entries under `dirs`.
///
/// Duplicate entry ids resolve first-dir-wins, matching desktop-file
/// precedence, before anything is filtered: a `Hidden` or `NoDisplay`
/// copy in the user's directory hides the system one, as the spec says.
/// Then skips hidden/`NoDisplay` entries, entries `OnlyShowIn`/
/// `NotShowIn` keep off this desktop (`XDG_CURRENT_DESKTOP`, as GLib
/// decides), and entries whose `Exec` line is missing or fails
/// field-code parsing — an app the shell cannot safely spawn is not an
/// app the overview offers.
pub fn discover(dirs: &[PathBuf]) -> Vec<AppEntry> {
    let locales: Vec<String> = LOCALES.iter().map(|s| s.to_string()).collect();
    let desktops = current_desktops();
    let mut seen = std::collections::HashSet::new();
    Iter::new(dirs.iter().cloned())
        .entries(Some(&locales))
        .filter(|entry| seen.insert(entry.id().to_owned()))
        .filter(|entry| shown_in(entry.only_show_in(), entry.not_show_in(), &desktops))
        .filter_map(|entry| from_crate_entry(&entry))
        .collect()
}

/// `XDG_CURRENT_DESKTOP`'s names, in order.
fn current_desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .collect()
}

/// GLib's `g_desktop_app_info_get_show_in`: the first current desktop an
/// entry names decides (`OnlyShowIn` shows, `NotShowIn` hides); naming
/// none, an entry shows unless it has an `OnlyShowIn`.
pub fn shown_in(only: Option<Vec<&str>>, not: Option<Vec<&str>>, desktops: &[String]) -> bool {
    for desktop in desktops {
        if only
            .as_ref()
            .is_some_and(|o| o.iter().any(|d| d == desktop))
        {
            return true;
        }
        if not.as_ref().is_some_and(|n| n.iter().any(|d| d == desktop)) {
            return false;
        }
    }
    only.is_none()
}

/// Discover from the host's [`default_app_dirs`].
pub fn discover_system() -> Vec<AppEntry> {
    discover(&default_app_dirs())
}

fn from_crate_entry(entry: &CrateEntry) -> Option<AppEntry> {
    from_crate_entry_for_launch(entry, false)
}

fn from_crate_entry_for_launch(entry: &CrateEntry, allow_no_display: bool) -> Option<AppEntry> {
    if entry.hidden() || (!allow_no_display && entry.no_display()) {
        return None;
    }
    entry.exec()?;
    let argv: Vec<OsString> = entry
        .parse_exec()
        .ok()?
        .into_iter()
        .map(|arg| {
            // The Exec parser splits on whitespace but leaves the
            // spec's double-quote grouping in place, so a quoted
            // argument arrives with its quotes attached. Strip one
            // surrounding pair per argument; without this a probe like
            // Exec=touch "<marker>" touches a quote-named file.
            arg.strip_prefix('"')
                .and_then(|inner| inner.strip_suffix('"'))
                .unwrap_or(&arg)
                .to_owned()
        })
        .map(OsString::from)
        .collect();
    if argv.is_empty() {
        return None;
    }
    let app_id = entry.id().to_owned();
    let name = entry
        .name::<&str>(&[])
        .map(|name| name.into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| app_id.trim_end_matches(".desktop").to_owned());
    let categories = entry
        .categories()
        .map(|c| {
            c.into_iter()
                .filter(|c| !c.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(AppEntry {
        app_id,
        name,
        categories,
        generic_name: entry
            .generic_name::<&str>(&[])
            .map(|name| name.into_owned())
            .filter(|name| !name.is_empty()),
        keywords: entry
            .keywords::<&str>(&[])
            .map(|words| {
                words
                    .into_iter()
                    .map(|word| word.into_owned())
                    .filter(|word| !word.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        argv,
        icon: entry.icon().map(str::to_owned),
    })
}

/// Parse one desktop file into a launchable entry: the same
/// launchability filter [`discover`] applies, so a stack `.desktop`
/// file the shell cannot spawn is not a stack cell it offers.
/// Returns `None` for unreadable, hidden, or `Exec`-less files.
pub fn entry_from_file(path: &std::path::Path) -> Option<AppEntry> {
    let locales: Vec<String> = LOCALES.iter().map(|s| s.to_string()).collect();
    let entry = CrateEntry::from_path(path, Some(&locales)).ok()?;
    from_crate_entry(&entry)
}

/// Resolve an explicitly selected default MIME handler. `NoDisplay` hides
/// an app from menus, but does not prevent users choosing it as a handler.
/// `Hidden` entries still mask installed copies and must never launch.
pub fn handler_entry_from_file(path: &std::path::Path) -> Option<AppEntry> {
    let locales: Vec<String> = LOCALES.iter().map(|s| s.to_string()).collect();
    let entry = CrateEntry::from_path(path, Some(&locales)).ok()?;
    from_crate_entry_for_launch(&entry, true)
}

/// [`SearchProvider`] over discovered entries: case-insensitive
/// substring over name, id, generic name, and keywords.
pub struct AppProvider {
    apps: Vec<AppEntry>,
}

impl AppProvider {
    /// Provider over already-discovered entries.
    pub fn new(apps: Vec<AppEntry>) -> Self {
        Self { apps }
    }

    /// Provider over the host's [`default_app_dirs`].
    pub fn system() -> Self {
        Self::new(discover_system())
    }

    /// Current entries (discovery snapshot).
    pub fn apps(&self) -> &[AppEntry] {
        &self.apps
    }

    /// Entry behind a launch action, if still known.
    pub fn entry(&self, app_id: &str) -> Option<&AppEntry> {
        self.apps.iter().find(|app| app.app_id == app_id)
    }
}

impl SearchProvider for AppProvider {
    fn id(&self) -> &'static str {
        "apps"
    }

    fn query(&self, text: &str) -> Vec<SearchResult> {
        let needle = text.to_lowercase();
        self.apps
            .iter()
            .filter(|app| app.haystack().contains(&needle))
            .take(crate::search::MAX_PROVIDER_RESULTS)
            .map(|app| SearchResult::launch(app.name.clone(), app.app_id.clone()))
            .collect()
    }
}

/// A spawned application the shell tracks for launch feedback.
#[derive(Debug)]
pub struct Launched {
    /// Desktop-entry id that was launched.
    pub app_id: String,
    /// OS process id for diagnostics (never trusted for signaling).
    pub pid: u32,
    child: Option<Child>,
    started_at: Instant,
}

/// Honest launch state polled from the tracked child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchState {
    /// Spawned within [`STARTING_WINDOW`] and still running.
    Starting,
    /// Alive past the starting window (presumed up; the compositor
    /// owns real window-mapping truth).
    Running,
    /// The child exited (code preserved for diagnostics).
    Exited {
        /// Exit status code (`None` = signaled).
        code: Option<i32>,
    },
}

/// Spawn `entry` detached from the shell's stdio and track it.
///
/// Direct `argv` execution — no shell, so `Exec` field codes parsed
/// by the crate cannot smuggle redirections or substitutions. The
/// child is an orphan on purpose: shell restarts (001 supervision)
/// must not take launched apps down.
/// Session variables every launched app inherits on top of the shell's
/// own environment (#59: `DISPLAY` once XWayland is up). Set from the
/// compositor's `Environment` message.
static LAUNCH_ENV: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

/// Replace the session variables launched apps inherit.
pub fn set_launch_environment(vars: Vec<(String, String)>) {
    if let Ok(mut env) = LAUNCH_ENV.lock() {
        *env = vars;
    }
}

/// The session variables launched apps inherit.
pub fn launch_environment() -> Vec<(String, String)> {
    LAUNCH_ENV.lock().map(|env| env.clone()).unwrap_or_default()
}

pub fn launch(entry: &AppEntry) -> io::Result<Launched> {
    let (program, args) = entry
        .argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty Exec argv"))?;
    let child = Command::new(program)
        .args(args)
        .envs(launch_environment())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(Launched {
        app_id: entry.app_id.clone(),
        pid: child.id(),
        child: Some(child),
        started_at: Instant::now(),
    })
}

impl Launched {
    /// Poll the child without blocking: reaps on exit.
    pub fn state(&mut self) -> LaunchState {
        let running = match self.child.as_mut().and_then(|c| c.try_wait().ok()) {
            Some(Some(status)) => {
                self.child = None;
                return LaunchState::Exited {
                    code: status.code(),
                };
            }
            Some(None) => true,
            None => return LaunchState::Exited { code: None },
        };
        let _ = running;
        if self.started_at.elapsed() < STARTING_WINDOW {
            LaunchState::Starting
        } else {
            LaunchState::Running
        }
    }
}

/// Bounded tracker behind overview launch feedback ("starting…"
/// badges): app id to live [`Launched`] children, oldest evicted past
/// [`MAX_TRACKED_LAUNCHES`].
#[derive(Debug, Default)]
pub struct LaunchTracker {
    launches: HashMap<String, Vec<Launched>>,
}

impl LaunchTracker {
    /// Empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn `entry` and track it for feedback.
    pub fn launch(&mut self, entry: &AppEntry) -> io::Result<u32> {
        let launched = launch(entry)?;
        let pid = launched.pid;
        let queue = self.launches.entry(entry.app_id.clone()).or_default();
        queue.push(launched);
        while self.len() > MAX_TRACKED_LAUNCHES {
            let oldest = self
                .launches
                .iter_mut()
                .filter(|(_, queue)| !queue.is_empty())
                .min_by_key(|(_, queue)| queue.first().map(|l| l.started_at))
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => {
                    let queue = self.launches.get_mut(&id).expect("oldest exists");
                    queue.remove(0);
                    if queue.is_empty() {
                        self.launches.remove(&id);
                    }
                }
                None => break,
            }
        }
        Ok(pid)
    }

    /// Tracked children across all apps.
    pub fn len(&self) -> usize {
        self.launches.values().map(Vec::len).sum()
    }

    /// Whether anything is tracked.
    pub fn is_empty(&self) -> bool {
        self.launches.is_empty()
    }

    /// Current feedback state per app (reaps exited children).
    pub fn states(&mut self) -> HashMap<String, Vec<LaunchState>> {
        let mut out = HashMap::new();
        for (id, queue) in self.launches.iter_mut() {
            out.insert(id.clone(), queue.iter_mut().map(Launched::state).collect());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchAction;
    use std::fs;
    use std::path::Path;

    fn write_entry(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).expect("write test entry");
    }

    fn sample_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        write_entry(
            dir.path(),
            "term.desktop",
            "[Desktop Entry]\nName=Terminal\nGenericName=Terminal Emulator\nKeywords=shell;prompt;\nExec=/bin/true\nType=Application\n",
        );
        // Name and id deliberately differ: queries for `zshell` can
        // only match through the entry id.
        write_entry(
            dir.path(),
            "zshell.desktop",
            "[Desktop Entry]\nName=Zebra Console\nExec=/bin/true\nType=Application\n",
        );
        write_entry(
            dir.path(),
            "hidden.desktop",
            "[Desktop Entry]\nName=Hidden\nExec=/bin/true\nNoDisplay=true\nType=Application\n",
        );
        write_entry(
            dir.path(),
            "noexec.desktop",
            "[Desktop Entry]\nName=NoExec\nType=Application\n",
        );
        dir
    }

    #[test]
    fn discover_filters_to_launchable_entries() {
        let dir = sample_dir();
        let apps = discover(&[dir.path().to_owned()]);
        assert_eq!(apps.len(), 2, "hidden and exec-less entries are skipped");
        let app = apps.iter().find(|a| a.name == "Terminal").expect("term");
        // Temp dirs carry no `/applications/` segment, so the id is the
        // file stem, not the file name.
        assert_eq!(app.app_id, "term");
        assert_eq!(app.generic_name.as_deref(), Some("Terminal Emulator"));
        assert!(app.keywords.iter().any(|k| k == "shell"));
    }

    #[test]
    fn duplicate_ids_resolve_first_dir_wins() {
        let first = tempfile::tempdir().expect("tempdir");
        let second = tempfile::tempdir().expect("tempdir");
        write_entry(
            first.path(),
            "dup.desktop",
            "[Desktop Entry]\nName=First\nExec=/bin/true\nType=Application\n",
        );
        write_entry(
            second.path(),
            "dup.desktop",
            "[Desktop Entry]\nName=Second\nExec=/bin/true\nType=Application\n",
        );
        let apps = discover(&[first.path().to_owned(), second.path().to_owned()]);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "First");
    }

    #[test]
    fn a_users_hidden_copy_hides_the_system_entry() {
        let user = tempfile::tempdir().expect("tempdir");
        let system = tempfile::tempdir().expect("tempdir");
        write_entry(
            user.path(),
            "gone.desktop",
            "[Desktop Entry]\nName=Gone\nExec=/bin/true\nType=Application\nHidden=true\n",
        );
        write_entry(
            system.path(),
            "gone.desktop",
            "[Desktop Entry]\nName=Gone\nExec=/bin/true\nType=Application\n",
        );
        let apps = discover(&[user.path().to_owned(), system.path().to_owned()]);
        assert!(apps.is_empty(), "the user's Hidden copy wins");
    }

    #[test]
    fn show_in_follows_glib() {
        let gnome = vec!["Roost".to_owned(), "GNOME".to_owned()];
        assert!(shown_in(None, None, &gnome));
        assert!(shown_in(Some(vec!["GNOME"]), None, &gnome));
        assert!(!shown_in(Some(vec!["KDE"]), None, &gnome));
        assert!(!shown_in(None, Some(vec!["GNOME"]), &gnome));
        // The first desktop that the entry names decides.
        assert!(shown_in(Some(vec!["Roost"]), Some(vec!["GNOME"]), &gnome));
        assert!(!shown_in(Some(vec!["GNOME"]), None, &[]));
    }

    #[test]
    fn provider_matches_name_generic_keywords_and_id() {
        let dir = sample_dir();
        let provider = AppProvider::new(discover(&[dir.path().to_owned()]));
        assert_eq!(provider.query("term").len(), 1);
        assert_eq!(provider.query("emulator").len(), 1, "generic name matches");
        assert_eq!(provider.query("prompt").len(), 1, "keywords match");
        assert_eq!(provider.query("zshell").len(), 1, "id matches");
        assert!(provider.query("browser").is_empty());
        let hit = &provider.query("term")[0];
        assert_eq!(
            hit.action,
            SearchAction::Launch {
                app_id: "term".to_owned()
            }
        );
    }

    #[test]
    fn quoted_exec_arg_survives_discovery_unquoted() {
        // Probe shape from the CI journey: Exec=touch "<abs marker>".
        // The desktop-entry spec quotes Exec arguments; argv must not
        // carry the quote characters into the spawn.
        let dir = tempfile::tempdir().expect("tempdir");
        write_entry(
            dir.path(),
            "probe.desktop",
            "[Desktop Entry]\nName=Roostterm Probe\nExec=touch \"/tmp/roost-marker\"\nType=Application\n",
        );
        let apps = discover(&[dir.path().to_owned()]);
        let app = apps
            .iter()
            .find(|a| a.name == "Roostterm Probe")
            .expect("probe");
        assert_eq!(
            app.argv,
            vec![OsString::from("touch"), OsString::from("/tmp/roost-marker")],
            "quoted Exec arg keeps its quotes: {:?}",
            app.argv
        );
    }

    #[test]
    fn launch_true_exits_zero_and_tracker_reports_it() {
        let dir = sample_dir();
        let provider = AppProvider::new(discover(&[dir.path().to_owned()]));
        let entry = provider.entry("term").expect("term entry");
        let mut tracker = LaunchTracker::new();
        tracker.launch(entry).expect("spawn /bin/true");
        assert_eq!(tracker.len(), 1);
        // Wait for the immediate exit to become visible to polling.
        // Deadline, not an iteration budget (#72): a loaded runner only
        // makes this wait longer.
        let mut exited = false;
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            let states = tracker.states();
            if states["term"]
                .iter()
                .any(|s| matches!(s, LaunchState::Exited { code: Some(0) }))
            {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(exited, "/bin/true launch reports Exited(0)");
    }

    #[test]
    fn launch_missing_binary_fails_closed() {
        let entry = AppEntry {
            app_id: "missing.desktop".to_owned(),
            name: "Missing".to_owned(),
            generic_name: None,
            keywords: Vec::new(),
            argv: vec![OsString::from("/nonexistent-roost-binary-xyz")],
            icon: None,
            categories: Vec::new(),
        };
        let mut tracker = LaunchTracker::new();
        assert!(tracker.launch(&entry).is_err());
        assert!(tracker.is_empty(), "failed spawns track nothing");
    }
}
