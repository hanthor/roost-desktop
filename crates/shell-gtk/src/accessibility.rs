//! GNOME accessibility indicator and keyboard focus visibility preference.
use crate::gtk;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const FOCUS_KEY: &str = "keyboard-focus-visible-timeout";
const BOOLEAN_ITEMS: &[(&str, &str, &str)] = &[
    (
        "High Contrast",
        "org.gnome.desktop.a11y.interface",
        "high-contrast",
    ),
    (
        "Zoom",
        "org.gnome.desktop.a11y.applications",
        "screen-magnifier-enabled",
    ),
    (
        "Screen Reader",
        "org.gnome.desktop.a11y.applications",
        "screen-reader-enabled",
    ),
    (
        "Screen Keyboard",
        "org.gnome.desktop.a11y.applications",
        "screen-keyboard-enabled",
    ),
    (
        "Visual Alerts",
        "org.gnome.desktop.wm.preferences",
        "visual-bell",
    ),
    (
        "Sticky Keys",
        "org.gnome.desktop.a11y.keyboard",
        "stickykeys-enable",
    ),
    (
        "Slow Keys",
        "org.gnome.desktop.a11y.keyboard",
        "slowkeys-enable",
    ),
    (
        "Bounce Keys",
        "org.gnome.desktop.a11y.keyboard",
        "bouncekeys-enable",
    ),
    (
        "Mouse Keys",
        "org.gnome.desktop.a11y.keyboard",
        "mousekeys-enable",
    ),
];
fn has_key(settings: &gio::Settings, key: &str) -> bool {
    settings.settings_schema().is_some_and(|s| s.has_key(key))
}
fn indicator_visible(always: bool, mut active: impl Iterator<Item = bool>) -> bool {
    always || active.any(|a| a)
}

pub fn menu() -> gtk::MenuButton {
    let popover = gtk::Popover::new();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.set_halign(gtk::Align::Start);
    column.set_margin_top(12);
    column.set_margin_bottom(12);
    column.set_margin_start(12);
    column.set_margin_end(12);
    let mut items = Vec::new();
    let mut retained = Vec::new();
    for (label, schema, key) in BOOLEAN_ITEMS {
        let item = gtk::CheckButton::with_label(label);
        if let Some(settings) = crate::settings(schema).filter(|s| has_key(s, key)) {
            settings.bind(key, &item, "active").build();
            retained.push(settings);
        } else {
            item.set_sensitive(false);
        }
        column.append(&item);
        items.push(item);
    }
    let large = gtk::CheckButton::with_label("Large Text");
    // GNOME places Large Text between Zoom and Screen Reader.
    column.insert_child_after(&large, Some(&items[1]));
    if let Some(settings) = crate::settings(crate::INTERFACE_SCHEMA) {
        large.set_active(settings.double("text-scaling-factor") > 1.0);
        settings.bind_writable("text-scaling-factor", &large, "sensitive", false);
        let updating = Rc::new(Cell::new(false));
        let changed = large.downgrade();
        let sync = updating.clone();
        settings.connect_changed(Some("text-scaling-factor"), move |s, _| {
            if let Some(item) = changed.upgrade() {
                sync.set(true);
                item.set_active(s.double("text-scaling-factor") > 1.0);
                sync.set(false);
            }
        });
        let target = settings.clone();
        large.connect_toggled(move |item| {
            if !updating.get() {
                if item.is_active() {
                    let _ = target.set_double("text-scaling-factor", 1.25);
                } else {
                    target.reset("text-scaling-factor");
                }
            }
        });
        retained.push(settings);
    } else {
        large.set_sensitive(false);
    }
    items.push(large);
    column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let settings_button = gtk::Button::with_label("Accessibility Settings");
    settings_button.connect_clicked(|_| {
        if let Err(error) = std::process::Command::new("gnome-control-center")
            .arg("universal-access")
            .spawn()
        {
            eprintln!("tuna-shell-gtk: accessibility settings: {error}");
        }
    });
    column.append(&settings_button);
    popover.set_child(Some(&column));
    let icon = gtk::Image::from_icon_name("accessibility-menu-symbolic");
    icon.add_css_class("system-status-icon");
    let button = crate::panel_menu_button(&icon, "Accessibility", &popover);
    let always = crate::settings("org.gnome.desktop.a11y");
    let sync: Rc<dyn Fn()> = {
        let button = button.downgrade();
        let items: Vec<_> = items.iter().map(|item| item.downgrade()).collect();
        let always = always.as_ref().map(|s| s.downgrade());
        Rc::new(move || {
            if let Some(button) = button.upgrade() {
                let visible = indicator_visible(
                    always
                        .as_ref()
                        .and_then(|s| s.upgrade())
                        .is_some_and(|s| s.boolean("always-show-universal-access-status")),
                    items
                        .iter()
                        .filter_map(|i| i.upgrade())
                        .map(|i| i.is_active()),
                );
                if !visible {
                    button.popdown();
                }
                button.set_visible(visible);
            }
        })
    };
    for item in items {
        let sync = sync.clone();
        item.connect_toggled(move |_| sync());
    }
    if let Some(always) = always {
        let update = sync.clone();
        always.connect_changed(Some("always-show-universal-access-status"), move |_, _| {
            update()
        });
        retained.push(always);
    }
    sync();
    // Keep backend watches alive exactly as long as this menu.
    button.connect_destroy(move |_| {
        let _ = &retained;
    });
    button
}

