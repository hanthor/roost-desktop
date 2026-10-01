# Roost extensions

Small Rhai scripts that extend the shell without rebuilding it: bar
cells, dock badges, and user-visible notes.

## Install

Copy a `.rhai` file into the extension directory
(`~/.local/share/roost/extensions/` by default; follows
`XDG_DATA_HOME`). The shell loads it on the next update pass — no
restart. Editing or deleting the file applies the same way.

## API (allowlist — nothing else is reachable)

- `bar_cell(text, icon)` — contribute the strip cell (text plus an
  icon name from the theme).
- `on_press(id)` — declare the press action id for the cell.
- `press(id)` — optional script function the shell calls on press.
- `dock_badge(text)` — contribute the dock badge text.
- `notice(text)` — record a user-visible note.
- `print(text)` — captured into notes (never reaches stdout).

Scripts run on the slow status tick with an operation budget, off
the paint and event paths; painters read cached outputs only, and
strip presses queue into handlers instead of running inline. Anything
else — filesystem, network, processes, `import` from disk — does not
exist for scripts. A script that fails (parse error, runtime error,
budget exceeded) is disabled with a visible note naming the reason;
fix the file and the shell reloads it.

## Example

See `crates/shell-host/sample-extensions/hello_cell.rhai`: a greeting
cell that notes its own presses.
