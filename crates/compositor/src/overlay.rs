//! Recovery overlay state machine (001 R5, ADR 0003).
//!
//! Pure logic only: visibility, window list, selection, restart budget,
//! and key-to-action mapping. There is no rendering here — pixels arrive
//! with the winit backend slice, which will own the input + GLES path
//! and drive this state machine.
//!
//! Behavior reference (notes only, no code read or copied): GNOME's
//! locked-shell shield keeps a minimal shield up when the normal shell
//! is unavailable, offering only session-preserving actions (seeing what
//! is open, restarting/re-entering the session) while ordinary shell
//! chrome stays out of reach, with focus and input kept alive on the
//! shield itself. This overlay follows that same shape: a
//! compositor-owned window list plus shell relaunch served from
//! compositor state, safe with no shell process running. All code here
//! is original.

pub use crate::supervise::RecoveryAction;

/// One entry in the overlay's compositor-served window list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayWindow {
    /// Compositor-side window identifier, stable across shell restarts.
    pub id: u64,
    /// Display title served from compositor state (no shell IPC needed).
    pub title: String,
}

/// Compositor-owned emergency overlay: visible while the shell is absent.
///
/// Selection is an id (not an index) so it survives list refreshes; every
/// mutator keeps `selected` pointing at a listed id or `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    /// Whether the overlay is currently shown.
    pub visible: bool,
    /// Windows served from compositor state.
    pub windows: Vec<OverlayWindow>,
    /// Selected window id, if any (always a listed id when `Some`).
    pub selected: Option<u64>,
    /// Shell relaunches consumed through this overlay so far.
    pub restart_attempts_used: u32,
    /// Finite relaunch budget (mirrors the [`Supervisor`](crate::supervise::Supervisor) policy).
    pub budget: u32,
}

/// Keys the overlay handles while it holds focus and input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKey {
    /// Move selection towards the first window.
    Up,
    /// Move selection towards the last window.
    Down,
    /// Confirm the selection against the compositor-served list.
    Activate,
    /// Request an immediate shell relaunch.
    Relaunch,
    /// Dismiss (toggle off) the overlay itself.
    Dismiss,
}

/// Map a key to the shell-absent-safe [`RecoveryAction`] it requests.
///
/// `Up`/`Down` are navigation: they drive [`Overlay::select_next`] /
/// [`Overlay::select_prev`] and are not recovery actions, so they map to
/// `None`. `Activate` confirms against the compositor-served list
/// ([`RecoveryAction::ListWindows`]), `Relaunch` relaunches the shell,
/// and `Dismiss` toggles the overlay itself.
pub fn action_for(key: OverlayKey) -> Option<RecoveryAction> {
    match key {
        OverlayKey::Up | OverlayKey::Down => None,
        OverlayKey::Activate => Some(RecoveryAction::ListWindows),
        OverlayKey::Relaunch => Some(RecoveryAction::RelaunchShell),
        OverlayKey::Dismiss => Some(RecoveryAction::ShowOverlay),
    }
}

/// Linux evdev key codes driving the overlay while it holds focus.
/// Basis: `linux/input-event-codes.h` (`KEY_ESC`, `KEY_ENTER`, `KEY_R`,
/// `KEY_UP`, `KEY_DOWN`). The runtime feeds pressed keys here only while
/// the overlay is visible; anything else falls through to the windows.
pub mod keycode {
    /// Dismiss the overlay.
    pub const ESC: u32 = 1;
    /// Confirm the selection against the compositor-served list.
    pub const ENTER: u32 = 28;
    /// Request an immediate shell relaunch.
    pub const R: u32 = 19;
    /// Move selection towards the first window.
    pub const UP: u32 = 103;
    /// Move selection towards the last window.
    pub const DOWN: u32 = 108;
}

/// Map a pressed key code to its overlay key, if the overlay handles it.
pub fn overlay_key_for_keycode(keycode: u32) -> Option<OverlayKey> {
    match keycode {
        keycode::UP => Some(OverlayKey::Up),
        keycode::DOWN => Some(OverlayKey::Down),
        keycode::ENTER => Some(OverlayKey::Activate),
        keycode::R => Some(OverlayKey::Relaunch),
        keycode::ESC => Some(OverlayKey::Dismiss),
        _ => None,
    }
}

impl Overlay {
    /// Apply one overlay key: navigation mutates the selection inline,
    /// other keys report their [`RecoveryAction`] for the runtime to
    /// carry out (refresh, relaunch, dismiss). Returns `None` for pure
    /// navigation.
    pub fn apply_key(&mut self, key: OverlayKey) -> Option<RecoveryAction> {
        match key {
            OverlayKey::Up => {
                self.select_prev();
                None
            }
            OverlayKey::Down => {
                self.select_next();
                None
            }
            _ => action_for(key),
        }
    }
}

