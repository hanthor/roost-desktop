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
/// client-provided text; `active` marks the focused/selected window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowEntry {
    /// Compositor-issued opaque window id.
    pub id: u64,
    /// Untrusted client-provided title.
    pub title: String,
    /// Whether this window is the currently active one.
    pub active: bool,
}

impl WindowEntry {
    /// Build a window entry with the given id, title, and active flag.
    pub fn new(id: u64, title: impl Into<String>, active: bool) -> Self {
        Self {
            id,
            title: title.into(),
            active,
        }
    }
}

/// Shell-side view state: window list, workspace set, overview flag.
///
/// Rendering reads this; the (later) control-protocol adapter writes it.
/// Selection is single: at most one window is `active` at a time.
#[derive(Debug, Default)]
pub struct ShellModel {
    windows: Vec<WindowEntry>,
    workspaces: Vec<u32>,
    overview_open: bool,
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
        true
    }

    /// Flip the overview open/closed state, returning the new state.
    ///
    /// Toggling is local UI state and works with any window list,
    /// including an empty one.
    pub fn toggle_overview(&mut self) -> bool {
        self.overview_open = !self.overview_open;
        self.overview_open
    }
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
}
