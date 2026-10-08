//! One-release compatibility with the project's old name (#505).
//! tuna-rename: keep-file (the old names below are the point of it).
//!
//! Tuna Desktop used to be called Roost. For one release after the
//! rename, two kinds of old state keep working:
//!
//! - **Environment.** `ROOST_*` variables are still honoured. Each
//!   binary calls [`import_env`] at start-up: every `ROOST_X` is copied
//!   to `TUNA_X` (a `TUNA_X` that is already set wins), the old names
//!   are dropped so child processes do not warn again, and one
//!   deprecation line goes to stderr.
//! - **Per-user directories.** Saved monitor layouts, favorites,
//!   notification history, prefs and extensions lived under `roost` and
//!   `roost-shell` in the XDG base directories. [`adopt_dirs`] links each
//!   new name to the old directory when only the old one exists, so the
//!   state carries over and a rollback to the old release still sees
//!   everything written since.
//!
//! Remove this module (and its callers) one release after the rename.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Prefix of the deprecated environment variables.
pub const LEGACY_ENV_PREFIX: &str = "ROOST_";
/// Prefix of the current environment variables.
pub const ENV_PREFIX: &str = "TUNA_";

/// What [`import_env`] does to an environment.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EnvImport {
    /// `TUNA_*` names to set, with the value of their `ROOST_*` twin.
    pub set: Vec<(String, OsString)>,
    /// Every `ROOST_*` name present, sorted; all of them are removed.
    pub legacy: Vec<String>,
}

/// Plan the import for `vars` (pure, for tests): each `ROOST_X` becomes
/// `TUNA_X` unless `TUNA_X` is already present.
pub fn plan_env_import<I, K, V>(vars: I) -> EnvImport
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let vars: Vec<(OsString, OsString)> = vars
        .into_iter()
        .map(|(k, v)| (k.as_ref().to_owned(), v.as_ref().to_owned()))
        .collect();
    let mut plan = EnvImport::default();
    for (key, value) in &vars {
        let Some(rest) = key.to_str().and_then(|k| k.strip_prefix(LEGACY_ENV_PREFIX)) else {
            continue;
        };
        plan.legacy.push(format!("{LEGACY_ENV_PREFIX}{rest}"));
        let new = format!("{ENV_PREFIX}{rest}");
        if !vars.iter().any(|(k, _)| k.as_os_str() == OsStr::new(&new)) {
            plan.set.push((new, value.clone()));
        }
    }
    plan.legacy.sort();
    plan.set.sort();
    plan
}

/// The deprecation line for `legacy` names, or `None` when there are none.
pub fn deprecation_line(program: &str, legacy: &[String]) -> Option<String> {
    if legacy.is_empty() {
        return None;
    }
    Some(format!(
        "{program}: deprecated: {} read as {ENV_PREFIX}*; the {LEGACY_ENV_PREFIX}* names stop working in the next release",
        legacy.join(", ")
    ))
}

/// Copy `ROOST_*` variables to their `TUNA_*` names in this process and
/// log one deprecation line. Call first thing in `main`, before any
/// thread starts (it mutates the process environment).
pub fn import_env(program: &str) {
    let plan = plan_env_import(std::env::vars_os());
    for (key, value) in &plan.set {
        std::env::set_var(key, value);
    }
    for key in &plan.legacy {
        std::env::remove_var(key);
    }
    if let Some(line) = deprecation_line(program, &plan.legacy) {
        eprintln!("{line}");
    }
}

/// (old, new) entry names inside one base directory.
type Renames = &'static [(&'static str, &'static str)];

/// Old and new directory names under each XDG base directory: the base's
/// variable, its default under `$HOME`, and the renamed entries.
const DIRS: &[(&str, &str, Renames)] = &[
    ("XDG_CONFIG_HOME", ".config", &[("roost", "tuna")]),
    ("XDG_CACHE_HOME", ".cache", &[("roost", "tuna")]),
    (
        "XDG_DATA_HOME",
        ".local/share",
        &[("roost", "tuna"), ("roost-shell", "tuna-shell")],
    ),
    (
        "XDG_STATE_HOME",
        ".local/state",
        &[("roost-shell", "tuna-shell")],
    ),
];

/// The (old, new) directory pairs for an environment read through `var`.
pub fn legacy_dirs(var: impl Fn(&str) -> Option<OsString>) -> Vec<(PathBuf, PathBuf)> {
    let home = var("HOME").map(PathBuf::from).filter(|h| h.is_absolute());
    let mut out = Vec::new();
    for (key, default, names) in DIRS {
        let base = var(key)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.as_ref().map(|h| h.join(default)));
        let Some(base) = base else { continue };
        for (old, new) in *names {
            out.push((base.join(old), base.join(new)));
        }
    }
    out
}

