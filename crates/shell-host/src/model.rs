//! Shell-side view model for the supervised shell host.
//!
//! Plain types only: this module depends on nothing outside `std` and
//! deliberately does not depend on any other workspace crate.
//!
//! Merge seam (later work): the control-protocol wiring that feeds this
//! model — `Hello` / `Snapshot` / ordered state changes from the
//! compositor, and activation requests back through compositor policy —
//! is owned by another stream (001 spec R4, ADR 0002). When that schema
//! lands, a small adapter will translate its snapshot/change types into
//! [`ShellModel::apply_window_list`] calls and its activation results
//! into [`ShellModel::select_window`] calls. Nothing in this file assumes
//! that wire format.

/// One window as the shell shows it in the overview list.
///
/// `id` is the compositor-issued opaque window id (never reused during a
/// compositor session per the 001 spec); `title` is untrusted
/// client-provided text; `active` marks the focused/selected window;
/// `workspace` scopes overview rendering to the active workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowEntry {
    /// Compositor-issued opaque window id.
    pub id: u64,
    /// Untrusted client-provided title.
    pub title: String,
    /// Whether this window is the currently active one.
    pub active: bool,
    /// Workspace this window belongs to (0 when unknown).
    pub workspace: u32,
}

impl WindowEntry {
    /// Build a window entry with the given id, title, and active flag.
    pub fn new(id: u64, title: impl Into<String>, active: bool) -> Self {
        Self {
            id,
            title: title.into(),
            active,
            workspace: 0,
        }
    }

    /// Attach the workspace, for overview filtering.
    pub fn with_workspace(mut self, workspace: u32) -> Self {
        self.workspace = workspace;
        self
    }
}

/// Shell-side view state: window list, workspace set, overview flag.
///
/// Rendering reads this; the (later) control-protocol adapter writes it.
/// Selection is single: at most one window is `active` at a time.
///
/// MRU order derives here from `active` transitions (002 workspaces):
/// every selection and snapshot application refreshes it, and Alt-Tab
/// renders from it — the compositor never sends recency, it only sends
/// the switcher drive events.
#[derive(Debug, Default)]
pub struct ShellModel {
    windows: Vec<WindowEntry>,
    workspaces: Vec<u32>,
    active_workspace: u32,
    overview_open: bool,
    /// Most-recently-active window ids, front = most recent. Holds only
    /// known ids; refreshed on selection and on every list replacement.
    mru: Vec<u64>,
    /// Alt-Tab switcher overlay: open while Alt is held.
    switcher_open: bool,
    /// Index into [`mru`](Self::mru_order) of the switcher selection.
    switcher_index: usize,
}

impl ShellModel {
    /// Empty model: no windows, no workspaces, overview closed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current window list in display order.
    pub fn windows(&self) -> &[WindowEntry] {
        &self.windows
    }

    /// Known workspace ids, sorted and deduplicated.
    pub fn workspaces(&self) -> &[u32] {
        &self.workspaces
    }

    /// Active workspace id scoping overview rendering.
    pub fn active_workspace(&self) -> u32 {
        self.active_workspace
    }

    /// Track the compositor's active workspace (snapshot application
    /// and exclusive `WorkspaceChanged` ops converge here).
    pub fn set_active_workspace(&mut self, workspace: u32) {
        self.active_workspace = workspace;
    }

    /// Whether the overview is currently open.
    pub fn is_overview_open(&self) -> bool {
        self.overview_open
    }

    /// Id of the active window, if any.
    pub fn selected(&self) -> Option<u64> {
        self.windows.iter().find(|w| w.active).map(|w| w.id)
    }

    /// Replace the window list and workspace set (snapshot application).
    ///
    /// The list is stored in the given order. If more than one entry
    /// arrives marked active, the first one wins and the rest are cleared,
    /// keeping the single-selection invariant. A snapshot never changes
    /// the overview flag: opening/closing the overview is local UI state.
    pub fn apply_window_list(&mut self, windows: Vec<WindowEntry>, workspaces: Vec<u32>) {
        let mut seen_active = false;
        self.windows = windows
            .into_iter()
            .map(|mut entry| {
                if entry.active {
                    if seen_active {
                        entry.active = false;
                    } else {
                        seen_active = true;
                    }
                }
                entry
            })
            .collect();
        let mut sorted = workspaces;
        sorted.sort_unstable();
        sorted.dedup();
        self.workspaces = sorted;
        self.refresh_mru();
    }

