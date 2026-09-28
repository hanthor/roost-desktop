//! Notification center: local store behind the freedesktop seam
//! (002 notifications).
//!
//! The 004 daemon will serve `org.freedesktop.Notifications` on top of
//! this store unchanged: [`NotificationCenter::notify`] takes the
//! freedesktop shape (app name, summary, body, action list, urgency,
//! replace id) and applies queue/history/DND rules here. Banners never
//! take focus — the center owns no selection and touches none — and
//! action invocations are single-use with typed errors, replay-guarded
//! like activation tokens.
//!
//! Privacy: [`Notification`] `Debug` is redacted (ids and counts
//! only). Logs must never carry app names, titles, bodies, or action
//! labels.

use std::collections::{HashSet, VecDeque};

/// How many banners may stack before older ones wait in history.
const MAX_BANNERS: usize = 3;
/// How many notifications history retains (oldest drop first).
const MAX_HISTORY: usize = 50;

/// Freedesktop-style urgency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Urgency {
    /// Background information.
    Low,
    /// Normal notification (the default).
    #[default]
    Normal,
    /// Breaks through Do Not Disturb onto the banner queue.
    Critical,
}

/// One invokable notification action (freedesktop `actions` pairs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationAction {
    /// Action key (`"default"` activates the notification itself).
    pub id: String,
    /// Human-readable label (rendered by the banner surface later).
    pub label: String,
}

/// One stored notification. `Debug` is redacted: ids and counts only.
#[derive(Clone, PartialEq, Eq)]
pub struct Notification {
    /// Store-assigned id (also the daemon's outward id).
    pub id: u64,
    /// Sending application name.
    app: String,
    /// Summary line.
    title: String,
    /// Body text.
    body: String,
    /// Invokable actions.
    actions: Vec<NotificationAction>,
    /// Urgency.
    pub urgency: Urgency,
    /// Expanded in the banner (body shown).
    pub expanded: bool,
    /// Action keys already invoked (replay guard).
    consumed: HashSet<String>,
}

impl Notification {
    /// Action keys still available (never the consumed ones).
    pub fn pending_actions(&self) -> Vec<&str> {
        self.actions
            .iter()
            .map(|action| action.id.as_str())
            .filter(|id| !self.consumed.contains(*id))
            .collect()
    }
}

impl std::fmt::Debug for Notification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notification")
            .field("id", &self.id)
            .field("urgency", &self.urgency)
            .field("actions", &self.actions.len())
            .field("expanded", &self.expanded)
            .finish()
    }
}

/// Typed notification-center failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationError {
    /// No notification with this id.
    UnknownNotification,
    /// This notification has no such action key.
    UnknownAction,
    /// This action key was already invoked (replay).
    ActionReplayed,
}

impl std::fmt::Display for NotificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownNotification => write!(f, "unknown notification"),
            Self::UnknownAction => write!(f, "unknown action"),
            Self::ActionReplayed => write!(f, "action already invoked"),
        }
    }
}

impl std::error::Error for NotificationError {}

/// Local notification store: banner queue, history, DND gate.
#[derive(Debug, Default)]
pub struct NotificationCenter {
    next_id: u64,
    /// Banner-visible ids, oldest first.
    banners: VecDeque<u64>,
    /// Full history, oldest first (bounded).
    history: VecDeque<Notification>,
    /// Do Not Disturb: normal/low notifications skip the banners.
    dnd: bool,
}

impl NotificationCenter {
    /// Empty center with Do Not Disturb off.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether Do Not Disturb is on.
    pub fn dnd(&self) -> bool {
        self.dnd
    }

    /// Flip Do Not Disturb. Enabling never pulls banners (already
    /// shown ones stay); disabling shows nothing retroactively —
    /// gated notifications wait in history.
    pub fn set_dnd(&mut self, dnd: bool) {
        self.dnd = dnd;
    }

    /// Banner-visible notifications, oldest first.
    pub fn banners(&self) -> Vec<&Notification> {
        self.banners
            .iter()
            .filter_map(|id| self.history.iter().find(|n| n.id == *id))
            .collect()
    }

    /// Full history, oldest first.
    pub fn history(&self) -> &VecDeque<Notification> {
        &self.history
    }

