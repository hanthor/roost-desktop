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

use rwd_shell_host::panel::{run_panel_with_control, PanelConfig};

fn main() -> ExitCode {
    // Set by the supervised compositor child recipe (ADR 0003); absent
    // when run by hand against any compositor.
    let control_path = std::env::var_os("RWD_CONTROL_SOCKET").map(std::path::PathBuf::from);
    match run_panel_with_control(PanelConfig::default(), control_path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("rwd-shell-host: {err}");
            ExitCode::FAILURE
        }
    }
}