/// Link `new` to `old` when `old` is a directory and nothing is at
/// `new`. Returns whether a link was made. The link is relative (both
/// live in one base directory), so moving the base keeps it valid.
pub fn adopt_dir(old: &Path, new: &Path) -> std::io::Result<bool> {
    if !old.is_dir() || new.symlink_metadata().is_ok() {
        return Ok(false);
    }
    let target = old.file_name().map(Path::new).unwrap_or(old);
    std::os::unix::fs::symlink(target, new)?;
    Ok(true)
}

/// Adopt every old per-user directory of this process's environment;
/// failures are logged, never fatal.
pub fn adopt_dirs(program: &str) {
    for (old, new) in legacy_dirs(|key| std::env::var_os(key)) {
        match adopt_dir(&old, &new) {
            Ok(true) => eprintln!(
                "{program}: using {} (from the old name) as {}",
                old.display(),
                new.display()
            ),
            Ok(false) => {}
            Err(err) => eprintln!(
                "{program}: cannot link {} to {}: {err}",
                new.display(),
                old.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    #[test]
    fn legacy_env_vars_fall_back_to_their_new_names() {
        let plan = plan_env_import(env(&[
            ("ROOST_SCALE", "1.5"),
            ("PATH", "/usr/bin"),
            ("ROOST_CONTROL_SOCKET", "/run/old.sock"),
        ]));
        assert_eq!(
            plan.set,
            vec![
                (
                    "TUNA_CONTROL_SOCKET".to_owned(),
                    OsString::from("/run/old.sock")
                ),
                ("TUNA_SCALE".to_owned(), OsString::from("1.5")),
            ]
        );
        assert_eq!(plan.legacy, vec!["ROOST_CONTROL_SOCKET", "ROOST_SCALE"]);
    }

    #[test]
    fn new_env_name_wins_over_the_legacy_one() {
        let plan = plan_env_import(env(&[("ROOST_SCALE", "1.5"), ("TUNA_SCALE", "2")]));
        assert!(plan.set.is_empty());
        assert_eq!(
            plan.legacy,
            vec!["ROOST_SCALE"],
            "still dropped and warned about"
        );
    }

    #[test]
    fn no_legacy_env_means_no_change_and_no_warning() {
        let plan = plan_env_import(env(&[("TUNA_SCALE", "2"), ("ROOSTER", "x")]));
        assert_eq!(plan, EnvImport::default());
        assert_eq!(deprecation_line("tuna-compositor", &plan.legacy), None);
    }

    #[test]
    fn deprecation_is_one_line_naming_every_legacy_variable() {
        let line = deprecation_line(
            "tuna-compositor",
            &["ROOST_A".to_owned(), "ROOST_B".to_owned()],
        )
        .unwrap();
        assert!(!line.contains('\n'));
        assert!(line.starts_with("tuna-compositor: deprecated: ROOST_A, ROOST_B read as TUNA_*"));
    }

    #[test]
    fn legacy_dirs_follow_xdg_and_home_defaults() {
        let dirs = legacy_dirs(|key| match key {
            "HOME" => Some("/home/u".into()),
            "XDG_CONFIG_HOME" => Some("/cfg".into()),
            "XDG_STATE_HOME" => Some("relative/ignored".into()),
            _ => None,
        });
        assert!(dirs.contains(&("/cfg/roost".into(), "/cfg/tuna".into())));
        assert!(dirs.contains(&("/home/u/.cache/roost".into(), "/home/u/.cache/tuna".into())));
        assert!(dirs.contains(&(
            "/home/u/.local/share/roost-shell".into(),
            "/home/u/.local/share/tuna-shell".into()
        )));
        assert!(dirs.contains(&(
            "/home/u/.local/state/roost-shell".into(),
            "/home/u/.local/state/tuna-shell".into()
        )));
        assert!(legacy_dirs(|_| None).is_empty());
    }

    #[test]
    fn old_dir_is_adopted_once_and_an_existing_new_dir_is_left_alone() {
        let base = std::env::temp_dir().join(format!("tuna-legacy-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("roost")).unwrap();
        std::fs::write(base.join("roost/monitors.xml"), "<monitors/>").unwrap();

        assert!(adopt_dir(&base.join("roost"), &base.join("tuna")).unwrap());
        assert_eq!(
            std::fs::read_to_string(base.join("tuna/monitors.xml")).unwrap(),
            "<monitors/>"
        );
        assert_eq!(
            std::fs::read_link(base.join("tuna")).unwrap(),
            Path::new("roost")
        );
        assert!(
            !adopt_dir(&base.join("roost"), &base.join("tuna")).unwrap(),
            "second run"
        );

        std::fs::create_dir_all(base.join("roost-shell")).unwrap();
        std::fs::create_dir_all(base.join("tuna-shell")).unwrap();
        assert!(!adopt_dir(&base.join("roost-shell"), &base.join("tuna-shell")).unwrap());
        assert!(!base.join("tuna-shell").is_symlink());
        assert!(!adopt_dir(&base.join("missing"), &base.join("new")).unwrap());
        assert!(!base.join("new").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