impl Overlay {
    /// Hidden overlay with an empty list and a fresh restart budget.
    pub fn new(budget: u32) -> Self {
        Self {
            visible: false,
            windows: Vec::new(),
            selected: None,
            restart_attempts_used: 0,
            budget,
        }
    }

    /// Show the overlay with a fresh window list, selecting the first
    /// window when the list is non-empty.
    pub fn show(&mut self, windows: Vec<OverlayWindow>) {
        self.visible = true;
        self.selected = windows.first().map(|w| w.id);
        self.windows = windows;
    }

    /// Hide the overlay, keeping the list and selection for the next show.
    pub fn hide(&mut self) {
        self.visible = false;
    }

    /// Move selection one step towards the last window, wrapping around.
    /// Empty list: no-op. No selection with a non-empty list: select first.
    pub fn select_next(&mut self) {
        if self.windows.is_empty() {
            return;
        }
        let next = match self.selected.and_then(|id| self.index_of(id)) {
            Some(i) => self.windows[(i + 1) % self.windows.len()].id,
            None => self.windows[0].id,
        };
        self.selected = Some(next);
    }

    /// Move selection one step towards the first window, wrapping around.
    /// Empty list: no-op. No selection with a non-empty list: select last.
    pub fn select_prev(&mut self) {
        if self.windows.is_empty() {
            return;
        }
        let prev = match self.selected.and_then(|id| self.index_of(id)) {
            Some(i) => {
                let len = self.windows.len();
                self.windows[(i + len - 1) % len].id
            }
            None => self.windows[self.windows.len() - 1].id,
        };
        self.selected = Some(prev);
    }

    /// Select the window with `id`. Unknown ids are ignored: the current
    /// selection is left untouched.
    pub fn select(&mut self, id: u64) {
        if self.windows.iter().any(|w| w.id == id) {
            self.selected = Some(id);
        }
    }

    /// Replace the window list from a fresh compositor snapshot, keeping
    /// the selection when its id still exists and otherwise falling back
    /// to the first window (or `None` for an empty snapshot).
    pub fn refresh_from_snapshot(&mut self, titles: &[(u64, String)]) {
        self.windows = titles
            .iter()
            .map(|(id, title)| OverlayWindow {
                id: *id,
                title: title.clone(),
            })
            .collect();
        if !self
            .selected
            .is_some_and(|id| self.windows.iter().any(|w| w.id == id))
        {
            self.selected = self.windows.first().map(|w| w.id);
        }
    }

    /// Count one operator-requested shell relaunch against the budget.
    pub fn record_restart(&mut self) {
        self.restart_attempts_used = self.restart_attempts_used.saturating_add(1);
    }

