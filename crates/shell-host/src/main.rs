// Wave 2 stream 3 owns this crate: supervised shell host (Activities
// trigger, window list) as a separate Wayland client. See the 001 spec
// R3/R5 and ADR 0003.
//
// The compositor spawns this binary as a supervised child with a
// nested-session WAYLAND_DISPLAY set for the child only (ADR 0003). The
// panel attaches to the compositor's layer-shell global; without it the
// binary reports that clearly instead of falling back to a misplaced
// surface role.

use std::process::ExitCode;

use roost_shell_host::panel::{run_panel_with_control, PanelConfig};

/// Release version stamped at build time: `ROOST_VERSION` (a `vX.Y.Z` tag or
/// plain `X.Y.Z`) wins, otherwise the crate version. Duplicated per binary on
/// purpose — no shared dependency for a few lines.
fn normalize_version<'a>(raw: Option<&'a str>, fallback: &'a str) -> &'a str {
    match raw {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => fallback,
    }
}

fn release_version() -> &'static str {
    normalize_version(option_env!("ROOST_VERSION"), env!("CARGO_PKG_VERSION"))
}

fn main() -> ExitCode {
    if std::env::args_os()
        .skip(1)
        .any(|arg| arg == "--version" || arg == "-V")
    {
        println!("roost-shell-host {}", release_version());
        return ExitCode::SUCCESS;
    }
    // The old name's environment, for one release (#505).
    roost_shell_control::legacy::import_env("roost-shell-host");
    // Set by the supervised compositor child recipe (ADR 0003); absent
    // when run by hand against any compositor.
    let control_path = std::env::var_os("ROOST_CONTROL_SOCKET").map(std::path::PathBuf::from);
    match run_panel_with_control(PanelConfig::default(), control_path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("roost-shell-host: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod version_tests {
    use super::normalize_version;

    #[test]
    fn tag_prefix_is_stripped() {
        assert_eq!(normalize_version(Some("v1.2.3"), "0.1.0"), "1.2.3");
    }

    #[test]
    fn plain_version_is_kept() {
        assert_eq!(normalize_version(Some("1.2.3"), "0.1.0"), "1.2.3");
    }

    #[test]
    fn missing_or_empty_falls_back() {
        assert_eq!(normalize_version(None, "0.1.0"), "0.1.0");
        assert_eq!(normalize_version(Some(""), "0.1.0"), "0.1.0");
    }
}
