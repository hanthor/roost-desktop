// Wave 2 stream 3 owns this crate: supervised shell host (Activities
// trigger, window list) as a separate Wayland client. See the 001 spec
// R3/R5 and ADR 0003.
//
// The compositor spawns this binary as a supervised child with a
// nested-session WAYLAND_DISPLAY set for the child only (ADR 0003). Live
// runtime against our test compositor is DEFERRED: it has no layer-shell
// yet, so the binary reports that clearly instead of falling back to a
// misplaced surface role.

use std::process::ExitCode;

use rwd_shell_host::panel::{run_panel, PanelConfig};

fn main() -> ExitCode {
    match run_panel(PanelConfig::default()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("rwd-shell-host: {err}");
            ExitCode::FAILURE
        }
    }
}