    /// Whether the relaunch budget is spent.
    pub fn is_exhausted(&self) -> bool {
        self.restart_attempts_used >= self.budget
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows(ids: &[u64]) -> Vec<OverlayWindow> {
        ids.iter()
            .map(|id| OverlayWindow {
                id: *id,
                title: format!("win {id}"),
            })
            .collect()
    }

    #[test]
    fn visibility_transitions() {
        let mut overlay = Overlay::new(3);
        assert!(!overlay.visible);

        overlay.show(windows(&[1, 2]));
        assert!(overlay.visible);
        assert_eq!(overlay.selected, Some(1));

        overlay.hide();
        assert!(!overlay.visible);
        // List and selection survive hiding for the next show.
        assert_eq!(overlay.windows.len(), 2);
        assert_eq!(overlay.selected, Some(1));

        overlay.show(Vec::new());
        assert!(overlay.visible);
        assert_eq!(overlay.selected, None);
    }

    #[test]
    fn navigation_wraps_around() {
        let mut overlay = Overlay::new(3);
        overlay.show(windows(&[1, 2, 3]));

        overlay.select_next();
        assert_eq!(overlay.selected, Some(2));
        overlay.select_next();
        assert_eq!(overlay.selected, Some(3));
        overlay.select_next();
        assert_eq!(overlay.selected, Some(1));

        overlay.select_prev();
        assert_eq!(overlay.selected, Some(3));
        overlay.select_prev();
        assert_eq!(overlay.selected, Some(2));
    }

    #[test]
    fn navigation_on_empty_list_is_safe() {
        let mut overlay = Overlay::new(3);
        overlay.show(Vec::new());
        overlay.select_next();
        overlay.select_prev();
        assert_eq!(overlay.selected, None);

        // Stale selection with an emptied list also stays safe.
        overlay.show(windows(&[7]));
        overlay.refresh_from_snapshot(&[]);
        overlay.select_next();
        overlay.select_prev();
        assert_eq!(overlay.selected, None);
    }

    #[test]
    fn unknown_id_select_is_ignored() {
        let mut overlay = Overlay::new(3);
        overlay.show(windows(&[1, 2]));
        overlay.select(2);
        assert_eq!(overlay.selected, Some(2));
        overlay.select(99);
        assert_eq!(overlay.selected, Some(2));

        // Unknown id with no selection leaves it at None.
        let mut empty = Overlay::new(3);
        empty.show(Vec::new());
        empty.select(99);
        assert_eq!(empty.selected, None);
    }

    #[test]
    fn budget_exhaustion() {
        let mut overlay = Overlay::new(2);
        assert!(!overlay.is_exhausted());
        overlay.record_restart();
        assert!(!overlay.is_exhausted());
        overlay.record_restart();
        assert!(overlay.is_exhausted());
        // Further relaunches keep it exhausted without overflow.
        overlay.record_restart();
        assert!(overlay.is_exhausted());

        let zero = Overlay::new(0);
        assert!(zero.is_exhausted());
    }

    #[test]
    fn refresh_preserves_selection_when_id_survives() {
        let mut overlay = Overlay::new(3);
        overlay.show(windows(&[1, 2, 3]));
        overlay.select(2);

        overlay.refresh_from_snapshot(&[
            (3, "win 3".to_string()),
            (2, "win 2 renamed".to_string()),
            (4, "win 4".to_string()),
        ]);
        assert_eq!(overlay.selected, Some(2));
        assert_eq!(overlay.windows.len(), 3);
        // Titles come from the fresh snapshot.
        assert_eq!(overlay.windows[1].title, "win 2 renamed");
    }

    #[test]
    fn refresh_falls_back_when_selection_is_gone() {
        let mut overlay = Overlay::new(3);
        overlay.show(windows(&[1, 2]));
        overlay.select(2);

        overlay.refresh_from_snapshot(&[(5, "win 5".to_string())]);
        assert_eq!(overlay.selected, Some(5));

        overlay.refresh_from_snapshot(&[]);
        assert_eq!(overlay.selected, None);
        assert!(overlay.windows.is_empty());
    }

    #[test]
    fn key_mapping() {
        assert_eq!(
            action_for(OverlayKey::Activate),
            Some(RecoveryAction::ListWindows)
        );
        assert_eq!(
            action_for(OverlayKey::Relaunch),
            Some(RecoveryAction::RelaunchShell)
        );
        assert_eq!(
            action_for(OverlayKey::Dismiss),
            Some(RecoveryAction::ShowOverlay)
        );
        // Navigation keys are handled by select_next/select_prev instead.
        assert_eq!(action_for(OverlayKey::Up), None);
        assert_eq!(action_for(OverlayKey::Down), None);
    }

    #[test]
    fn keycodes_map_to_overlay_keys() {
        assert_eq!(overlay_key_for_keycode(keycode::UP), Some(OverlayKey::Up));
        assert_eq!(
            overlay_key_for_keycode(keycode::DOWN),
            Some(OverlayKey::Down)
        );
        assert_eq!(
            overlay_key_for_keycode(keycode::ENTER),
            Some(OverlayKey::Activate)
        );
        assert_eq!(
            overlay_key_for_keycode(keycode::R),
            Some(OverlayKey::Relaunch)
        );
        assert_eq!(
            overlay_key_for_keycode(keycode::ESC),
            Some(OverlayKey::Dismiss)
        );
        assert_eq!(overlay_key_for_keycode(30), None);
    }

    #[test]
    fn apply_key_navigates_inline_and_reports_actions() {
        let mut overlay = Overlay::new(3);
        overlay.show(windows(&[1, 2, 3]));
        assert_eq!(overlay.selected, Some(1));

        assert_eq!(overlay.apply_key(OverlayKey::Down), None);
        assert_eq!(overlay.selected, Some(2));
        assert_eq!(overlay.apply_key(OverlayKey::Up), None);
        assert_eq!(overlay.selected, Some(1));

        assert_eq!(
            overlay.apply_key(OverlayKey::Activate),
            Some(RecoveryAction::ListWindows)
        );
        assert_eq!(
            overlay.apply_key(OverlayKey::Relaunch),
            Some(RecoveryAction::RelaunchShell)
        );
        assert_eq!(
            overlay.apply_key(OverlayKey::Dismiss),
            Some(RecoveryAction::ShowOverlay)
        );
    }
}
