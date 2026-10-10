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
//!
//! Persistence: the queue survives restarts through a versioned JSON
//! file under the XDG state dir ([`NotificationCenter::load`] /
//! [`NotificationCenter::save`]). Writes are atomic (temp file plus
//! rename) and fire on every mutation once the center knows its path,
//! so the unread list on disk always matches memory. Missing, corrupt,
//! or version-skewed files load as empty — fail-closed, never a
//! startup error.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::notification_policy::{self, LockSource, PolicyStore, SourcePolicy};

/// How many banners may stack before older ones wait in history.
pub const MAX_BANNERS: usize = 3;
/// How many notifications history retains (oldest drop first).
pub const MAX_HISTORY: usize = 50;
/// Shell state dir name under `$XDG_STATE_HOME`.
pub const STATE_DIR_NAME: &str = "tuna-shell";
/// Versioned notification queue file name.
pub const QUEUE_FILE: &str = "notifications.json";
/// Queue file schema version (a mismatch loads as empty).
const QUEUE_VERSION: u64 = 1;

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

impl Urgency {
    /// Stable queue-file spelling.
    fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::Critical => "critical",
        }
    }

    /// Parse a queue-file spelling; unknown reads as normal.
    fn from_str(text: &str) -> Self {
        match text {
            "low" => Self::Low,
            "critical" => Self::Critical,
            _ => Self::Normal,
        }
    }
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
    /// Source icon: the sender's `app_icon` (icon name, path or file
    /// URI), shown when no desktop entry names the app (GNOME's
    /// notificationDaemon.js). Empty for none.
    icon: String,
    /// The `desktop-entry` hint, without `.desktop`; empty for none.
    desktop_entry: String,
    /// The installed app whose policy governs it (desktop id without
    /// `.desktop`); empty for the generic policy.
    app_id: String,
    /// Seen by the user: shown as a banner or in the open message list
    /// (GNOME's `acknowledged`). The lock screen counts the rest.
    acknowledged: bool,
    /// When it arrived (or was last replaced), in Unix seconds, for
    /// GNOME's "Just now" / "5 minutes ago" label.
    received: u64,
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
    /// Summary line (the banner paints this).
    pub fn summary(&self) -> &str {
        &self.title
    }

    /// Body text (the banner paints this; empty means no body line).
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Sending application name.
    pub fn app(&self) -> &str {
        &self.app
    }

    /// Source icon from `Notify` (empty when none was given).
    pub fn icon(&self) -> &str {
        &self.icon
    }

    /// The `desktop-entry` hint (empty when absent).
    pub fn desktop_entry(&self) -> &str {
        &self.desktop_entry
    }

    /// The installed app whose notification policy applies (empty for
    /// the generic policy).
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// The source it belongs to: its app, else its desktop-entry hint,
    /// else the sender's name.
    pub fn source_key(&self) -> &str {
        [&self.app_id, &self.desktop_entry, &self.app]
            .into_iter()
            .find(|k| !k.is_empty())
            .map_or("", |k| k.as_str())
    }

    /// Whether the user has seen it.
    pub fn acknowledged(&self) -> bool {
        self.acknowledged
    }

    /// Arrival time in Unix seconds.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Every action with its label, in sender order.
    pub fn actions(&self) -> &[NotificationAction] {
        &self.actions
    }

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

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Decode one queue-file notification; `None` skips a malformed
/// entry while the rest of the file still loads.
fn decode_notification(item: &serde_json::Value) -> Option<Notification> {
    let id = item.get("id")?.as_u64()?;
    if id == 0 {
        return None;
    }
    let actions = item
        .get("actions")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(NotificationAction {
                        id: row.get("id")?.as_str()?.to_owned(),
                        label: row.get("label")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let consumed = item
        .get("consumed")
        .and_then(|v| v.as_array())
        .map(|keys| {
            keys.iter()
                .filter_map(|key| key.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let text = |key: &str| {
        item.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Some(Notification {
        id,
        icon: text("icon"),
        desktop_entry: text("desktop_entry"),
        app_id: text("app_id"),
        // Files from before acknowledgement existed hold nothing new.
        acknowledged: item
            .get("acknowledged")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        received: item
            .get("received")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default(),
        app: item.get("app")?.as_str()?.to_owned(),
        title: item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned(),
        body: item
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned(),
        actions,
        urgency: Urgency::from_str(item.get("urgency").and_then(|v| v.as_str()).unwrap_or("")),
        expanded: item
            .get("expanded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        consumed,
    })
}

/// `$XDG_STATE_HOME/tuna-shell` (default `~/.local/state/tuna-shell`),
/// or `None` when neither resolves (#49: never `/tmp`).
pub fn state_dir() -> Option<PathBuf> {
    crate::xdg::state_home().map(|base| base.join(STATE_DIR_NAME))
}

/// A notification's sound hints (`sound-name`, `sound-file`); empty
/// fields are absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sound {
    /// A sound theme name (`sound-name`).
    pub name: String,
    /// A sound file path (`sound-file`).
    pub file: String,
}

impl Sound {
    /// Whether there is anything to play.
    pub fn is_empty(&self) -> bool {
        self.name.is_empty() && self.file.is_empty()
    }
}

/// One `Notify` call as the daemon receives it.
#[derive(Debug, Clone, Default)]
pub struct Incoming {
    pub app: String,
    pub icon: String,
    pub title: String,
    pub body: String,
    pub actions: Vec<NotificationAction>,
    pub urgency: Urgency,
    pub replaces_id: Option<u64>,
    /// The `desktop-entry` hint.
    pub desktop_entry: String,
    pub sound: Sound,
}

/// The policy store behind a center (none: everything generic with the
/// default global keys).
#[derive(Clone, Default)]
struct PolicySlot(Option<Arc<dyn PolicyStore>>);

impl std::fmt::Debug for PolicySlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "PolicySlot(set)"
        } else {
            "PolicySlot(none)"
        })
    }
}

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
    /// Queue file behind this center; `None` keeps it memory-only.
    /// Set by [`NotificationCenter::load`], so a restored center
    /// keeps persisting to the file it came from.
    queue_path: Option<PathBuf>,
    /// GNOME Settings' notification policy.
    policy: PolicySlot,
    /// Sounds due to play, oldest first (the shell drains them).
    sounds: Vec<Sound>,
}

impl NotificationCenter {
    /// Empty center with Do Not Disturb off.
    pub fn new() -> Self {
        Self::default()
    }

    /// System queue file path under the XDG state dir.
    pub fn system_path() -> Option<PathBuf> {
        state_dir().map(|dir| dir.join(QUEUE_FILE))
    }

    /// Load from the system queue file, creating its dir when absent.
    /// Missing, corrupt, or version-skewed files start empty.
    pub fn load_system() -> Self {
        let Some(path) = Self::system_path() else {
            return Self::new();
        };
        let _ = fs::create_dir_all(path.parent().expect("queue file has a parent"));
        Self::load(&path)
    }

    /// Load from `path`; missing or unreadable files start empty. A
    /// corrupt file (or a schema version we do not know) also starts
    /// empty — the queue is user data, not configuration the shell
    /// may crash over. Either way the path sticks, so later
    /// mutations persist back to it.
    pub fn load(path: &Path) -> Self {
        let mut center = Self {
            queue_path: Some(path.to_owned()),
            ..Self::default()
        };
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(_) => return center,
        };
        let doc: serde_json::Value = match serde_json::from_str(&text) {
            Ok(doc) => doc,
            Err(_) => return center,
        };
        if doc.get("version").and_then(serde_json::Value::as_u64) != Some(QUEUE_VERSION) {
            return center;
        }
        center.dnd = doc
            .get("dnd")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        center.next_id = doc
            .get("next_id")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let mut history = VecDeque::new();
        if let Some(items) = doc.get("notifications").and_then(|v| v.as_array()) {
            for item in items {
                let Some(entry) = decode_notification(item) else {
                    continue;
                };
                history.push_back(entry);
            }
        }
        while history.len() > MAX_HISTORY {
            history.pop_front();
        }
        // Ids must keep moving forward even when the file was edited
        // by hand: never reuse an id a live entry still carries.
        let high = history.iter().map(|n| n.id).max().unwrap_or(0);
        center.next_id = center.next_id.max(high);
        center.history = history;
        let live: HashSet<u64> = center.history.iter().map(|n| n.id).collect();
        let mut banners = VecDeque::new();
        if let Some(ids) = doc.get("banners").and_then(|v| v.as_array()) {
            for id in ids.iter().filter_map(serde_json::Value::as_u64) {
                if live.contains(&id) && !banners.contains(&id) {
                    banners.push_back(id);
                }
            }
        }
        while banners.len() > MAX_BANNERS {
            banners.pop_front();
        }
        center.banners = banners;
        center
    }

    /// Persist to `path` atomically: write plus rename, so readers
    /// never see a torn file. Creates parent dirs; surfaces I/O
    /// errors to the caller (the in-memory queue stays regardless).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let items: Vec<serde_json::Value> = self
            .history
            .iter()
            .map(|n| {
                serde_json::json!({
                    "id": n.id,
                    "app": n.app,
                    "icon": n.icon,
                    "desktop_entry": n.desktop_entry,
                    "app_id": n.app_id,
                    "acknowledged": n.acknowledged,
                    "received": n.received,
                    "title": n.title,
                    "body": n.body,
                    "actions": n.actions.iter().map(|a| serde_json::json!({
                        "id": a.id,
                        "label": a.label,
                    })).collect::<Vec<_>>(),
                    "urgency": n.urgency.as_str(),
                    "expanded": n.expanded,
                    "consumed": n.consumed.iter().collect::<Vec<_>>(),
                })
            })
            .collect();
        let doc = serde_json::json!({
            "version": QUEUE_VERSION,
            "next_id": self.next_id,
            "dnd": self.dnd,
            "banners": self.banners.iter().collect::<Vec<_>>(),
            "notifications": items,
        });
        let text = serde_json::to_string_pretty(&doc)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Pin the queue file behind this center (a [`load`](Self::load)
    /// already does this): later mutations persist back to `path`.
    pub fn set_queue_path(&mut self, path: PathBuf) {
        self.queue_path = Some(path);
    }

    /// Queue file behind this center, if any.
    pub fn queue_path(&self) -> Option<&Path> {
        self.queue_path.as_deref()
    }

    /// Persist back to the queue file when one is pinned. Failures
    /// log and keep the in-memory queue: a full disk must never lose
    /// or break the live banners.
    fn persist(&self) {
        if let Some(path) = self.queue_path.as_ref() {
            if let Err(e) = self.save(path) {
                eprintln!("tuna-shell-host: notification queue save failed: {e}");
            }
        }
    }

    /// Record where a notification came from: its `app_icon` and
    /// `desktop-entry` hint, which pick the icon and name its header
    /// shows. Unknown ids are ignored.
    pub fn set_source(&mut self, id: u64, icon: &str, desktop_entry: &str) {
        if let Some(entry) = self.history.iter_mut().find(|n| n.id == id) {
            entry.icon = icon.to_owned();
            entry.desktop_entry = desktop_entry.trim_end_matches(".desktop").to_owned();
            self.persist();
        }
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
        self.persist();
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

    /// How many notifications are unread: the banner-visible queue
    /// length. Dismissing a banner or invoking its action reads it
    /// (history keeps the entry); DND-gated arrivals never queue,
    /// so they never count.
    pub fn unread_count(&self) -> usize {
        self.banners.len()
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
        let id = self.file(app, title, body, actions, urgency, replaces_id);
        self.queue_banner(id, urgency);
        self.persist();
        id
    }

    /// Store a notification, replacing `replaces_id` in place when it
    /// names a live entry; banners are left to the caller.
    fn file(
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
                entry.received = now_secs();
                entry.expanded = false;
                entry.acknowledged = false;
                entry.consumed.clear();
                return replaces;
            }
        }
        self.alloc_fresh(app, title, body, actions, urgency)
    }

    /// Append a fresh entry, pruning history.
    fn alloc_fresh(
        &mut self,
        app: &str,
        title: &str,
        body: &str,
        actions: Vec<NotificationAction>,
        urgency: Urgency,
    ) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.history.push_back(Notification {
            id,
            icon: String::new(),
            desktop_entry: String::new(),
            app_id: String::new(),
            acknowledged: false,
            received: now_secs(),
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
        id
    }

    /// Queue a banner unless the DND gate holds (critical always
    /// breaks through) or the id is already queued.
    fn queue_banner(&mut self, id: u64, urgency: Urgency) {
        if self.dnd && urgency != Urgency::Critical {
            return;
        }
        self.push_banner(id);
    }

    /// Queue a banner (once), dropping the oldest past the cap.
    fn push_banner(&mut self, id: u64) {
        if !self.banners.contains(&id) {
            self.banners.push_back(id);
        }
        while self.banners.len() > MAX_BANNERS {
            self.banners.pop_front();
        }
    }

    /// Follow GNOME Settings' notification policy from `store`.
    pub fn set_policy(&mut self, store: Arc<dyn PolicyStore>) {
        self.policy = PolicySlot(Some(store));
    }

    /// The effective policy for an app (`None`: the generic policy).
    pub fn policy_for(&self, app_id: Option<&str>) -> SourcePolicy {
        match &self.policy.0 {
            Some(store) => store.policy(app_id),
            None => SourcePolicy::generic(Default::default()),
        }
    }

    /// File a `Notify` call under its source's policy. A disabled app's
    /// notification is dropped (never stored, critical or not), but the
    /// sender still gets a fresh id, as from GNOME. Otherwise the app is
    /// registered with GNOME Settings, and the policy decides the
    /// banner, its sound and whether it opens expanded.
    pub fn deliver(&mut self, n: Incoming) -> u64 {
        let app_id = self
            .policy
            .0
            .as_ref()
            .and_then(|store| store.app_for(&n.desktop_entry, &n.app));
        let policy = self.policy_for(app_id.as_deref());
        let Some(delivery) = notification_policy::deliver(&policy, n.urgency, self.dnd) else {
            return match n
                .replaces_id
                .filter(|id| self.history.iter().any(|e| e.id == *id))
            {
                // Replacing a filed one from a since-disabled app: it goes.
                Some(id) => {
                    self.remove(id);
                    id
                }
                None => {
                    self.next_id += 1;
                    self.persist();
                    self.next_id
                }
            };
        };
        if let (Some(store), Some(app)) = (self.policy.0.as_ref(), app_id.as_deref()) {
            store.register(app);
        }
        let id = self.file(
            &n.app,
            &n.title,
            &n.body,
            n.actions,
            n.urgency,
            n.replaces_id,
        );
        if let Some(entry) = self.history.iter_mut().find(|e| e.id == id) {
            entry.icon = n.icon;
            entry.desktop_entry = n.desktop_entry.trim_end_matches(".desktop").to_owned();
            entry.app_id = app_id.unwrap_or_default();
            entry.expanded = delivery.expanded;
        }
        if delivery.banner {
            self.push_banner(id);
        } else {
            self.banners.retain(|b| *b != id);
        }
        if delivery.sound && !n.sound.is_empty() {
            self.sounds.push(n.sound);
        }
        self.persist();
        id
    }

    /// Re-apply the policy to what is already filed: an app disabled
    /// since its notifications arrived loses them. Returns the dropped
    /// ids.
    pub fn enforce_policy(&mut self) -> Vec<u64> {
        let disabled: Vec<u64> = self
            .history
            .iter()
            .filter(|n| !n.app_id.is_empty())
            .filter(|n| !self.policy_for(Some(&n.app_id)).enable)
            .map(|n| n.id)
            .collect();
        if !disabled.is_empty() {
            self.history.retain(|n| !disabled.contains(&n.id));
            self.banners.retain(|b| !disabled.contains(b));
            self.persist();
        }
        disabled
    }

    /// Sounds due to play since the last call.
    pub fn take_sounds(&mut self) -> Vec<Sound> {
        std::mem::take(&mut self.sounds)
    }

    /// Mark notifications seen (a banner shown, the list opened).
    pub fn acknowledge(&mut self, ids: &[u64]) {
        let mut changed = false;
        for entry in self.history.iter_mut().filter(|n| ids.contains(&n.id)) {
            changed |= !std::mem::replace(&mut entry.acknowledged, true);
        }
        if changed {
            self.persist();
        }
    }

    /// What the lock screen shows, by the current policy: one entry per
    /// source with unseen notifications that may show while locked
    /// (see [`notification_policy::lock_screen`]).
    pub fn lock_screen(&self) -> Vec<LockSource> {
        notification_policy::lock_screen(self.history.iter().rev(), |n| {
            self.policy_for((!n.app_id.is_empty()).then_some(n.app_id.as_str()))
        })
    }

    /// Dismiss a banner (history keeps the entry).
    pub fn dismiss(&mut self, id: u64) -> Result<(), NotificationError> {
        if self.history.iter().all(|n| n.id != id) {
            return Err(NotificationError::UnknownNotification);
        }
        self.banners.retain(|b| *b != id);
        self.persist();
        Ok(())
    }

    /// Toggle a banner's expanded state.
    pub fn expand(&mut self, id: u64, expanded: bool) -> Result<(), NotificationError> {
        let Some(entry) = self.history.iter_mut().find(|n| n.id == id) else {
            return Err(NotificationError::UnknownNotification);
        };
        entry.expanded = expanded;
        self.persist();
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
        self.persist();
        Ok(())
    }

    /// Forget one notification entirely (its close button in the
    /// list): history and banner both. Unknown ids are ignored.
    pub fn remove(&mut self, id: u64) {
        self.history.retain(|n| n.id != id);
        self.banners.retain(|b| *b != id);
        self.persist();
    }

    /// Drop all history (and with it, every banner).
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.banners.clear();
        self.persist();
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

    /// GNOME Settings in memory: apps by desktop id, the registrations
    /// they made.
    #[derive(Default)]
    struct FakeStore {
        global: std::sync::Mutex<notification_policy::GlobalPrefs>,
        apps: std::sync::Mutex<std::collections::HashMap<String, notification_policy::AppPrefs>>,
        registered: std::sync::Mutex<Vec<String>>,
    }

    impl FakeStore {
        fn set(&self, id: &str, f: impl FnOnce(&mut notification_policy::AppPrefs)) {
            f(self.apps.lock().unwrap().entry(id.to_owned()).or_default());
        }
    }

    impl PolicyStore for FakeStore {
        fn app_for(&self, desktop_entry: &str, app_name: &str) -> Option<String> {
            let apps = self.apps.lock().unwrap();
            notification_policy::resolve_app(desktop_entry, app_name, |id| apps.contains_key(id))
        }
        fn global(&self) -> notification_policy::GlobalPrefs {
            *self.global.lock().unwrap()
        }
        fn app(&self, app_id: &str) -> notification_policy::AppPrefs {
            self.apps
                .lock()
                .unwrap()
                .get(app_id)
                .copied()
                .unwrap_or_default()
        }
        fn register(&self, app_id: &str) {
            let mut seen = self.registered.lock().unwrap();
            if !seen.iter().any(|s| s == app_id) {
                seen.push(app_id.to_owned());
            }
        }
    }

    fn incoming(entry: &str, title: &str, urgency: Urgency) -> Incoming {
        Incoming {
            app: "Sender".to_owned(),
            title: title.to_owned(),
            body: format!("{title} body"),
            urgency,
            desktop_entry: entry.to_owned(),
            sound: Sound {
                name: "message-new-instant".to_owned(),
                file: String::new(),
            },
            ..Incoming::default()
        }
    }

    fn policed() -> (NotificationCenter, Arc<FakeStore>) {
        let store = Arc::new(FakeStore::default());
        store.set("org.example.Mail", |_| {});
        store.set("org.example.Chat", |_| {});
        let mut c = center();
        c.set_policy(store.clone());
        (c, store)
    }

    #[test]
    fn disabled_apps_are_dropped_and_others_register() {
        let (mut c, store) = policed();
        store.set("org.example.Chat", |a| a.enable = false);
        let mail = c.deliver(incoming(
            "org.example.Mail.desktop",
            "Mail",
            Urgency::Normal,
        ));
        let chat = c.deliver(incoming("org.example.Chat", "Chat", Urgency::Critical));
        // The sender still gets an id, but nothing is filed or shown.
        assert_ne!(chat, 0);
        assert_ne!(chat, mail);
        assert_eq!(c.history().iter().map(|n| n.id).collect::<Vec<_>>(), [mail]);
        assert_eq!(c.banners().iter().map(|n| n.id).collect::<Vec<_>>(), [mail]);
        assert_eq!(c.history()[0].app_id(), "org.example.Mail");
        // Only the app that notified registers with GNOME Settings.
        assert_eq!(*store.registered.lock().unwrap(), ["org.example.Mail"]);
        // A generic sender registers nothing and follows the defaults.
        c.deliver(incoming("", "Generic", Urgency::Normal));
        assert_eq!(c.history().len(), 2);
        assert_eq!(store.registered.lock().unwrap().len(), 1);
    }

    #[test]
    fn per_app_banners_and_sounds_follow_settings_and_dnd() {
        let (mut c, store) = policed();
        store.set("org.example.Chat", |a| {
            a.show_banners = false;
        });
        store.set("org.example.Mail", |a| a.enable_sound = false);
        let mail = c.deliver(incoming("org.example.Mail", "Mail", Urgency::Normal));
        let chat = c.deliver(incoming("org.example.Chat", "Chat", Urgency::Normal));
        let generic = c.deliver(incoming("", "Generic", Urgency::Normal));
        assert_eq!(
            c.banners().iter().map(|n| n.id).collect::<Vec<_>>(),
            [mail, generic]
        );
        // Mail is muted, chat had no banner: only the generic one sounds.
        assert_eq!(c.take_sounds().len(), 1);
        assert!(c.take_sounds().is_empty());
        let _ = chat;
        // Do Not Disturb holds every banner and sound but critical ones,
        // which also break through the per-app banner switch.
        c.set_dnd(true);
        c.deliver(incoming("org.example.Mail", "Quiet", Urgency::Normal));
        let crit = c.deliver(incoming("org.example.Chat", "Alarm", Urgency::Critical));
        assert_eq!(c.banners().last().map(|n| n.id), Some(crit));
        assert!(c.banners().last().unwrap().expanded);
        assert_eq!(c.take_sounds().len(), 1);
        // suppress-sound (an empty Sound) plays nothing.
        let mut quiet = incoming("org.example.Chat", "Alarm 2", Urgency::Critical);
        quiet.sound = Sound::default();
        c.deliver(quiet);
        assert!(c.take_sounds().is_empty());
    }

    #[test]
    fn force_expanded_opens_the_banner() {
        let (mut c, store) = policed();
        store.set("org.example.Mail", |a| a.force_expanded = true);
        let mail = c.deliver(incoming("org.example.Mail", "Mail", Urgency::Normal));
        let chat = c.deliver(incoming("org.example.Chat", "Chat", Urgency::Normal));
        let expanded = |id| c.history().iter().find(|n| n.id == id).unwrap().expanded;
        assert!(expanded(mail));
        assert!(!expanded(chat));
    }

    #[test]
    fn disabling_an_app_later_drops_what_it_filed() {
        let (mut c, store) = policed();
        let mail = c.deliver(incoming("org.example.Mail", "Mail", Urgency::Normal));
        let chat = c.deliver(incoming("org.example.Chat", "Chat", Urgency::Normal));
        assert!(c.enforce_policy().is_empty());
        store.set("org.example.Chat", |a| a.enable = false);
        assert_eq!(c.enforce_policy(), [chat]);
        assert_eq!(c.history().iter().map(|n| n.id).collect::<Vec<_>>(), [mail]);
        assert!(c.banners().iter().all(|n| n.id == mail));
        // An update sent by a disabled app takes the notification it
        // replaces away and files nothing.
        let mut again = incoming("org.example.Chat", "Chat 2", Urgency::Normal);
        again.replaces_id = Some(mail);
        c.deliver(again);
        assert!(c.history().is_empty());
    }

    #[test]
    fn lock_screen_follows_live_policy_and_acknowledgement() {
        let (mut c, store) = policed();
        let mail = c.deliver(incoming("org.example.Mail", "Invoice", Urgency::Normal));
        let view = c.lock_screen();
        assert_eq!(view.len(), 1);
        assert_eq!((view[0].newest, view[0].count), (mail, 1));
        assert!(view[0].details.is_none());
        store.set("org.example.Mail", |a| a.details_in_lock_screen = true);
        assert_eq!(
            c.lock_screen()[0].details.as_deref(),
            Some(&[("Invoice".to_owned(), "Invoice body".to_owned())][..])
        );
        store.global.lock().unwrap().show_in_lock_screen = false;
        assert!(c.lock_screen().is_empty());
        store.global.lock().unwrap().show_in_lock_screen = true;
        c.acknowledge(&[mail]);
        assert!(c.lock_screen().is_empty());
    }

    #[test]
    fn app_identity_and_acknowledgement_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(QUEUE_FILE);
        let (mut c, _store) = policed();
        c.set_queue_path(path.clone());
        let a = c.deliver(incoming("org.example.Mail", "A", Urgency::Normal));
        let b = c.deliver(incoming("org.example.Mail", "B", Urgency::Normal));
        c.acknowledge(&[a]);
        let back = NotificationCenter::load(&path);
        let by = |id| back.history().iter().find(|n| n.id == id).unwrap();
        assert_eq!(by(b).app_id(), "org.example.Mail");
        assert!(by(a).acknowledged() && !by(b).acknowledged());
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
    fn unread_count_tracks_the_banner_queue() {
        let mut c = center();
        assert_eq!(c.unread_count(), 0);
        let a = c.notify("app", "t", "b", vec![], Urgency::Normal, None);
        assert_eq!(c.unread_count(), 1);
        c.notify("app", "t", "b", vec![], Urgency::Low, None);
        assert_eq!(c.unread_count(), 2);
        // Dismissing reads the banner; history keeps the entry.
        c.dismiss(a).unwrap();
        assert_eq!(c.unread_count(), 1);
        assert_eq!(c.history().len(), 2);
        // DND-gated arrivals never queue, so they never read as unread.
        c.set_dnd(true);
        c.notify("app", "t", "b", vec![], Urgency::Normal, None);
        assert_eq!(c.unread_count(), 1);
        assert_eq!(c.history().len(), 3);
        // Critical breaks through DND and counts again.
        c.notify("app", "t", "b", vec![], Urgency::Critical, None);
        assert_eq!(c.unread_count(), 2);
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
    fn queue_roundtrip_preserves_unread_list_and_guards() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(QUEUE_FILE);
        let mut c = center();
        c.set_dnd(true);
        let gated = c.notify("app", "gated", "b", vec![], Urgency::Normal, None);
        assert!(
            c.banners().iter().all(|n| n.id != gated),
            "DND-gated arrival never queues"
        );
        let crit = c.notify(
            "app",
            "loud",
            "boom",
            vec![action("open")],
            Urgency::Critical,
            None,
        );
        c.expand(crit, true).unwrap();
        // A second critical whose key a banner press consumes: the
        // invocation dismisses it from the unread list, but history
        // keeps the consumed key.
        let acted = c.notify(
            "app",
            "acted",
            "b",
            vec![action("open"), action("later")],
            Urgency::Critical,
            None,
        );
        c.invoke_action(acted, "open").unwrap();
        c.save(&path).expect("save");
        // Atomic write: no temp file left behind.
        assert!(
            dir.path()
                .join("notifications.json.tmp")
                .symlink_metadata()
                .is_err(),
            "rename must consume the temp file"
        );

        let again = NotificationCenter::load(&path);
        assert!(again.dnd(), "DND survives the restart");
        assert_eq!(
            again.banners().iter().map(|n| n.id).collect::<Vec<_>>(),
            c.banners().iter().map(|n| n.id).collect::<Vec<_>>(),
            "restart preserves the unread list"
        );
        assert_eq!(again.history().len(), c.history().len());
        let entry = again
            .history()
            .iter()
            .find(|n| n.id == crit)
            .expect("entry");
        assert_eq!(entry.summary(), "loud");
        assert_eq!(entry.body(), "boom");
        assert!(entry.expanded, "expand flag survives");
        assert_eq!(
            entry.pending_actions(),
            vec!["open"],
            "pending key survives"
        );
        assert_eq!(entry.urgency, Urgency::Critical);
        let acted_entry = again
            .history()
            .iter()
            .find(|n| n.id == acted)
            .expect("entry");
        assert_eq!(
            acted_entry.pending_actions(),
            vec!["later"],
            "consumed key stays consumed"
        );
        // Ids keep moving forward: the next file never reuses one.
        let mut again = again;
        let fresh = again.notify("app", "t", "b", vec![], Urgency::Normal, None);
        assert!(fresh > crit, "no id reuse after reload");
    }

    #[test]
    fn queue_mutations_persist_back_to_the_loaded_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(QUEUE_FILE);
        let mut c = center();
        c.set_queue_path(path.clone());
        let id = c.notify("app", "t", "b", vec![], Urgency::Normal, None);
        assert!(path.exists(), "notify writes the queue file");
        c.dismiss(id).unwrap();
        let again = NotificationCenter::load(&path);
        assert!(again.banners().is_empty(), "dismiss reaches the disk queue");
        assert_eq!(again.history().len(), 1, "history still kept");
    }

    #[test]
    fn remove_forgets_one_notification() {
        let mut c = center();
        let a = c.notify("app", "a", "", vec![], Urgency::Normal, None);
        let b = c.notify("app", "b", "", vec![], Urgency::Normal, None);
        c.remove(a);
        assert_eq!(
            c.history().iter().map(|n| n.id).collect::<Vec<_>>(),
            vec![b]
        );
        assert!(c.banners().iter().all(|n| n.id != a), "its banner goes too");
    }

    #[test]
    fn source_and_arrival_survive_a_reload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(QUEUE_FILE);
        let mut c = center();
        c.set_queue_path(path.clone());
        let id = c.notify("Files", "t", "b", vec![], Urgency::Normal, None);
        c.set_source(id, "folder-symbolic", "org.gnome.Nautilus.desktop");
        let again = NotificationCenter::load(&path);
        let n = &again.history()[0];
        assert_eq!(n.icon(), "folder-symbolic");
        assert_eq!(n.desktop_entry(), "org.gnome.Nautilus", "suffix dropped");
        assert!(n.received() > 1_700_000_000, "arrival time recorded");
    }

    #[test]
    fn queue_load_is_fail_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Missing file starts empty but remembers its path.
        let missing = dir.path().join("nope.json");
        let c = NotificationCenter::load(&missing);
        assert!(c.history().is_empty());
        assert!(c.banners().is_empty());
        assert_eq!(c.queue_path(), Some(missing.as_path()));
        // Corrupt content starts empty.
        let corrupt = dir.path().join(QUEUE_FILE);
        std::fs::write(&corrupt, "{ not json").expect("write corrupt");
        let c = NotificationCenter::load(&corrupt);
        assert!(c.history().is_empty());
        assert!(c.banners().is_empty());
        // Unknown schema versions start empty (never half-read).
        std::fs::write(&corrupt, r#"{"version": 999, "notifications": []}"#).expect("write skewed");
        let c = NotificationCenter::load(&corrupt);
        assert!(c.history().is_empty());
        // Dangling banner ids and oversized queues clamp on load.
        let doc = serde_json::json!({
            "version": 1,
            "next_id": 3,
            "dnd": false,
            "banners": [1, 2, 3, 999],
            "notifications": [
                {"id": 1, "app": "a", "title": "t", "body": "b",
                 "actions": [], "urgency": "normal", "expanded": false, "consumed": []},
                {"id": 2, "app": "a", "title": "t", "body": "b",
                 "actions": [], "urgency": "bogus", "expanded": false, "consumed": []},
                {"id": 0, "app": "a", "title": "t", "body": "b",
                 "actions": [], "urgency": "normal", "expanded": false, "consumed": []},
                {"no-id": true},
            ],
        });
        std::fs::write(&corrupt, serde_json::to_string(&doc).unwrap()).expect("write clamped");
        let c = NotificationCenter::load(&corrupt);
        assert_eq!(c.history().len(), 2, "zero-id and id-less rows skipped");
        assert_eq!(
            c.history()[1].urgency,
            Urgency::Normal,
            "bogus urgency reads normal"
        );
        assert!(
            c.banners().iter().all(|n| n.id == 1 || n.id == 2),
            "dangling banner id 999 dropped"
        );
    }

    #[test]
    fn flood_keeps_ten_thousand_notifies_bounded_and_newest() {
        let mut c = center();
        for _ in 0..10_000 {
            c.notify("app", "t", "b", vec![], Urgency::Normal, None);
        }
        assert_eq!(c.history().len(), MAX_HISTORY, "history stays capped");
        assert_eq!(c.banners().len(), MAX_BANNERS, "banner queue stays capped");
        let ids: Vec<u64> = c.history().iter().map(|n| n.id).collect();
        assert_eq!(
            ids,
            (9951..=10_000).collect::<Vec<_>>(),
            "newest kept in history"
        );
        let queued: Vec<u64> = c.banners().iter().map(|n| n.id).collect();
        assert_eq!(
            queued,
            vec![9998, 9999, 10_000],
            "newest kept on the banners"
        );
        // A second flood changes nothing about the bounds: flat.
        for _ in 0..10_000 {
            c.notify("app", "t", "b", vec![], Urgency::Normal, None);
        }
        assert_eq!(c.history().len(), MAX_HISTORY);
        assert_eq!(c.banners().len(), MAX_BANNERS);
        let ids: Vec<u64> = c.history().iter().map(|n| n.id).collect();
        assert_eq!(ids, (19951..=20_000).collect::<Vec<_>>());
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
