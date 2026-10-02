//! GNOME Shell's own keybindings (`org.gnome.shell.keybindings`), as
//! windowManager.js, screenshot.js and brightnessManager.js bind them:
//! the overview and app view, the message tray and quick settings, the
//! dash's apps by number, screenshots, and the brightness keys. Keys
//! come from GSettings when GNOME's schema is installed, else GNOME 51's
//! defaults; the compositor grabs them like any accelerator.

use gtk4::gio;
use gtk4::prelude::*;

use roost_shell_control::{MODE_LOCK_SCREEN, MODE_NORMAL, MODE_OVERVIEW, MODE_UNLOCK_SCREEN};

pub const SCHEMA: &str = "org.gnome.shell.keybindings";
/// GNOME's window-manager keys (gsettings-desktop-schemas).
pub const WM_SCHEMA: &str = "org.gnome.desktop.wm.keybindings";

/// What a binding does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ToggleOverview,
    ToggleApplicationView,
    ToggleMessageTray,
    ToggleQuickSettings,
    /// Focus the nth dash app (1-based), launching it when not running.
    SwitchToApplication(u8),
    /// Open a new window of the nth dash app.
    OpenNewWindow(u8),
    ShowScreenshotUi,
    Screenshot,
    ScreenshotWindow,
    BrightnessUp,
    BrightnessDown,
    /// Window-manager keys on the focused window or the workspaces.
    WindowMenu,
    ToggleMaximized,
    Unmaximize,
    BeginMove,
    BeginResize,
    /// Switch to a workspace: first, last, or one step left/right.
    Workspace(Target),
    /// Move the focused window there and follow it.
    MoveToWorkspace(Target),
}

/// A workspace a key goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    First,
    Last,
    Left,
    Right,
}

impl Target {
    /// The workspace id for this target, from `active` among `workspaces`
    /// with windows on `occupied`. GNOME's workspaces are dynamic: one
    /// empty workspace always follows the last occupied one, and that is
    /// where Last goes; Right past the end is the next new one.
    pub fn resolve(self, active: u32, workspaces: &[u32], occupied: &[u32]) -> Option<u32> {
        let first = workspaces.iter().copied().min().unwrap_or(0);
        let trailing = occupied.iter().copied().max().map_or(0, |m| m + 1);
        let last = workspaces
            .iter()
            .copied()
            .max()
            .unwrap_or(active)
            .max(trailing);
        match self {
            Target::First => Some(first),
            Target::Last => Some(last),
            Target::Left => (active > first).then(|| active - 1),
            Target::Right => Some(active + 1),
        }
    }
}

/// Which schema a key lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Schema {
    Shell,
    Wm,
}

/// One key, its action, GNOME 51's default accelerators, and its modes.
struct Spec {
    schema: Schema,
    key: String,
    action: Action,
    defaults: Vec<&'static str>,
    modes: u32,
}

const NORMAL_OVERVIEW: u32 = MODE_NORMAL | MODE_OVERVIEW;
const ALL: u32 = MODE_NORMAL | MODE_OVERVIEW | MODE_LOCK_SCREEN | MODE_UNLOCK_SCREEN;

const DIGIT_DEFAULTS: [&str; 9] = [
    "<Super>1", "<Super>2", "<Super>3", "<Super>4", "<Super>5", "<Super>6", "<Super>7", "<Super>8",
    "<Super>9",
];
const NEW_WINDOW_DEFAULTS: [&str; 9] = [
    "<Super><Control>1",
    "<Super><Control>2",
    "<Super><Control>3",
    "<Super><Control>4",
    "<Super><Control>5",
    "<Super><Control>6",
    "<Super><Control>7",
    "<Super><Control>8",
    "<Super><Control>9",
];