    /// Select a window by id, marking it active and clearing the others.
    ///
    /// Returns `true` when the id names a known window. Returns `false`
    /// and changes nothing when the list is empty or the id is unknown.
    pub fn select_window(&mut self, id: u64) -> bool {
        if !self.windows.iter().any(|w| w.id == id) {
            return false;
        }
        for window in &mut self.windows {
            window.active = window.id == id;
        }
        self.touch_mru(id);
        true
    }

    /// Set the overview flag from a compositor intent (`Handled::Overview`).
    ///
    /// Preserves window order and selection; a selected id survives when
    /// it is still present.
    pub fn set_overview_open(&mut self, open: bool) {
        self.overview_open = open;
    }

    /// Flip the overview open/closed state, returning the new state.
    ///
    /// Toggling is local UI state and works with any window list,
    /// including an empty one.
    pub fn toggle_overview(&mut self) -> bool {
        self.overview_open = !self.overview_open;
        self.overview_open
    }

    /// Replace state from a plain snapshot view.
    ///
    /// Same semantics as [`ShellModel::apply_window_list`]: order kept,
    /// first-active-wins, overview flag untouched. The view's active
    /// workspace scopes overview rendering. This is the method the
    /// control-protocol adapter (`control.rs`) calls after mapping the
    /// schema `Snapshot` into a [`SnapshotView`].
    pub fn apply_snapshot_view(&mut self, snapshot: SnapshotView) {
        self.apply_window_list(snapshot.windows, snapshot.workspaces);
        self.set_active_workspace(snapshot.active_workspace);
    }

    /// Most-recently-active window ids, front = most recent. The
    /// Alt-Tab switcher renders and steps through this order.
    pub fn mru_order(&self) -> &[u64] {
        &self.mru
    }

    /// Whether the Alt-Tab switcher overlay is open.
    pub fn is_switcher_open(&self) -> bool {
        self.switcher_open
    }

    /// Current switcher selection, if the switcher is open and nonempty.
    pub fn switcher_selection(&self) -> Option<u64> {
        self.switcher_open
            .then(|| self.mru.get(self.switcher_index).copied())
            .flatten()
    }

    /// Move `id` to the MRU front (no-op for unknown ids).
    fn touch_mru(&mut self, id: u64) {
        if !self.windows.iter().any(|w| w.id == id) {
            return;
        }
        self.mru.retain(|known| *known != id);
        self.mru.insert(0, id);
    }

    /// Rebuild MRU after a list replacement: survivors keep their
    /// relative order, new ids arrive in list order at the front, and
    /// the selected window goes first. An open switcher re-clamps to
    /// the new list, closing when nothing remains.
    fn refresh_mru(&mut self) {
        self.mru
            .retain(|id| self.windows.iter().any(|w| w.id == *id));
        for window in self.windows.iter().rev() {
            if !self.mru.contains(&window.id) {
                self.mru.insert(0, window.id);
            }
        }
        if let Some(selected) = self.selected() {
            self.touch_mru(selected);
        }
        if self.switcher_open {
            if self.mru.is_empty() {
                self.switcher_open = false;
                self.switcher_index = 0;
            } else {
                self.switcher_index = self.switcher_index.min(self.mru.len() - 1);
            }
        }
    }

    /// Advance the switcher selection, opening it on first step. The
    /// first step lands past the current window (`mru[1]`); further
    /// steps wrap around. Returns the new selection, or `None` with no
    /// windows (the switcher stays closed).
    pub fn switcher_step(&mut self, forward: bool) -> Option<u64> {
        if self.mru.is_empty() {
            return None;
        }
        if !self.switcher_open {
            self.switcher_open = true;
            self.switcher_index = if self.mru.len() >= 2 { 1 } else { 0 };
        } else if forward {
            self.switcher_index = (self.switcher_index + 1) % self.mru.len();
        } else {
            self.switcher_index = self
                .switcher_index
                .checked_sub(1)
                .unwrap_or(self.mru.len() - 1);
        }
        self.mru.get(self.switcher_index).copied()
    }

    /// Commit the switcher: close it and return the selection for the
    /// caller to activate. Returns `None` when closed or empty.
    pub fn switcher_commit(&mut self) -> Option<u64> {
        let selection = self.switcher_selection();
        self.switcher_open = false;
        self.switcher_index = 0;
        selection
    }