fn duration_seconds(value: i32) -> Option<u64> {
    match value {
        0 => None,
        v if v < 0 => Some(3),
        v => Some(v as u64),
    }
}

pub fn start_focus_policy() {
    let Some(settings) =
        crate::settings(crate::A11Y_INTERFACE_SCHEMA).filter(|s| has_key(s, FOCUS_KEY))
    else {
        return;
    };
    install_focus_policy(settings);
}

fn install_focus_policy(settings: gio::Settings) {
    let Some(toolkit) = gtk::Settings::default() else {
        return;
    };
    if toolkit
        .find_property("gtk-keyboard-focus-visible-timeout")
        .is_some()
    {
        settings
            .bind(FOCUS_KEY, &toolkit, "gtk-keyboard-focus-visible-timeout")
            .build();
        // Toolkit owns keyboard/mouse handling and timers on GTK >= 4.23.3.
        std::mem::forget(settings);
        return;
    }
    // Older GTK has a fixed three-second timer. Refresh it while the
    // requested duration is alive, using a weak window and no input grabs.
    let windows = gtk::Window::toplevels();
    let install: Rc<dyn Fn(gtk::Window)> = Rc::new(move |window| {
        let last = Rc::new(Cell::new(None::<Instant>));
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let pressed = last.clone();
        key.connect_key_pressed(move |_, _, _, _| {
            pressed.set(Some(Instant::now()));
            glib::Propagation::Proceed
        });
        window.add_controller(key);
        let visible = last.clone();
        window.connect_focus_visible_notify(move |w| {
            if w.gets_focus_visible() {
                if visible.get().is_none() {
                    visible.set(Some(Instant::now()));
                }
            } else {
                visible.set(None);
            }
        });
        let weak = window.downgrade();
        let prefs = settings.clone();
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(window) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if window.gets_focus_visible() {
                if let Some(start) = last.get() {
                    if duration_seconds(prefs.int(FOCUS_KEY))
                        .is_none_or(|s| start.elapsed() < Duration::from_secs(s))
                    {
                        window.set_focus_visible(true);
                    } else {
                        window.set_focus_visible(false);
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    });
    for i in 0..windows.n_items() {
        if let Some(w) = windows.item(i).and_downcast::<gtk::Window>() {
            install(w);
        }
    }
    windows.connect_items_changed(move |windows, position, _, added| {
        for i in position..position + added {
            if let Some(w) = windows.item(i).and_downcast::<gtk::Window>() {
                install(w);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indicator_follows_gnome_enabled_or_requested_rule() {
        assert!(!indicator_visible(false, [false; 10].into_iter()));
        assert!(indicator_visible(true, [false; 10].into_iter()));
        for active in 0..10 {
            assert!(indicator_visible(false, (0..10).map(|i| i == active)));
        }
    }
    #[test]
    fn focus_timeout_preserves_forever_and_toolkit_default() {
        assert_eq!(duration_seconds(0), None);
        assert_eq!(duration_seconds(-1), Some(3));
        assert_eq!(duration_seconds(1), Some(1));
        assert_eq!(duration_seconds(i32::MAX), Some(i32::MAX as u64));
    }
    #[test]
    #[ignore = "requires private graphical/session/accessibility buses and focus schema fixture"]
    fn live_menu_and_focus_settings() {
        gtk::init().unwrap();
        assert_eq!(std::env::var("GSETTINGS_BACKEND").as_deref(), Ok("memory"));
        let drain = |seconds: f64| {
            let end = Instant::now() + Duration::from_secs_f64(seconds);
            while Instant::now() < end {
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        for (_, schema, key) in BOOLEAN_ITEMS {
            if let Some(s) = crate::settings(schema).filter(|s| has_key(s, key)) {
                s.set_boolean(key, false).unwrap();
            }
        }
        let iface = crate::settings(crate::INTERFACE_SCHEMA).unwrap();
        iface.set_double("text-scaling-factor", 1.0).unwrap();
        let always = crate::settings("org.gnome.desktop.a11y").unwrap();
        always
            .set_boolean("always-show-universal-access-status", false)
            .unwrap();
        let button = menu();
        assert!(!button.is_visible());
        always
            .set_boolean("always-show-universal-access-status", true)
            .unwrap();
        drain(0.1);
        assert!(button.is_visible());
        always
            .set_boolean("always-show-universal-access-status", false)
            .unwrap();
        drain(0.1);
        assert!(!button.is_visible());
        iface.set_double("text-scaling-factor", 1.1).unwrap();
        drain(0.1);
        assert!(button.is_visible());
        let column = button
            .popover()
            .unwrap()
            .child()
            .unwrap()
            .downcast::<gtk::Box>()
            .unwrap();
        let mut child = column.first_child();
        let mut count = 0;
        while let Some(item) = child {
            child = item.next_sibling();
            if let Ok(check) = item.downcast::<gtk::CheckButton>() {
                count += 1;
                if check.label().as_deref() == Some("Large Text") {
                    assert!(check.is_active());
                    check.set_active(false);
                    assert_eq!(
                        iface.double("text-scaling-factor"),
                        iface
                            .default_value("text-scaling-factor")
                            .unwrap()
                            .get::<f64>()
                            .unwrap()
                    );
                    check.set_active(true);
                    assert_eq!(iface.double("text-scaling-factor"), 1.25);
                }
            }
        }
        assert_eq!(count, 10);
        iface.set_double("text-scaling-factor", 1.0).unwrap();
        drain(0.1);
        for (label, schema, key) in BOOLEAN_ITEMS {
            let Some(prefs) = crate::settings(schema).filter(|s| has_key(s, key)) else {
                continue;
            };
            prefs.set_boolean(key, true).unwrap();
            drain(0.1);
            assert!(button.is_visible(), "enabled {label} exposes indicator");
            let mut child = column.first_child();
            let mut found = false;
            while let Some(item) = child {
                child = item.next_sibling();
                if let Ok(check) = item.downcast::<gtk::CheckButton>() {
                    if check.label().as_deref() == Some(label) {
                        assert!(check.is_active());
                        check.set_active(false);
                        assert!(!prefs.boolean(key));
                        found = true;
                        break;
                    }
                }
            }
            assert!(found);
            drain(0.1);
            assert!(
                !button.is_visible(),
                "last enabled feature disabled hides indicator"
            );
        }

        let fixture = std::env::var("TUNA_A11Y_TEST_SCHEMA_DIR").unwrap();
        let source = gio::SettingsSchemaSource::from_directory(
            fixture,
            None::<&gio::SettingsSchemaSource>,
            false,
        )
        .unwrap();
        let schema = source.lookup("org.tuna.FocusPolicy", false).unwrap();
        let prefs = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
        prefs.set_int(FOCUS_KEY, 1).unwrap();
        install_focus_policy(prefs.clone());
        let window = gtk::Window::new();
        window.set_child(Some(&button));
        window.present();
        drain(0.2);
        window.set_focus_visible(false);
        window.set_focus_visible(true);
        drain(0.3);
        assert!(window.gets_focus_visible());
        drain(1.0);
        assert!(!window.gets_focus_visible());
        prefs.set_int(FOCUS_KEY, 0).unwrap();
        window.set_focus_visible(true);
        drain(3.5);
        assert!(
            window.gets_focus_visible(),
            "zero survives older GTK's fixed three-second timeout"
        );
        // Pointer-driven clearing stays cleared even with forever enabled.
        window.set_focus_visible(false);
        drain(0.2);
        assert!(!window.gets_focus_visible());
        prefs.set_int(FOCUS_KEY, -1).unwrap();
        window.set_focus_visible(true);
        drain(3.3);
        assert!(!window.gets_focus_visible());
        window.destroy();
        if let Ok(path) = std::env::var("TUNA_A11Y_RECEIPT") {
            std::fs::write(path, "PASS actual GTK: ten menu rows; requested/feature visibility; Large Text reset/1.25; focus 1s/forever/default/mouse-clear\n").unwrap();
        }
    }
}
