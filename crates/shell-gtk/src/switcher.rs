//! Alt-Tab switcher (GNOME 51 shape): a centered strip of app icons in
//! most-recently-used order, the selection highlighted and its window
//! title underneath. The compositor owns the keys and the selection
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
    items: Vec<(u64, String, Option<String>)>,
    selected: Option<u64>,
}

pub struct SwitcherUi {
    window: gtk::ApplicationWindow,
    row: gtk::Box,
    title: gtk::Label,
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
        window.set_title(Some("Switch Windows"));
        let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
        column.add_css_class("switcher");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_halign(gtk::Align::Center);
        let title = gtk::Label::new(None);
        title.add_css_class("switcher-title");
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(40);
        column.append(&row);
        column.append(&title);
        window.set_child(Some(&column));
        Rc::new(Self {
            window,
            row,
            title,
            apps,
            shown: Default::default(),
        })
    }

    /// Mirror the model: show, hide, or move the highlight.
    pub fn sync(&self, model: &ShellModel) {
        let now = if model.is_switcher_open() {
            Shown {
                items: model
                    .mru_order()
                    .iter()
                    .filter_map(|id| model.windows().iter().find(|w| w.id == *id))
                    .map(|w| (w.id, w.title.clone(), w.app_id.clone()))
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
        for (id, title, app_id) in &now.items {
            let entry = app_id.as_deref().and_then(|a| {
                apps.entry(a.trim_end_matches(".desktop"))
                    .or_else(|| apps.entry(a))
            });
            let icon = match entry.and_then(|e| e.icon.clone()) {
                Some(icon) if icon.starts_with('/') => gtk::Image::from_file(icon),
                Some(icon) => gtk::Image::from_icon_name(&icon),
                None => gtk::Image::from_icon_name("application-x-executable"),
            };
            icon.set_pixel_size(96);
            let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
            card.add_css_class("switcher-item");
            card.append(&icon);
            let name = entry
                .map(|e| e.name.clone())
                .unwrap_or_else(|| title.clone());
            let selected = now.selected == Some(*id);
            if selected {
                card.add_css_class("selected");
                self.title.set_text(title);
            }
            // Screen readers hear "<app>: <title>"; the selection is
            // marked selected, as GNOME's switcher does.
            card.set_accessible_role(gtk::AccessibleRole::ListItem);
            card.update_property(&[gtk::accessible::Property::Label(&format!(
                "{name}: {title}"
            ))]);
            card.update_state(&[gtk::accessible::State::Selected(Some(selected))]);
            self.row.append(&card);
        }
    }
}
