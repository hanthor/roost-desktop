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
    /// Client-supplied application id, when the compositor knows it.
    /// The dock matches running windows to desktop entries on this;
    /// `None` falls back to title matching.
    pub app_id: Option<String>,
}

impl WindowEntry {
    /// Build a window entry with the given id, title, and active flag.
    pub fn new(id: u64, title: impl Into<String>, active: bool) -> Self {
        Self {
            id,
            title: title.into(),
            active,
            workspace: 0,
            app_id: None,
        }
    }

    /// Attach the workspace, for overview filtering.
    pub fn with_workspace(mut self, workspace: u32) -> Self {
        self.workspace = workspace;
        self
    }

    /// Attach the client-supplied application id, for dock matching.
    pub fn with_app_id(mut self, app_id: Option<String>) -> Self {
        self.app_id = app_id;
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
    /// The thumbnails' focus: an index into the selected app's windows
    /// (GNOME's `_currentWindow` with `_thumbnailsFocused`), or `None`
    /// while the app icon itself is selected.
    switcher_window: Option<usize>,
}

/// What a key in the open switcher asks of the caller (GNOME's
/// AppSwitcherPopup `_keyPressHandler`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitcherEffect {
    /// Nothing beyond the selection change.
    None,
    /// Close this window (W or F4 on a thumbnail).
    CloseWindow(u64),
    /// Quit the selected app: close each of its windows (Q).
    QuitApp(Vec<u64>),
}

/// Keysyms the switcher acts on.
pub mod switcher_keys {
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const F4: u32 = 0xffc1;
    pub const Q: u32 = 0x71;
    pub const Q_UPPER: u32 = 0x51;
    pub const W: u32 = 0x77;
    pub const W_UPPER: u32 = 0x57;
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

    /// The switcher's items: GNOME's Alt+Tab (`switch-applications`)
    /// shows one item per app, in most-recently-used order, each standing
    /// for that app's most recent window. Windows with no app id are
    /// their own app.
    pub fn switcher_items(&self) -> Vec<u64> {
        let mut seen: Vec<String> = Vec::new();
        self.mru
            .iter()
            .copied()
            .filter(|id| {
                let key = self.app_key(*id);
                if seen.contains(&key) {
                    false
                } else {
                    seen.push(key);
                    true
                }
            })
            .collect()
    }

    /// How many windows the app of window `id` has open (GNOME draws an
    /// arrow under switcher items with more than one).
    pub fn app_window_count(&self, id: u64) -> usize {
        let key = self.app_key(id);
        self.windows
            .iter()
            .filter(|w| self.app_key(w.id) == key)
            .count()
    }

    fn app_key(&self, id: u64) -> String {
        self.windows
            .iter()
            .find(|w| w.id == id)
            .and_then(|w| w.app_id.clone())
            .unwrap_or_else(|| format!("window:{id}"))
    }

    /// Current switcher selection, if the switcher is open and nonempty:
    /// the focused thumbnail's window, else the selected app's most
    /// recent window.
    pub fn switcher_selection(&self) -> Option<u64> {
        let app = self
            .switcher_open
            .then(|| self.switcher_items().get(self.switcher_index).copied())
            .flatten()?;
        match self.switcher_window {
            Some(w) => self.app_windows(app).get(w).copied().or(Some(app)),
            None => Some(app),
        }
    }

    /// The selected app's representative window while the switcher is
    /// open (the icon highlighted).
    pub fn switcher_app(&self) -> Option<u64> {
        self.switcher_open
            .then(|| self.switcher_items().get(self.switcher_index).copied())
            .flatten()
    }

    /// The focused thumbnail, if the thumbnails hold the focus.
    pub fn switcher_window(&self) -> Option<usize> {
        self.switcher_open.then_some(self.switcher_window).flatten()
    }

    /// The windows of the app of window `id`, most recent first (GNOME's
    /// `cachedWindows`).
    pub fn app_windows(&self, id: u64) -> Vec<u64> {
        let key = self.app_key(id);
        self.mru
            .iter()
            .copied()
            .filter(|w| self.app_key(*w) == key)
            .collect()
    }