    /// Cancel the switcher without activating.
    pub fn switcher_cancel(&mut self) {
        self.switcher_open = false;
        self.switcher_index = 0;
    }

    /// Mirror another model's switcher overlay (host sync path, after
    /// the window list already matches): open with the same selection
    /// when it names a known window, else close.
    pub fn apply_switcher_state(&mut self, open: bool, selection: Option<u64>) {
        match (open, selection) {
            (true, Some(id)) => {
                if let Some(index) = self.mru.iter().position(|known| *known == id) {
                    self.switcher_open = true;
                    self.switcher_index = index;
                } else {
                    self.switcher_cancel();
                }
            }
            _ => self.switcher_cancel(),
        }
    }
}

/// Plain snapshot payload owned by `shell-host` (no dependency on any
/// other workspace crate).
///
/// Schema-mapping seam: `control.rs` translates the protocol's `Snapshot
/// { windows, workspaces }` into this view (`WindowInfo.focused` maps to
/// [`WindowEntry::active`]; workspace ids narrow from the schema's `u64`
/// to the `u32` ids this model keeps). This file never imports
/// `roost-shell-control`, so the wire format can evolve without churning
/// the view model.
#[derive(Debug, Clone, Default)]
pub struct SnapshotView {
    /// Window list in display order.
    pub windows: Vec<WindowEntry>,
    /// Workspace ids (sorted and deduped on apply).
    pub workspaces: Vec<u32>,
    /// Active workspace id scoping overview rendering.
    pub active_workspace: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_windows() -> Vec<WindowEntry> {
        vec![
            WindowEntry::new(1, "Terminal", true),
            WindowEntry::new(2, "Browser", false),
        ]
    }

    #[test]
    fn select_known_window_moves_active_flag() {
        let mut model = ShellModel::new();
        model.apply_window_list(two_windows(), vec![0]);
        assert!(model.select_window(2));
        assert_eq!(model.selected(), Some(2));
        assert_eq!(
            model.windows().iter().filter(|w| w.active).count(),
            1,
            "exactly one window stays active"
        );
    }

    #[test]
    fn select_on_empty_list_returns_false_and_changes_nothing() {
        let mut model = ShellModel::new();
        assert!(!model.select_window(1));
        assert_eq!(model.selected(), None);
        assert!(model.windows().is_empty());
    }

    #[test]
    fn select_unknown_id_returns_false_and_keeps_selection() {
        let mut model = ShellModel::new();
        model.apply_window_list(two_windows(), vec![0]);
        assert!(!model.select_window(99));
        assert_eq!(model.selected(), Some(1));
        assert!(model.windows()[0].active);
    }

    #[test]
    fn apply_replaces_list_and_normalizes_multiple_active() {
        let mut model = ShellModel::new();
        model.apply_window_list(two_windows(), vec![0]);
        model.apply_window_list(
            vec![
                WindowEntry::new(7, "A", true),
                WindowEntry::new(8, "B", true),
                WindowEntry::new(9, "C", false),
            ],
            vec![1, 0, 1],
        );
        assert_eq!(model.selected(), Some(7), "first active entry wins");
        assert_eq!(model.workspaces(), &[0, 1], "workspaces sorted and deduped");
    }

    #[test]
    fn apply_empty_list_clears_windows_and_selection() {
        let mut model = ShellModel::new();
        model.apply_window_list(two_windows(), vec![0]);
        model.apply_window_list(Vec::new(), Vec::new());
        assert!(model.windows().is_empty());
        assert_eq!(model.selected(), None);
        assert!(model.workspaces().is_empty());
    }

    #[test]
    fn apply_snapshot_does_not_touch_overview_flag() {
        let mut model = ShellModel::new();
        model.toggle_overview();
        model.apply_window_list(two_windows(), vec![0]);
        assert!(model.is_overview_open());
    }

    #[test]
    fn toggle_overview_flips_with_no_windows() {
        let mut model = ShellModel::new();
        assert!(model.windows().is_empty());
        assert!(model.toggle_overview());
        assert!(model.is_overview_open());
        assert!(!model.toggle_overview());
        assert!(!model.is_overview_open());
    }

    #[test]
    fn toggle_overview_preserves_selection() {
        let mut model = ShellModel::new();
        model.apply_window_list(two_windows(), vec![0]);
        model.toggle_overview();
        model.toggle_overview();
        assert_eq!(model.selected(), Some(1));
    }

