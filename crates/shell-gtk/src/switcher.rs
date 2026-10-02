//! Alt-Tab switcher, GNOME 51's AppSwitcher (`switch-applications`): a
//! centered list of app tiles in most-recently-used order, one per app,
//! each a 96px icon (smaller when many) over the app's name, the
//! selection highlighted, an arrow under apps with several windows. The compositor owns the keys and the selection
//! (control model `switcher_*`); this module only draws it.

use std::rc::Rc;

use crate::live_apps::LiveApps;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
use roost_shell_host::model::ShellModel;

/// Layer namespace (matches the legacy shell's switcher surface).
pub const NAMESPACE: &str = "roost-shell-switcher";

/// What is on screen, to rebuild only on change.
#[derive(PartialEq, Default, Clone)]
struct Shown {
    /// (representative window, title, app id, the app's window count).
    items: Vec<(u64, String, Option<String>, usize)>,
    selected: Option<u64>,
}

pub struct SwitcherUi {
    window: gtk::ApplicationWindow,
    row: gtk::Box,
    apps: Rc<LiveApps>,
    shown: std::cell::RefCell<Shown>,
}

impl SwitcherUi {
    pub fn new(app: &gtk::Application, apps: Rc<LiveApps>) -> Rc<Self> {
        let window = gtk::ApplicationWindow::new(app);
        window.add_css_class("roost-switcher");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(NAMESPACE));
        window.set_keyboard_mode(KeyboardMode::None);
        // Centred on the whole monitor, top bar included, as GNOME
        // centres its switcher popup.
        window.set_exclusive_zone(-1);
        window.set_title(Some("Switch Windows"));
        // GNOME's AppSwitcher: one rounded list of app tiles.
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("switcher-list");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_halign(gtk::Align::Center);
        list.append(&row);
        window.set_child(Some(&list));
        Rc::new(Self {
            window,
            row,
            apps,
            shown: Default::default(),
        })
    }

    /// Mirror the model: show, hide, or move the highlight.
    pub fn sync(&self, model: &ShellModel) {
        let now = if model.is_switcher_open() {
            Shown {
                items: model
                    .switcher_items()
                    .iter()
                    .filter_map(|id| model.windows().iter().find(|w| w.id == *id))
                    .map(|w| {
                        (
                            w.id,
                            w.title.clone(),
                            w.app_id.clone(),
                            model.app_window_count(w.id),
                        )
                    })
                    .collect(),
                selected: model.switcher_selection(),
            }
        } else {
            Shown::default()
        };
        if *self.shown.borrow() == now {
            return;
        }
        if now.items.is_empty() {
            self.window.set_visible(false);
        } else {
            self.rebuild(&now);
            self.window.present();
        }
        *self.shown.borrow_mut() = now;
    }

    fn rebuild(&self, now: &Shown) {
        while let Some(child) = self.row.first_child() {
            self.row.remove(&child);
        }
        let apps = self.apps.get();
        let width = gtk::gdk::Display::default()
            .and_then(|d| d.monitors().item(0))
            .and_downcast::<gtk::gdk::Monitor>()
            .map(|m| m.geometry().width())
            .unwrap_or(1280);
        let icon_size = crate::logic::switcher_icon_size(now.items.len(), width);
        for (id, title, app_id, windows) in &now.items {
            let entry = app_id.as_deref().and_then(|a| {
                apps.entry(a.trim_end_matches(".desktop"))
                    .or_else(|| apps.entry(a))
            });
            let icon = match entry.and_then(|e| e.icon.clone()) {
                Some(icon) if icon.starts_with('/') => gtk::Image::from_file(icon),
                Some(icon) => gtk::Image::from_icon_name(&icon),
                None => gtk::Image::from_icon_name("application-x-executable"),
            };
            icon.set_pixel_size(icon_size);
            // The app's name (window-backed apps: their app id), as
            // GNOME labels the tile.
            let name = entry
                .map(|e| e.name.clone())
                .or_else(|| app_id.clone())
                .unwrap_or_else(|| title.clone());
            let label = gtk::Label::new(Some(&name));
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(1);
            label.set_hexpand(true);
            let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
            tile.add_css_class("item-box");
            tile.set_size_request(icon_size + 31, icon_size + 31);
            tile.append(&icon);
            tile.append(&label);
            let selected = now.selected == Some(*id);
            if selected {
                tile.add_css_class("selected");
            }
            // Screen readers hear "<app>: <title>"; the selection is
            // marked selected, as GNOME's switcher does.
            tile.set_accessible_role(gtk::AccessibleRole::ListItem);
            tile.update_property(&[gtk::accessible::Property::Label(&format!(
                "{name}: {title}"
            ))]);
            tile.update_state(&[gtk::accessible::State::Selected(Some(selected))]);
            // An app with several windows gets GNOME's arrow under its tile.
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            column.append(&tile);
            let arrow = gtk::Image::from_icon_name("pan-down-symbolic");
            arrow.add_css_class("switcher-arrow");
            arrow.set_pixel_size(8);
            arrow.set_opacity(if *windows > 1 { 1.0 } else { 0.0 });
            column.append(&arrow);
            self.row.append(&column);
        }
    }
}
