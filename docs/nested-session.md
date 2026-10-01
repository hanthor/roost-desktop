# Nested session: launch, crash journey, and diagnostics

One command starts the 001 nested desktop slice: a supervised compositor
session with the shell panel attached.

```sh
scripts/roost-nested run [--socket NAME] [--shell-bin PATH] [--artifacts DIR]
```

Defaults: socket `roost-nested-<pid>`, shell binary from the workspace
build, artifacts under
`$XDG_STATE_HOME/roost-nested/<socket>` (else
`~/.local/state/roost-nested/<socket>`). The script builds both binaries,
starts `roost-compositor` on a private socket, and records
`nested.log` plus `compositor.pid` in the artifacts dir. Needs a host
Wayland/X session with EGL (llvmpipe is fine); headless CI cannot run
it — automation covers the same paths headless (see below).

## Crash and reconnect journey

1. Launch with `scripts/roost-nested run`. The nested window appears;
   the panel attaches as a top strip.
2. Open application clients inside the session (e.g.
   `WAYLAND_DISPLAY=<socket> <app>` from another terminal).
3. Kill the shell: `scripts/roost-nested kill-shell --socket <socket>`.
   Application windows stay mapped and the background shifts to deep
   red: the compositor-owned recovery overlay covers the session.
4. The compositor respawns the shell within its finite restart budget
   (bounded backoff visible in `nested.log` as `shell supervision`
   lines). The panel re-attaches and resyncs from a full snapshot —
   the exact window and workspace set, never partial state.
5. While the overlay is visible, keyboard input stays on the shield:
   Up/Down move the selection, Enter confirms, R relaunches the shell,
   Esc dismisses. If the restart budget is spent, the session stays
   calm and observable until an operator relaunch (R).

## Environment hygiene

`WAYLAND_DISPLAY` points at the private socket for the compositor
process and its shell child only. The parent shell and the host
session are never mutated; shutdown restores the previous value.
`ROOST_CONTROL_SOCKET` hands the shell child its control socket path
(overview feed); without it the panel runs with an empty overview.
Neither variable is exported by the launcher into your shell.

Host pointer grab/ungrab follows the winit backend: click into the
nested window to capture input; the host escape combination releases
it (see the winit backend docs for the exact key).

## Failure artifacts and redaction

Every supervision or control event is logged with codes, revisions,
and counts only. Window titles, activation tokens, and frame content
never enter logs: `RunArtifact` carries a content-free snapshot hash
instead of the window set, and control errors name categories, not
payloads. When reporting a failure, attach `nested.log` plus the
revision/restart counts from it — that is the complete artifact.

The headless proof for environments without EGL: `cargo test
--workspace` runs the 100-run fault harness
(`hundred_fault_runs_keep_apps_alive_and_resync`), which kills,
disconnects, stalls, crash-loops, and gap-forces shell peers and
asserts app survival, exact resync, budget exhaustion, and artifact
redaction on every run.

## Provisional visuals

The slice-1 overlay visual is the background shift plus the input
shield; full overlay text rendering (window list pixels) arrives with
shell workflow parity (002). The panel's 200 Hz control poll loop is
likewise provisional pacing for the shell binary only — compositor
input, focus, and frame paths never wait on it.