    #[test]
    fn apply_snapshot_view_scopes_active_workspace() {
        let mut model = ShellModel::new();
        model.apply_snapshot_view(SnapshotView {
            windows: vec![
                WindowEntry::new(1, "alpha", false).with_workspace(0),
                WindowEntry::new(2, "beta", true).with_workspace(1),
            ],
            workspaces: vec![1, 0, 1],
            active_workspace: 1,
        });
        assert_eq!(model.active_workspace(), 1);
        assert_eq!(model.workspaces(), &[0, 1]);
        assert_eq!(model.selected(), Some(2));
        model.set_active_workspace(0);
        assert_eq!(model.active_workspace(), 0);
    }

    #[test]
    fn mru_tracks_selection_churn_most_recent_first() {
        let mut model = ShellModel::new();
        model.apply_window_list(
            vec![
                WindowEntry::new(1, "a", true),
                WindowEntry::new(2, "b", false),
                WindowEntry::new(3, "c", false),
            ],
            vec![0],
        );
        // Snapshot order with the selected window first.
        assert_eq!(model.mru_order(), &[1, 2, 3]);
        assert!(model.select_window(3));
        assert_eq!(model.mru_order(), &[3, 1, 2]);
        assert!(model.select_window(2));
        assert_eq!(model.mru_order(), &[2, 3, 1]);
        // Unknown selections change nothing.
        assert!(!model.select_window(99));
        assert_eq!(model.mru_order(), &[2, 3, 1]);
    }

    #[test]
    fn mru_survives_resync_with_relative_order() {
        let mut model = ShellModel::new();
        model.apply_window_list(
            vec![
                WindowEntry::new(1, "a", false),
                WindowEntry::new(2, "b", false),
                WindowEntry::new(3, "c", true),
            ],
            vec![0],
        );
        assert!(model.select_window(2));
        assert_eq!(model.mru_order(), &[2, 3, 1]);
        // Window 3 closes; survivors keep order, newcomer 4 fronts.
        model.apply_window_list(
            vec![
                WindowEntry::new(4, "d", true),
                WindowEntry::new(1, "a", false),
                WindowEntry::new(2, "b", false),
            ],
            vec![0],
        );
        assert_eq!(model.mru_order(), &[4, 2, 1]);
    }

    #[test]
    fn switcher_steps_from_previous_and_wraps() {
        let mut model = ShellModel::new();
        model.apply_window_list(two_windows(), vec![0]);
        assert!(!model.is_switcher_open());
        // First step skips the current window (id 1).
        assert_eq!(model.switcher_step(true), Some(2));
        assert!(model.is_switcher_open());
        assert_eq!(model.switcher_selection(), Some(2));
        // Wrap around both directions.
        assert_eq!(model.switcher_step(true), Some(1));
        assert_eq!(model.switcher_step(true), Some(2));
        assert_eq!(model.switcher_step(false), Some(1));
        // Commit closes and yields the selection for activation.
        assert_eq!(model.switcher_commit(), Some(1));
        assert!(!model.is_switcher_open());
        assert_eq!(model.switcher_selection(), None);
        // Commit while closed yields nothing.
        assert_eq!(model.switcher_commit(), None);
    }

    #[test]
    fn switcher_cancel_closes_without_selection() {
        let mut model = ShellModel::new();
        model.apply_window_list(two_windows(), vec![0]);
        assert_eq!(model.switcher_step(true), Some(2));
        model.switcher_cancel();
        assert!(!model.is_switcher_open());
        assert_eq!(model.switcher_selection(), None);
    }

    #[test]
    fn switcher_stays_closed_with_no_windows() {
        let mut model = ShellModel::new();
        assert_eq!(model.switcher_step(true), None);
        assert!(!model.is_switcher_open());
    }

    #[test]
    fn set_overview_open_from_compositor_intent_preserves_state() {
        let mut model = ShellModel::new();
        model.apply_window_list(two_windows(), vec![0]);
        model.set_overview_open(true);
        assert!(model.is_overview_open());
        assert_eq!(model.selected(), Some(1));
        assert_eq!(model.windows().len(), 2);
        model.set_overview_open(false);
        assert!(!model.is_overview_open());
        assert_eq!(model.selected(), Some(1));
    }
}
