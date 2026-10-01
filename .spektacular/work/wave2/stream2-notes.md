# Wave 2 Stream 2 notes — revisioned state model (`state.rs`)

Survey sources (read-only shallow clones under `/tmp`; GPL — patterns only,
no verbatim code; implementation is original):

- `/tmp/niri-survey` — niri, commit as cloned 2026-09-28 (`git clone --depth 1`)
- `/tmp/cosmic-survey` — cosmic-comp, commit as cloned 2026-09-28

## niri — how windows / workspaces / focus are modeled

- `src/layout/mod.rs:343` — `pub struct Layout<W: LayoutElement>` holds
  `monitor_set: MonitorSet`, `is_active: bool`,
  `last_active_workspace_id: HashMap<String, WorkspaceId>` (restores the
  active workspace on monitor reconnect), plus interactive-move/dnd state.
- `src/layout/monitor.rs:50` — `pub struct Monitor` holds
  `workspaces: Vec<Workspace<W>>` (always non-empty) and
  `active_workspace_idx: usize` (`:69`), with
  `active_workspace_ref()` (`:375`) / `active_workspace()` (`:395`) accessors
  and index fix-ups on insert/remove (`:423`, `:458`, `:665`).
- `src/layout/workspace.rs:47` — `pub struct Workspace` holds
  `scrolling: ScrollingSpace` + `floating: FloatingSpace`, `output: Option<Output>`,
  `id: WorkspaceId`, `name: Option<String>` (trailing fields).
- `src/utils/id.rs:6` — `pub struct IdCounter` (atomic u64, starts at 1);
  `:21` `next()` via `fetch_add` — never reused, monotonic. Our `next_id`
  counter follows this pattern (plain `u64`, session-scoped).
- `src/window/mapped.rs:51` — `pub struct Mapped` with `:92` `is_focused: bool`,
  `:370` `is_focused()`, `:390` `set_is_focused()` — focus is a per-window
  flag set by the layout, mirrored in our `WindowEntry.focused` +
  `StateModel.focused`.
- Titles: no length cap found in niri (`src/window/mapped.rs` title handling
  recomputes rules on change, `:68`); the cap (`MAX_TITLE_LEN = 256`) is our
  own IPC-hygiene addition for untrusted client strings, per ADR 0002
  "untrusted titles".

## cosmic-comp — how windows / workspaces / focus are modeled

- `src/shell/mod.rs:278` — `pub struct Shell` owns `workspaces: Workspaces`,
  pending windows/layers/activations, seats, session lock.
- `src/shell/mod.rs:370` — `pub struct WorkspaceSet` with `active: usize`
  (index of the active workspace) + `workspaces: Vec<Workspace>`,
  `sticky_layer`, `minimized_windows`.
- `src/shell/mod.rs:839` — `pub struct Workspaces` with
  `sets: IndexMap<Output, WorkspaceSet>` — per-output workspace sets.
- `src/shell/workspace.rs:104` — `pub struct Workspace` with `:116`
  `focus_stack: FocusStacks`, `:108` `minimized_windows`; `:503`
  `refresh_focus_stack()` reconciles focus against live windows.
- `src/shell/focus/mod.rs:41` — `pub enum FocusTarget`; `:123` `FocusStack`,
  `:168` `ActiveFocus`, `:198` `set_focus()` — focus is a per-seat stack,
  last element wins. Our single `focused: Option<u64>` is the minimal
  single-seat slice-1 analogue.
- `src/shell/element/mod.rs:96` — `pub struct CosmicMapped`; `:413`
  `is_activated()` — activation is a method over window state, analogous to
  our `active` hint flag.
- Titles: `src/shell/element/window.rs:98` caches `last_title: Mutex<String>`
  (`:966` refresh on change) — titles are live client strings; again no
  upstream cap, ours is new.

## Decisions for `state.rs` (mechanical mapping for the schema stream)

- Plain types only: `u64` revision/ids, `String` titles, std
  `BTreeMap`/`VecDeque` (no `IndexMap` dependency — `Cargo.toml` is owned by
  another stream and must stay untouched; `BTreeMap` iteration is id-ordered,
  which snapshots rely on).
- `WindowEntry { id, title, workspace: u32, active: bool, focused: bool }` —
  both flags kept; `focused` mirrors `StateModel.focused`, `active` is
  compositor-controlled via `WindowUpdate`.
- `StateModel { revision, windows, workspaces: Vec<u32> (sorted/deduped),
  focused: Option<u64>, next_id, changes: VecDeque<(u64, StateChange)> }`.
- One revision bump per successful mutation; failed ops (unknown id) are
  no-ops with no bump. `remove` of the focused window appends an extra
  `FocusChanged` under the same revision.
- `changes_since(rev)`: entries with change-rev > `rev`; `Gap{ expected:
  current revision, got: requested }` when `rev` is newer than current or
  older than the retained window (`MAX_CHANGE_LOG = 1024`, prune oldest).
- `TokenPolicy::validate(issued_at_ms, now_ms, max_age_ms = 30_000 default,
  seat_ok, app_ok) -> Allow | Deny{Expired|SeatMismatch|AppMismatch}` —
  pure; future-dated tokens deny as `Expired`; one-use removal is caller-side
  (documented on the type).
