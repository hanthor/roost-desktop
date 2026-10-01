//! Sandboxed Rhai extension host: bar cells, dock badges, notices.
//!
//! Scripts live as plain text in one user directory and run through a
//! hardened [`Engine`]: the file module resolver is replaced with a
//! dummy (so `import` cannot touch the filesystem), `print` is
//! captured instead of reaching stdout, and an operation budget aborts
//! hangs. The only functions scripts can call are the hand-built API
//! below — no filesystem, network, or process surface exists to reach.
//!
//! Scripts run off the paint and event paths: the shell drives the
//! host on its update cadence and paints from the plain-data
//! [`ScriptOutput`] cache only. Any error (parse, runtime, budget)
//! disables the script with a user-visible note; the host stays up.
//!
//! All code here is original.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use rhai::module_resolvers::DummyModuleResolver;
use rhai::{Engine, EvalAltResult, Scope, AST};

/// Script file extension loaded from the extension directory.
pub const SCRIPT_EXT: &str = "rhai";
/// Max Rhai operations per script run: the hang bound. A script
/// exceeding it is aborted and disabled with a note.
pub const MAX_OPS: u64 = 200_000;
/// Entry function a script may define to receive press actions.
const PRESS_FN: &str = "press";

/// Plain-data output of one script run: everything painters may read.
/// Painters never see scripts, engines, or errors — only this.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScriptOutput {
    /// Bar cell text contributed by `bar_cell`, if any.
    pub cell_text: Option<String>,
    /// Bar cell icon name contributed by `bar_cell`, if any.
    pub cell_icon: Option<String>,
    /// Press action id declared by `on_press`, if any.
    pub press_id: Option<String>,
    /// Dock badge text contributed by `dock_badge`, if any.
    pub badge: Option<String>,
    /// User-visible notes: explicit `notice` calls plus captured
    /// `print` output, in call order.
    pub notices: Vec<String>,
}

/// Lifecycle of one loaded script.
#[derive(Debug, Clone)]
pub struct ExtensionState {
    /// File stem (script name).
    pub name: String,
    /// Last run's cached output.
    pub output: ScriptOutput,
    /// False once the script has failed; a disabled script never
    /// runs again until its file changes.
    pub enabled: bool,
    /// User-visible reason while disabled.
    pub note: Option<String>,
    /// Source mtime at last load; `None` when the file vanished.
    pub mtime: Option<SystemTime>,
}

/// Calls the script API records during one run, behind the lock the
/// `SendSync` engine callbacks require.
#[derive(Debug, Default)]
struct ApiSink {
    cell_text: Option<String>,
    cell_icon: Option<String>,
    press_id: Option<String>,
    badge: Option<String>,
    notices: Vec<String>,
}

/// One script: compiled AST plus its lifecycle state.
struct LoadedScript {
    state: ExtensionState,
    ast: Option<AST>,
}

/// The host: one hardened engine shared by every script, driven on
/// the shell's update cadence.
pub struct ExtensionHost {
    engine: Engine,
    sink: Arc<Mutex<ApiSink>>,
    dir: PathBuf,
    scripts: Vec<LoadedScript>,
}

/// Extension directory under the shared XDG data dir (mirrors the
/// favorites layout; tests pass their own temp dir instead).
/// `None` when no private data dir resolves (#49): scripts are then
/// never loaded, rather than read from a shared directory.
pub fn extension_dir() -> Option<PathBuf> {
    crate::favorites::data_dir().map(|dir| dir.join("extensions"))
}

/// Build the hardened engine: dummy module resolver (no `import`
/// from disk), captured `print`, operation budget, and only the
/// hand-built API functions registered.
fn hardened_engine(sink: Arc<Mutex<ApiSink>>) -> Engine {
    let mut engine = Engine::new();
    engine.set_module_resolver(DummyModuleResolver::new());
    let print_sink = sink.clone();
    engine.on_print(move |text| {
        if let Ok(mut sink) = print_sink.lock() {
            sink.notices.push(text.to_owned());
        }
    });
    engine.on_debug(|_, _, _| {});
    engine.on_progress(|ops| {
        if ops > MAX_OPS {
            Some("operation budget exceeded".into())
        } else {
            None
        }
    });
    let cell_sink = sink.clone();
    engine.register_fn("bar_cell", move |text: &str, icon: &str| {
        if let Ok(mut sink) = cell_sink.lock() {
            sink.cell_text = Some(text.to_owned());
            sink.cell_icon = Some(icon.to_owned());
        }
    });
    let badge_sink = sink.clone();
    engine.register_fn("dock_badge", move |text: &str| {
        if let Ok(mut sink) = badge_sink.lock() {
            sink.badge = Some(text.to_owned());
        }
    });
    let press_sink = sink.clone();
    engine.register_fn("on_press", move |id: &str| {
        if let Ok(mut sink) = press_sink.lock() {
            sink.press_id = Some(id.to_owned());
        }
    });
    let notice_sink = sink.clone();
    engine.register_fn("notice", move |text: &str| {
        if let Ok(mut sink) = notice_sink.lock() {
            sink.notices.push(text.to_owned());
        }
    });
    engine
}

