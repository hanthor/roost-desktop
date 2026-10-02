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
    Screenshot,
    ScreenshotWindow,
    BrightnessUp,
    BrightnessDown,
}

/// One key, its action, GNOME 51's default accelerators, and its modes.
struct Spec {
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
            key: "toggle-overview".into(),
            action: Action::ToggleOverview,
            defaults: vec![],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            key: "toggle-application-view".into(),
            action: Action::ToggleApplicationView,
            defaults: vec!["<Super>a"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            key: "toggle-message-tray".into(),
            action: Action::ToggleMessageTray,
            defaults: vec!["<Super>v", "<Super>m"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            key: "toggle-quick-settings".into(),
            action: Action::ToggleQuickSettings,
            defaults: vec!["<Super>s"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            key: "screenshot".into(),
            action: Action::Screenshot,
            defaults: vec!["<Shift>Print"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            key: "screenshot-window".into(),
            action: Action::ScreenshotWindow,
            defaults: vec!["<Alt>Print"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            key: "screen-brightness-up".into(),
            action: Action::BrightnessUp,
            defaults: vec!["XF86MonBrightnessUp"],
            modes: ALL,
        },
        Spec {
            key: "screen-brightness-down".into(),
            action: Action::BrightnessDown,
            defaults: vec!["XF86MonBrightnessDown"],
            modes: ALL,
        },
    ];
    for n in 1..=9u8 {
        specs.push(Spec {
            key: format!("switch-to-application-{n}"),
            action: Action::SwitchToApplication(n),
            defaults: vec![DIGIT_DEFAULTS[usize::from(n - 1)]],
            modes: NORMAL_OVERVIEW,
        });
        specs.push(Spec {
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

/// The current bindings: each key's accelerators from `settings` (GNOME's
/// schema) when it has the key, else GNOME 51's defaults.
pub fn bindings(settings: Option<&gio::Settings>) -> Vec<Binding> {
    let has = |key: &str| {
        settings
            .and_then(|s| s.settings_schema())
            .is_some_and(|schema| schema.has_key(key))
    };
    specs()
        .into_iter()
        .flat_map(|spec| {
            let accels: Vec<String> = match settings {
                Some(s) if has(&spec.key) => {
                    s.strv(&spec.key).iter().map(|a| a.to_string()).collect()
                }
                _ => spec.defaults.iter().map(|a| (*a).to_owned()).collect(),
            };
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

/// GNOME's schema, when installed.
pub fn settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup(SCHEMA, true)?;
    Some(gio::Settings::new(SCHEMA))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_51_defaults_without_the_schema() {
        let all = bindings(None);
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