fn specs() -> Vec<Spec> {
    let mut specs = vec![
        Spec {
            schema: Schema::Shell,
            key: "toggle-overview".into(),
            action: Action::ToggleOverview,
            defaults: vec![],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "toggle-application-view".into(),
            action: Action::ToggleApplicationView,
            defaults: vec!["<Super>a"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "toggle-message-tray".into(),
            action: Action::ToggleMessageTray,
            defaults: vec!["<Super>v", "<Super>m"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "toggle-quick-settings".into(),
            action: Action::ToggleQuickSettings,
            defaults: vec!["<Super>s"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "show-screenshot-ui".into(),
            action: Action::ShowScreenshotUi,
            defaults: vec!["Print"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "screenshot".into(),
            action: Action::Screenshot,
            defaults: vec!["<Shift>Print"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "screenshot-window".into(),
            action: Action::ScreenshotWindow,
            defaults: vec!["<Alt>Print"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "screen-brightness-up".into(),
            action: Action::BrightnessUp,
            defaults: vec!["XF86MonBrightnessUp"],
            modes: ALL,
        },
        Spec {
            schema: Schema::Shell,
            key: "screen-brightness-down".into(),
            action: Action::BrightnessDown,
            defaults: vec!["XF86MonBrightnessDown"],
            modes: ALL,
        },
    ];
    // GNOME's window-manager keys Roost does not bind itself (Super+
    // PageUp/PageDown, Super+arrows, Super+H, Alt+F4, Alt+Tab and
    // Super+Space are the compositor's own).
    let wm = |key: &str, action, defaults: Vec<&'static str>| Spec {
        schema: Schema::Wm,
        key: key.into(),
        action,
        defaults,
        modes: NORMAL_OVERVIEW,
    };
    specs.extend([
        wm(
            "activate-window-menu",
            Action::WindowMenu,
            vec!["<Alt>space"],
        ),
        wm(
            "toggle-maximized",
            Action::ToggleMaximized,
            vec!["<Alt>F10"],
        ),
        wm("unmaximize", Action::Unmaximize, vec!["<Alt>F5"]),
        wm("begin-move", Action::BeginMove, vec!["<Alt>F7"]),
        wm("begin-resize", Action::BeginResize, vec!["<Alt>F8"]),
        wm(
            "switch-to-workspace-1",
            Action::Workspace(Target::First),
            vec!["<Super>Home"],
        ),
        wm(
            "switch-to-workspace-last",
            Action::Workspace(Target::Last),
            vec!["<Super>End"],
        ),
        wm(
            "switch-to-workspace-left",
            Action::Workspace(Target::Left),
            vec!["<Super><Alt>Left", "<Control><Alt>Left"],
        ),
        wm(
            "switch-to-workspace-right",
            Action::Workspace(Target::Right),
            vec!["<Super><Alt>Right", "<Control><Alt>Right"],
        ),
        wm(
            "move-to-workspace-1",
            Action::MoveToWorkspace(Target::First),
            vec!["<Super><Shift>Home"],
        ),
        wm(
            "move-to-workspace-last",
            Action::MoveToWorkspace(Target::Last),
            vec!["<Super><Shift>End"],
        ),
        wm(
            "move-to-workspace-left",
            Action::MoveToWorkspace(Target::Left),
            vec!["<Super><Shift><Alt>Left", "<Control><Shift><Alt>Left"],
        ),
        wm(
            "move-to-workspace-right",
            Action::MoveToWorkspace(Target::Right),
            vec!["<Super><Shift><Alt>Right", "<Control><Shift><Alt>Right"],
        ),
    ]);
    for n in 1..=9u8 {
        specs.push(Spec {
            schema: Schema::Shell,
            key: format!("switch-to-application-{n}"),
            action: Action::SwitchToApplication(n),
            defaults: vec![DIGIT_DEFAULTS[usize::from(n - 1)]],
            modes: NORMAL_OVERVIEW,
        });
        specs.push(Spec {
            schema: Schema::Shell,
            key: format!("open-new-window-application-{n}"),
            action: Action::OpenNewWindow(n),
            defaults: vec![NEW_WINDOW_DEFAULTS[usize::from(n - 1)]],
            modes: NORMAL_OVERVIEW,
        });
    }
    specs
}

/// A bound accelerator: its action, accelerator string and modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub action: Action,
    pub accelerator: String,
    pub modes: u32,
}

/// The current bindings: each key's accelerators from its GNOME schema
/// (`shell` or `wm`) when installed, else GNOME 51's defaults. Of the
/// window-manager keys, only the accelerators Roost does not bind itself
/// are taken.
pub fn bindings(shell: Option<&gio::Settings>, wm: Option<&gio::Settings>) -> Vec<Binding> {
    specs()
        .into_iter()
        .flat_map(|spec| {
            let settings = match spec.schema {
                Schema::Shell => shell,
                Schema::Wm => wm,
            };
            let has = settings
                .and_then(|s| s.settings_schema())
                .is_some_and(|schema| schema.has_key(&spec.key));
            let accels: Vec<String> = match settings {
                Some(s) if has => s.strv(&spec.key).iter().map(|a| a.to_string()).collect(),
                _ => spec.defaults.iter().map(|a| (*a).to_owned()).collect(),
            };
            let accels: Vec<String> = accels
                .into_iter()
                .filter(|a| spec.schema == Schema::Shell || !compositor_owned(a))
                .collect();
            accels
                .into_iter()
                .filter(|a| !a.is_empty())
                .map(move |accelerator| Binding {
                    action: spec.action,
                    accelerator,
                    modes: spec.modes,
                })
        })
        .collect()
}

/// Keys the compositor binds itself (GNOME's defaults for them):
/// grabbing them too would never fire.
fn compositor_owned(accelerator: &str) -> bool {
    let a = accelerator.to_ascii_lowercase().replace("<shift>", "");
    matches!(
        a.as_str(),
        "<super>page_up" | "<super>page_down" | "<super>kp_prior" | "<super>kp_next"
    )
}

/// GNOME Shell's keybinding schema, when installed.
pub fn settings() -> Option<gio::Settings> {
    schema(SCHEMA)
}

/// GNOME's window-manager keybinding schema, when installed.
pub fn wm_settings() -> Option<gio::Settings> {
    schema(WM_SCHEMA)
}

fn schema(id: &str) -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup(id, true)?;
    Some(gio::Settings::new(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_targets_resolve_like_mutter() {
        let ws = [0, 1, 2];
        assert_eq!(Target::First.resolve(1, &ws, &[0, 1]), Some(0));
        assert_eq!(Target::Last.resolve(0, &ws, &[0, 1]), Some(2));
        assert_eq!(Target::Left.resolve(0, &ws, &[0]), None);
        assert_eq!(Target::Left.resolve(2, &ws, &[0]), Some(1));
        assert_eq!(Target::Right.resolve(2, &ws, &[0]), Some(3));
        // Everything on the first workspace: Last is the empty one after.
        assert_eq!(Target::Last.resolve(0, &[0], &[0]), Some(1));
        assert!(compositor_owned("<Super><Shift>Page_Up"));
        assert!(!compositor_owned("<Super>Home"));
    }

    #[test]
    fn gnome_51_defaults_without_the_schema() {
        let all = bindings(None, None);
        let find = |action| {
            all.iter()
                .filter(|b| b.action == action)
                .map(|b| b.accelerator.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(find(Action::ToggleApplicationView), ["<Super>a"]);
        assert_eq!(find(Action::ToggleMessageTray), ["<Super>v", "<Super>m"]);
        assert_eq!(find(Action::ToggleQuickSettings), ["<Super>s"]);
        assert!(find(Action::ToggleOverview).is_empty());
        assert_eq!(find(Action::SwitchToApplication(3)), ["<Super>3"]);
        assert_eq!(find(Action::OpenNewWindow(9)), ["<Super><Control>9"]);
        assert_eq!(find(Action::BrightnessUp), ["XF86MonBrightnessUp"]);
        assert_eq!(find(Action::ShowScreenshotUi), ["Print"]);
        assert_eq!(find(Action::WindowMenu), ["<Alt>space"]);
        assert_eq!(find(Action::Workspace(Target::Last)), ["<Super>End"]);
        assert_eq!(
            find(Action::MoveToWorkspace(Target::Right)),
            ["<Super><Shift><Alt>Right", "<Control><Shift><Alt>Right"]
        );
        let brightness = all
            .iter()
            .find(|b| b.action == Action::BrightnessUp)
            .unwrap();
        assert_ne!(
            brightness.modes & MODE_LOCK_SCREEN,
            0,
            "brightness keys work locked"
        );
        // Every default parses into a grabbable key.
        for b in &all {
            assert!(
                crate::shell_dbus::parse_accelerator(&b.accelerator).is_some(),
                "{}",
                b.accelerator
            );
        }
    }
}