impl ExtensionHost {
    /// Host over a script directory (created on load when missing).
    pub fn new(dir: PathBuf) -> Self {
        let sink = Arc::new(Mutex::new(ApiSink::default()));
        let engine = hardened_engine(sink.clone());
        Self {
            engine,
            sink,
            dir,
            scripts: Vec::new(),
        }
    }

    /// Lifecycle states of every known script, in load order.
    pub fn states(&self) -> Vec<&ExtensionState> {
        self.scripts.iter().map(|script| &script.state).collect()
    }

    /// Cached output of a script by name, when loaded.
    pub fn output(&self, name: &str) -> Option<&ScriptOutput> {
        self.scripts
            .iter()
            .find(|script| script.state.name == name)
            .map(|script| &script.state.output)
    }

    /// Script names currently in the directory, sorted.
    fn dir_names(&self) -> Vec<String> {
        // No private extension dir (#49): load nothing, never a
        // relative path resolved against the working directory.
        if self.dir.as_os_str().is_empty() {
            return Vec::new();
        }
        let _ = fs::create_dir_all(&self.dir);
        let mut names = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some(SCRIPT_EXT) {
                    if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                        names.push(stem.to_owned());
                    }
                }
            }
        }
        names.sort();
        names
    }

    /// Source mtime of one script, when the file exists.
    fn script_mtime(&self, name: &str) -> Option<SystemTime> {
        fs::metadata(self.dir.join(format!("{name}.{SCRIPT_EXT}")))
            .and_then(|meta| meta.modified())
            .ok()
    }

    /// (Re)load every script in the directory, then run each enabled
    /// one once. A missing directory loads nothing successfully.
    pub fn load_dir(&mut self) {
        for name in self.dir_names() {
            let mtime = self.script_mtime(&name);
            let fresh = self
                .scripts
                .iter()
                .position(|script| script.state.name == name)
                .is_none_or(|index| self.scripts[index].state.mtime != mtime);
            if fresh {
                self.refresh(&name, mtime);
            }
        }
    }

    /// Poll the directory for additions, changes, and removals, then
    /// re-run every enabled script so outputs stay live. This is the
    /// hot-reload path, driven on the shell's update cadence.
    pub fn drive(&mut self) {
        let names = self.dir_names();
        let dir = self.dir.clone();
        self.scripts.retain(|script| {
            names.contains(&script.state.name)
                && dir
                    .join(format!("{}.{}", script.state.name, SCRIPT_EXT))
                    .exists()
        });
        for name in &names {
            let mtime = self.script_mtime(name);
            let known = self.scripts.iter().position(|s| s.state.name == *name);
            match known {
                Some(index) if self.scripts[index].state.mtime != mtime => {
                    self.refresh(name, mtime);
                }
                None => self.refresh(name, mtime),
                _ => {}
            }
        }
        for index in 0..self.scripts.len() {
            if self.scripts[index].state.enabled {
                Self::run_one(&self.engine, &self.sink, &mut self.scripts[index]);
            }
        }
    }

    /// Deliver a press action to the script that declared it. Scripts
    /// without a `press` function ignore it; a failing handler
    /// disables its script with a note.
    pub fn press(&mut self, id: &str) {
        for index in 0..self.scripts.len() {
            let declares = self.scripts[index].state.output.press_id.as_deref() == Some(id);
            if !declares || !self.scripts[index].state.enabled {
                continue;
            }
            let Some(ast) = self.scripts[index].ast.clone() else {
                continue;
            };
            let name = self.scripts[index].state.name.clone();
            let result: Result<(), Box<EvalAltResult>> =
                self.engine
                    .call_fn(&mut Scope::new(), &ast, PRESS_FN, (id.to_owned(),));
            match result {
                Ok(()) => {
                    if let Ok(sink) = self.sink.lock() {
                        let output = &mut self.scripts[index].state.output;
                        if sink.cell_text.is_some() {
                            output.cell_text = sink.cell_text.clone();
                        }
                        if sink.cell_icon.is_some() {
                            output.cell_icon = sink.cell_icon.clone();
                        }
                        if sink.press_id.is_some() {
                            output.press_id = sink.press_id.clone();
                        }
                        if sink.badge.is_some() {
                            output.badge = sink.badge.clone();
                        }
                        output.notices.extend(sink.notices.iter().cloned());
                    }
                }
                Err(error) => {
                    if matches!(*error, EvalAltResult::ErrorFunctionNotFound(..)) {
                        continue;
                    }
                    self.scripts[index].state.enabled = false;
                    self.scripts[index].state.note =
                        Some(format!("press handler for {name} failed: {error}"));
                }
            }
        }
    }

    /// Compile one script fresh (new or changed), replacing any known
    /// entry, and run it when it compiles.
    fn refresh(&mut self, name: &str, mtime: Option<SystemTime>) {
        let path = self.dir.join(format!("{name}.{SCRIPT_EXT}"));
        let (ast, state) = match fs::read_to_string(&path) {
            Ok(source) => match self.engine.compile(&source) {
                Ok(ast) => (
                    Some(ast),
                    ExtensionState {
                        name: name.to_owned(),
                        output: ScriptOutput::default(),
                        enabled: true,
                        note: None,
                        mtime,
                    },
                ),
                Err(error) => (
                    None,
                    ExtensionState {
                        name: name.to_owned(),
                        output: ScriptOutput::default(),
                        enabled: false,
                        note: Some(format!("parse error: {error}")),
                        mtime,
                    },
                ),
            },
            Err(error) => (
                None,
                ExtensionState {
                    name: name.to_owned(),
                    output: ScriptOutput::default(),
                    enabled: false,
                    note: Some(format!("unreadable: {error}")),
                    mtime,
                },
            ),
        };
        if let Some(index) = self.scripts.iter().position(|s| s.state.name == name) {
            self.scripts[index] = LoadedScript { state, ast };
        } else {
            self.scripts.push(LoadedScript { state, ast });
        }
        if let Some(index) = self.scripts.iter().position(|s| s.state.name == name) {
            if self.scripts[index].state.enabled {
                Self::run_one(&self.engine, &self.sink, &mut self.scripts[index]);
            }
        }
    }

    /// Run one enabled script, draining the sink into its cached
    /// output. Any failure disables the script with a note; the host
    /// itself is unaffected.
    fn run_one(engine: &Engine, sink: &Arc<Mutex<ApiSink>>, loaded: &mut LoadedScript) {
        if let Ok(mut sink) = sink.lock() {
            *sink = ApiSink::default();
        }
        let Some(ast) = loaded.ast.clone() else {
            return;
        };
        let name = loaded.state.name.clone();
        match engine.run_ast(&ast) {
            Ok(()) => {
                if let Ok(sink) = sink.lock() {
                    loaded.state.output = ScriptOutput {
                        cell_text: sink.cell_text.clone(),
                        cell_icon: sink.cell_icon.clone(),
                        press_id: sink.press_id.clone(),
                        badge: sink.badge.clone(),
                        notices: sink.notices.clone(),
                    };
                }
            }
            Err(error) => {
                loaded.state.enabled = false;
                // Name the hang bound explicitly: a progress-hook halt
                // carries the budget message as its value.
                let detail = match &*error {
                    EvalAltResult::ErrorTerminated(halt, _) => halt.to_string(),
                    other => other.to_string(),
                };
                loaded.state.note = Some(format!("{name} disabled: {detail}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// Script dir under a temp dir: no HOME or system side effects.
    fn script_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let scripts = dir.path().join("extensions");
        fs::create_dir_all(&scripts).expect("mkdir");
        (dir, scripts)
    }

    fn write_script(dir: &Path, name: &str, source: &str) {
        fs::write(dir.join(format!("{name}.{SCRIPT_EXT}")), source).expect("write");
    }

    /// The sample cell script contributes its bar cell on a clean host.
    #[test]
    fn sample_cell_contributes_bar_cell() {
        let (_guard, dir) = script_dir();
        write_script(
            &dir,
            "hello",
            "bar_cell(\"hello\", \"face-smile\");\non_press(\"hello-wave\");",
        );
        let mut host = ExtensionHost::new(dir);
        host.load_dir();
        let output = host.output("hello").expect("hello loaded");
        assert_eq!(output.cell_text.as_deref(), Some("hello"));
        assert_eq!(output.cell_icon.as_deref(), Some("face-smile"));
        assert_eq!(output.press_id.as_deref(), Some("hello-wave"));
        assert!(host.states().iter().all(|state| state.enabled));
    }

    /// A syntax error disables the script with a visible note; the
    /// host stays usable and other scripts keep running.
    #[test]
    fn syntax_error_disables_with_note() {
        let (_guard, dir) = script_dir();
        write_script(&dir, "good", "bar_cell(\"ok\", \"\");");
        write_script(&dir, "bad", "bar_cell((oops;");
        let mut host = ExtensionHost::new(dir);
        host.load_dir();
        let bad = host
            .states()
            .into_iter()
            .find(|state| state.name == "bad")
            .expect("bad known");
        assert!(!bad.enabled, "bad script disabled");
        assert!(
            bad.note.as_deref().is_some_and(|note| !note.is_empty()),
            "disable carries a visible note"
        );
        let good = host.output("good").expect("good loaded");
        assert_eq!(good.cell_text.as_deref(), Some("ok"));
    }

    /// Filesystem access does not exist for scripts: the call fails,
    /// the script disables with a note, and the host stays up.
    #[test]
    fn filesystem_access_denied_and_disabled() {
        let (_guard, dir) = script_dir();
        write_script(&dir, "good", "bar_cell(\"ok\", \"\");");
        write_script(&dir, "evil", "let f = open(\"/etc/passwd\");");
        let mut host = ExtensionHost::new(dir);
        host.load_dir();
        let evil = host
            .states()
            .into_iter()
            .find(|state| state.name == "evil")
            .expect("evil known");
        assert!(!evil.enabled, "filesystem script disabled");
        assert!(
            evil.note.as_deref().is_some_and(|note| !note.is_empty()),
            "disable carries a visible note"
        );
        assert_eq!(
            host.output("good")
                .expect("good loaded")
                .cell_text
                .as_deref(),
            Some("ok"),
            "host stays up for good scripts"
        );
    }

    /// Network and process spawning do not exist for scripts either.
    #[test]
    fn network_and_spawn_denied() {
        for (name, source) in [
            ("net", "let r = http_get(\"http://example.com\");"),
            ("spawn", "system(\"id\");"),
        ] {
            let (_guard, dir) = script_dir();
            write_script(&dir, name, source);
            let mut host = ExtensionHost::new(dir);
            host.load_dir();
            let state = host
                .states()
                .into_iter()
                .find(|state| state.name == name)
                .expect("script known");
            assert!(!state.enabled, "{name} disabled");
            assert!(
                state.note.as_deref().is_some_and(|note| !note.is_empty()),
                "{name} disable carries a note"
            );
        }
    }

    /// `import` from disk is disabled: the dummy module resolver
    /// refuses every import.
    #[test]
    fn import_from_disk_refused() {
        let (_guard, dir) = script_dir();
        write_script(&dir, "sneaky", "import \"/etc/hostname\" as h;");
        let mut host = ExtensionHost::new(dir);
        host.load_dir();
        let state = host
            .states()
            .into_iter()
            .find(|state| state.name == "sneaky")
            .expect("sneaky known");
        assert!(!state.enabled, "importing script disabled");
        assert!(
            state.note.as_deref().is_some_and(|note| !note.is_empty()),
            "disable carries a visible note"
        );
    }

    /// An infinite loop hits the operation budget and disables with
    /// a note instead of hanging the host.
    #[test]
    fn infinite_loop_budgeted_and_disabled() {
        let (_guard, dir) = script_dir();
        write_script(&dir, "loopy", "while true {}");
        let mut host = ExtensionHost::new(dir);
        host.load_dir();
        let state = host
            .states()
            .into_iter()
            .find(|state| state.name == "loopy")
            .expect("loopy known");
        assert!(!state.enabled, "looping script disabled");
        assert!(
            state
                .note
                .as_deref()
                .is_some_and(|note| note.contains("budget")),
            "disable names the budget: {:?}",
            state.note
        );
    }

    /// Press actions reach the script function that declared them.
    #[test]
    fn press_reaches_declaring_script() {
        let (_guard, dir) = script_dir();
        write_script(
            &dir,
            "hello",
            "bar_cell(\"hello\", \"\");\non_press(\"wave\");\nfn press(id) { notice(\"got \" + id); }",
        );
        let mut host = ExtensionHost::new(dir);
        host.load_dir();
        host.press("wave");
        let output = host.output("hello").expect("hello loaded");
        assert!(
            output.notices.iter().any(|note| note.contains("got wave")),
            "press handler ran: {:?}",
            output.notices
        );
    }
}
