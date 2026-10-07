//! Publish the GNOME preference; only the hardware compositor owns Orca.
use gio::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub fn start(shell: Rc<RefCell<crate::Shell>>) {
    let Some(settings) = crate::settings("org.gnome.desktop.a11y.applications") else {
        return;
    };
    if !settings
        .settings_schema()
        .is_some_and(|schema| schema.has_key("screen-reader-enabled"))
    {
        return;
    }
    let publish = {
        let settings = settings.clone();
        Rc::new(move || {
            let enabled = settings.boolean("screen-reader-enabled");
            if enabled {
                if let Some(interface) = crate::settings("org.gnome.desktop.interface") {
                    if let Err(error) = interface.set_boolean("toolkit-accessibility", true) {
                        eprintln!("roost-shell-gtk: toolkit accessibility unavailable: {error}");
                    }
                }
            }
            if let Some(control) = shell.borrow_mut().control.as_mut() {
                let _ = control.set_screen_reader(enabled);
            }
        })
    };
    publish();
    settings.connect_changed(Some("screen-reader-enabled"), move |_, _| publish());
    std::mem::forget(settings);
}