    /// File a notification (freedesktop `Notify` shape). Returns the
    /// store id. With `replaces_id` naming a live entry, that entry is
    /// updated in place and re-queued instead of allocating.
    pub fn notify(
        &mut self,
        app: &str,
        title: &str,
        body: &str,
        actions: Vec<NotificationAction>,
        urgency: Urgency,
        replaces_id: Option<u64>,
    ) -> u64 {
        if let Some(replaces) = replaces_id {
            if let Some(entry) = self.history.iter_mut().find(|n| n.id == replaces) {
                entry.app = app.to_owned();
                entry.title = title.to_owned();
                entry.body = body.to_owned();
                entry.actions = actions;
                entry.urgency = urgency;
                entry.expanded = false;
                entry.consumed.clear();
                self.queue_banner(replaces, urgency);
                return replaces;
            }
        }
        self.next_id += 1;
        let id = self.next_id;
        self.history.push_back(Notification {
            id,
            app: app.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
            actions,
            urgency,
            expanded: false,
            consumed: HashSet::new(),
        });
        while self.history.len() > MAX_HISTORY {
            let dropped = self.history.pop_front().map(|n| n.id);
            if let Some(dropped) = dropped {
                self.banners.retain(|b| *b != dropped);
            }
        }
        self.queue_banner(id, urgency);
        id
    }

    /// Queue a banner unless the DND gate holds (critical always
    /// breaks through) or the id is already queued.
    fn queue_banner(&mut self, id: u64, urgency: Urgency) {
        if self.dnd && urgency != Urgency::Critical {
            return;
        }
        if !self.banners.contains(&id) {
            self.banners.push_back(id);
        }
        while self.banners.len() > MAX_BANNERS {
            self.banners.pop_front();
        }
    }

    /// Dismiss a banner (history keeps the entry).
    pub fn dismiss(&mut self, id: u64) -> Result<(), NotificationError> {
        if self.history.iter().all(|n| n.id != id) {
            return Err(NotificationError::UnknownNotification);
        }
        self.banners.retain(|b| *b != id);
        Ok(())
    }

    /// Toggle a banner's expanded state.
    pub fn expand(&mut self, id: u64, expanded: bool) -> Result<(), NotificationError> {
        let Some(entry) = self.history.iter_mut().find(|n| n.id == id) else {
            return Err(NotificationError::UnknownNotification);
        };
        entry.expanded = expanded;
        Ok(())
    }

    /// Invoke one action key. Single-use per notification: a second
    /// invocation of the same key fails with
    /// [`NotificationError::ActionReplayed`], like an activation-token
    /// replay. Invoking dismisses the banner (history keeps the entry).
    pub fn invoke_action(&mut self, id: u64, action: &str) -> Result<(), NotificationError> {
        let Some(entry) = self.history.iter_mut().find(|n| n.id == id) else {
            return Err(NotificationError::UnknownNotification);
        };
        if !entry.actions.iter().any(|a| a.id == action) {
            return Err(NotificationError::UnknownAction);
        }
        if !entry.consumed.insert(action.to_owned()) {
            return Err(NotificationError::ActionReplayed);
        }
        self.banners.retain(|b| *b != id);
        Ok(())
    }

    /// Drop all history (and with it, every banner).
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.banners.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(id: &str) -> NotificationAction {
        NotificationAction {
            id: id.to_owned(),
            label: format!("label-{id}"),
        }
    }

    fn center() -> NotificationCenter {
        NotificationCenter::new()
    }

    #[test]
    fn notify_queues_banner_and_history_in_order() {
        let mut c = center();
        let a = c.notify("app", "t1", "b1", vec![], Urgency::Normal, None);
        let b = c.notify("app", "t2", "b2", vec![], Urgency::Low, None);
        assert_eq!((a, b), (1, 2));
        assert_eq!(c.history().len(), 2);
        assert_eq!(
            c.banners().iter().map(|n| n.id).collect::<Vec<_>>(),
            vec![a, b]
        );
    }

    #[test]
    fn banner_queue_caps_and_history_prunes() {
        let mut c = center();
        for i in 0..(MAX_BANNERS + 2) {
            c.notify("app", "t", "b", vec![], Urgency::Normal, None);
            let _ = i;
        }
        assert_eq!(c.banners().len(), MAX_BANNERS);
        for _ in 0..(MAX_HISTORY + 5) {
            c.notify("app", "t", "b", vec![], Urgency::Normal, None);
        }
        assert_eq!(c.history().len(), MAX_HISTORY);
        // Pruned ids leave no dangling banners.
        let live: Vec<u64> = c.history().iter().map(|n| n.id).collect();
        assert!(c.banners().iter().all(|n| live.contains(&n.id)));
    }