    /// GNOME's `switch-group` (Alt+Above_Tab): open on the current app
    /// with its next window focused (its only one when it has one), or
    /// step through the selected app's windows. Returns the selection.
    pub fn switcher_step_window(&mut self, forward: bool) -> Option<u64> {
        let items = self.switcher_items();
        let first = *items.first()?;
        if !self.switcher_open {
            self.switcher_open = true;
            self.switcher_index = 0;
            let n = self.app_windows(first).len();
            self.switcher_window = Some(match (forward, n) {
                (false, n) => n.saturating_sub(1),
                (true, n) if n > 1 => 1,
                _ => 0,
            });
            return self.switcher_selection();
        }
        let app = items.get(self.switcher_index).copied()?;
        let n = self.app_windows(app).len().max(1);
        self.switcher_window = Some(match self.switcher_window {
            // The first Above_Tab only moves the focus into the
            // thumbnails, onto the first window.
            None if forward => 0,
            None => n - 1,
            Some(w) if forward => (w + 1) % n,
            Some(w) => (w + n - 1) % n,
        });
        self.switcher_selection()
    }

    /// A key in the open switcher: arrows move between apps, or between
    /// windows once the thumbnails hold the focus; Down focuses the
    /// thumbnails, Up leaves them; Q quits the app, W or F4 closes the
    /// focused thumbnail's window.
    pub fn switcher_key(&mut self, keysym: u32) -> SwitcherEffect {
        use switcher_keys::*;
        let items = self.switcher_items();
        let Some(app) = self.switcher_app() else {
            return SwitcherEffect::None;
        };
        let windows = self.app_windows(app);
        let n_apps = items.len();
        match (keysym, self.switcher_window) {
            (Q | Q_UPPER, _) => return SwitcherEffect::QuitApp(windows),
            (W | W_UPPER | F4, Some(w)) => {
                if let Some(id) = windows.get(w) {
                    return SwitcherEffect::CloseWindow(*id);
                }
            }
            (LEFT, Some(w)) => self.switcher_window = Some((w + windows.len() - 1) % windows.len()),
            (RIGHT, Some(w)) => self.switcher_window = Some((w + 1) % windows.len()),
            (UP, Some(_)) => self.switcher_window = None,
            (LEFT, None) => {
                self.switcher_index = (self.switcher_index + n_apps - 1) % n_apps;
            }
            (RIGHT, None) => self.switcher_index = (self.switcher_index + 1) % n_apps,
            (DOWN, None) => self.switcher_window = Some(0),
            _ => {}
        }
        SwitcherEffect::None
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
            let items = self.switcher_items().len();
            if items == 0 {
                self.switcher_open = false;
                self.switcher_index = 0;
                self.switcher_window = None;
            } else {
                self.switcher_index = self.switcher_index.min(items - 1);
                // A closed window can leave the focus past the end.
                let app = self.switcher_items()[self.switcher_index];
                let n = self.app_windows(app).len();
                self.switcher_window = self.switcher_window.filter(|w| *w < n);
            }
        }
    }

    /// Advance the switcher selection, opening it on first step. The
    /// first step lands past the current window (`mru[1]`); further
    /// steps wrap around. Returns the new selection, or `None` with no
    /// windows (the switcher stays closed).
    pub fn switcher_step(&mut self, forward: bool) -> Option<u64> {
        let items = self.switcher_items();
        if items.is_empty() {
            return None;
        }
        // A new app takes the focus back from the thumbnails.
        self.switcher_window = None;
        if !self.switcher_open {
            self.switcher_open = true;
            self.switcher_index = if items.len() >= 2 { 1 } else { 0 };
        } else if forward {
            self.switcher_index = (self.switcher_index + 1) % items.len();
        } else {
            self.switcher_index = self
                .switcher_index
                .checked_sub(1)
                .unwrap_or(items.len() - 1);
        }
        items.get(self.switcher_index).copied()
    }

    /// Commit the switcher: close it and return the selection for the
    /// caller to activate. Returns `None` when closed or empty.
    pub fn switcher_commit(&mut self) -> Option<u64> {
        let selection = self.switcher_selection();
        self.switcher_open = false;
        self.switcher_index = 0;
        self.switcher_window = None;
        selection
    }

    /// Cancel the switcher without activating.
    pub fn switcher_cancel(&mut self) {
        self.switcher_open = false;
        self.switcher_index = 0;
        self.switcher_window = None;
    }

    /// Mirror another model's switcher overlay (host sync path, after
    /// the window list already matches): open with the same selection
    /// when it names a known window, else close.
    pub fn apply_switcher_state(&mut self, open: bool, selection: Option<u64>) {
        match (open, selection) {
            (true, Some(id)) => {
                if let Some(index) = self.switcher_items().iter().position(|known| *known == id) {
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
    fn switcher_groups_windows_by_app_like_gnome() {
        let mut model = ShellModel::new();
        let windows = vec![
            WindowEntry::new(1, "Doc 1", true).with_app_id(Some("editor".into())),
            WindowEntry::new(2, "Web", false).with_app_id(Some("browser".into())),
            WindowEntry::new(3, "Doc 2", false).with_app_id(Some("editor".into())),
        ];
        model.apply_window_list(windows, vec![0]);
        // One item per app, each the app's most recent window.
        let items = model.switcher_items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], 1, "the focused editor window leads");
        assert_eq!(model.app_window_count(1), 2);
        assert_eq!(model.app_window_count(2), 1);
        // Alt+Tab goes straight to the other app.
        assert_eq!(model.switcher_step(true), Some(2));
        assert_eq!(
            model.switcher_step(true),
            Some(1),
            "wraps over apps, not windows"
        );
    }

    fn editor_and_browser() -> ShellModel {
        let mut model = ShellModel::new();
        model.apply_window_list(
            vec![
                WindowEntry::new(1, "Doc 1", true).with_app_id(Some("editor".into())),
                WindowEntry::new(2, "Web", false).with_app_id(Some("browser".into())),
                WindowEntry::new(3, "Doc 2", false).with_app_id(Some("editor".into())),
            ],
            vec![0],
        );
        model
    }

    #[test]
    fn switch_group_steps_through_the_apps_windows_like_gnome() {
        let mut model = editor_and_browser();
        assert_eq!(model.app_windows(1), vec![1, 3]);
        // Alt+Above_Tab opens on the current app's next window.
        assert_eq!(model.switcher_step_window(true), Some(3));
        assert_eq!(model.switcher_window(), Some(1));
        assert_eq!(model.switcher_step_window(true), Some(1), "wraps");
        // Left/Right move between windows while the thumbnails hold the
        // focus; Up gives it back to the icon.
        model.switcher_key(switcher_keys::RIGHT);
        assert_eq!(model.switcher_selection(), Some(3));
        model.switcher_key(switcher_keys::UP);
        assert_eq!(model.switcher_window(), None);
        assert_eq!(model.switcher_selection(), Some(1));
        // Now Right moves to the next app; Down focuses its windows.
        model.switcher_key(switcher_keys::RIGHT);
        assert_eq!(model.switcher_selection(), Some(2));
        model.switcher_key(switcher_keys::DOWN);
        assert_eq!(model.switcher_window(), Some(0));
        assert_eq!(model.switcher_commit(), Some(2));
    }

    #[test]
    fn switcher_keys_close_and_quit_like_gnome() {
        let mut model = editor_and_browser();
        model.switcher_step_window(true);
        assert_eq!(
            model.switcher_key(switcher_keys::W),
            SwitcherEffect::CloseWindow(3)
        );
        assert_eq!(
            model.switcher_key(switcher_keys::Q),
            SwitcherEffect::QuitApp(vec![1, 3])
        );
        // Alt+Tab takes the focus back to the app icons.
        model.switcher_step(true);
        assert_eq!(model.switcher_window(), None);
        assert_eq!(
            model.switcher_key(switcher_keys::W),
            SwitcherEffect::None,
            "W only closes a focused thumbnail"
        );
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