    #[test]
    fn dnd_gates_banners_but_not_critical_or_history() {
        let mut c = center();
        c.set_dnd(true);
        assert!(c.dnd());
        let a = c.notify("app", "t", "b", vec![], Urgency::Normal, None);
        let b = c.notify("app", "t", "b", vec![], Urgency::Low, None);
        let crit = c.notify("app", "t", "b", vec![], Urgency::Critical, None);
        assert!(c.banners().iter().all(|n| n.id == crit));
        assert_eq!(c.history().len(), 3);
        // Disabling shows nothing retroactively.
        c.set_dnd(false);
        assert_eq!(c.banners().len(), 1);
        let _ = (a, b);
    }

    #[test]
    fn dismiss_and_expand_keep_history() {
        let mut c = center();
        let id = c.notify("app", "t", "b", vec![], Urgency::Normal, None);
        assert!(c.expand(id, true).is_ok());
        assert!(c.banners()[0].expanded);
        assert!(c.dismiss(id).is_ok());
        assert!(c.banners().is_empty());
        assert_eq!(c.history().len(), 1);
        assert_eq!(
            c.dismiss(id + 99),
            Err(NotificationError::UnknownNotification)
        );
        assert_eq!(
            c.expand(id + 99, true),
            Err(NotificationError::UnknownNotification)
        );
    }

    #[test]
    fn actions_replay_guard_like_tokens() {
        let mut c = center();
        let id = c.notify(
            "app",
            "t",
            "b",
            vec![action("open"), action("default")],
            Urgency::Normal,
            None,
        );
        assert_eq!(c.invoke_action(id, "open"), Ok(()));
        // Banner dismissed by the invocation, history kept.
        assert!(c.banners().is_empty());
        assert_eq!(c.history().len(), 1);
        // Replay of the same key fails; the other key still works once.
        assert_eq!(
            c.invoke_action(id, "open"),
            Err(NotificationError::ActionReplayed)
        );
        assert_eq!(c.invoke_action(id, "default"), Ok(()));
        assert_eq!(
            c.invoke_action(id, "nope"),
            Err(NotificationError::UnknownAction)
        );
        assert_eq!(
            c.invoke_action(id + 99, "open"),
            Err(NotificationError::UnknownNotification)
        );
    }

    #[test]
    fn replaces_id_updates_in_place_and_requeues() {
        let mut c = center();
        let id = c.notify("app", "old", "b", vec![], Urgency::Normal, None);
        assert!(c.dismiss(id).is_ok());
        let same = c.notify(
            "app",
            "new",
            "b2",
            vec![action("x")],
            Urgency::Low,
            Some(id),
        );
        assert_eq!(same, id);
        assert_eq!(c.history().len(), 1);
        assert_eq!(c.history()[0].title, "new");
        assert_eq!(c.banners().len(), 1);
        // Unknown replace ids allocate fresh.
        let fresh = c.notify("app", "t", "b", vec![], Urgency::Normal, Some(999));
        assert_ne!(fresh, 999);
    }

    #[test]
    fn debug_is_redacted_ids_and_counts_only() {
        let mut c = center();
        let id = c.notify(
            "secret-app",
            "secret title",
            "secret body",
            vec![action("open")],
            Urgency::Critical,
            None,
        );
        let rendered = format!("{:?}", c.history()[0]);
        assert!(rendered.contains(&id.to_string()));
        for secret in [
            "secret-app",
            "secret title",
            "secret body",
            "label-open",
            "open",
        ] {
            assert!(!rendered.contains(secret), "leaked {secret}: {rendered}");
        }
    }

    #[test]
    fn center_never_touches_window_selection() {
        use crate::model::ShellModel;
        let mut model = ShellModel::new();
        model.apply_window_list(
            vec![
                crate::model::WindowEntry::new(1, "a", true),
                crate::model::WindowEntry::new(2, "b", false),
            ],
            vec![0],
        );
        let mut c = center();
        let id = c.notify(
            "app",
            "t",
            "b",
            vec![action("open")],
            Urgency::Critical,
            None,
        );
        let _ = c.expand(id, true);
        let _ = c.invoke_action(id, "open");
        let _ = c.dismiss(id);
        c.set_dnd(true);
        // No banner, action, dismiss, or DND path selects anything.
        assert_eq!(model.selected(), Some(1));
    }
}
