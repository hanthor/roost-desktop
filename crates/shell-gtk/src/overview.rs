//! The shell's half of the GNOME 51 overview (#54, #57).
//!
//! The compositor draws the workspace card and the live window previews;
//! the shell adds what GNOME Shell draws on top: the "Type to search"
//! entry at the top with its app results, the dash at the bottom
//! (favorites plus running apps, and Show Apps), and the app grid. All
//! three are layer surfaces shown only while the compositor reports the
//! overview open. The search surface uses the overview namespace, so the
//! compositor parks keyboard focus on it and typing goes straight to
//! search.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use roost_shell_host::apps::{AppEntry, AppProvider};

/// Namespace the compositor parks overview keyboard focus on
/// (`roost_compositor::layer::OVERVIEW_NAMESPACE`).
pub const OVERVIEW_NAMESPACE: &str = "roost-shell-overview";
/// App results shown under the search entry (GNOME shows one row).
pub const MAX_RESULTS: usize = 6;

/// What the overview asks the rest of the shell to do.
pub trait OverviewActions {
    /// Close the overview (after a launch or an activation).
    fn close_overview(&self);
    /// Raise an existing window by compositor id.
    fn activate_window(&self, id: u64);
    /// Running windows as `(id, app_id)`.
    fn running(&self) -> Vec<(u64, Option<String>)>;
}

fn app_icon(entry: &AppEntry, size: i32) -> gtk::Image {
    let image = match entry.icon.as_deref() {
        Some(icon) if icon.starts_with('/') => gtk::Image::from_file(icon),
        Some(icon) => gtk::Image::from_icon_name(icon),
        None => gtk::Image::from_icon_name("application-x-executable"),
    };
    image.set_pixel_size(size);
    image
}

fn app_button(entry: &AppEntry, icon_size: i32, with_label: bool) -> gtk::Button {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.append(&app_icon(entry, icon_size));
    if with_label {
        let label = gtk::Label::new(Some(&entry.name));
        label.set_max_width_chars(12);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        column.append(&label);
    }
    let button = gtk::Button::builder().child(&column).build();
    button.add_css_class("flat");
    button.add_css_class("overview-app");
    button.update_property(&[gtk::accessible::Property::Label(&entry.name)]);
    button.set_tooltip_text(Some(&entry.name));
    button
}

fn layer_window(app: &gtk::Application, namespace: &str, css: &str) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::new(app);
    window.add_css_class(css);
    window.init_layer_shell();
    window.set_layer(Layer::Top);
    window.set_namespace(Some(namespace));
    window
}

/// The overview's shell surfaces.
pub struct OverviewUi {
    search: gtk::ApplicationWindow,
    entry: gtk::SearchEntry,
    results: gtk::Box,
    dash: gtk::ApplicationWindow,
    dash_row: gtk::Box,
    grid: gtk::ApplicationWindow,
    apps: Rc<AppProvider>,
    favorites: Vec<String>,
    actions: Rc<dyn OverviewActions>,
    open: bool,
}

impl OverviewUi {
    /// Build the (hidden) surfaces.
    pub fn new(
        app: &gtk::Application,
        apps: Rc<AppProvider>,
        favorites: Vec<String>,
        actions: Rc<dyn OverviewActions>,
    ) -> Rc<RefCell<Self>> {
        // Search: top center, below the panel.
        let search = layer_window(app, OVERVIEW_NAMESPACE, "roost-overview-search");
        search.set_anchor(Edge::Top, true);
        search.set_margin(Edge::Top, 12);
        search.set_title(Some("Search"));
        let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let entry = gtk::SearchEntry::new();
        entry.set_placeholder_text(Some("Type to search"));
        entry.set_width_request(370);
        entry.set_halign(gtk::Align::Center);
        entry.update_property(&[gtk::accessible::Property::Label("Search")]);
        let results = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        results.add_css_class("overview-results");
        results.set_halign(gtk::Align::Center);
        results.set_visible(false);
        column.append(&entry);
        column.append(&results);
        search.set_child(Some(&column));

        // Dash: bottom center.
        let dash = layer_window(app, "roost-shell-dash", "roost-overview-dash");
        dash.set_anchor(Edge::Bottom, true);
        dash.set_margin(Edge::Bottom, 12);
        dash.set_keyboard_mode(KeyboardMode::None);
        dash.set_title(Some("Dash"));
        let dash_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        dash_row.add_css_class("overview-dash");
        dash.set_child(Some(&dash_row));

        // App grid: between search and dash, over the previews.
        let grid = layer_window(app, "roost-shell-appgrid", "roost-overview-grid");
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            grid.set_anchor(edge, true);
        }
        grid.set_margin(Edge::Top, 64);
        grid.set_margin(Edge::Bottom, 112);
        grid.set_keyboard_mode(KeyboardMode::None);
        grid.set_title(Some("App Grid"));

        let ui = Rc::new(RefCell::new(Self {
            search,
            entry,
            results,
            dash,
            dash_row,
            grid,
            apps,
            favorites,
            actions,
            open: false,
        }));
        Self::wire(&ui);
        ui
    }

    fn wire(ui: &Rc<RefCell<Self>>) {
        let me = ui.borrow();
        {
            let ui = ui.clone();
            me.entry.connect_search_changed(move |entry| {
                ui.borrow().show_results(&entry.text());
            });
        }
        {
            let ui = ui.clone();
            me.entry.connect_activate(move |entry| {
                let first = {
                    let me = ui.borrow();
                    crate::logic::rank_apps(me.apps.apps(), &entry.text(), 1)
                        .first()
                        .map(|e| (*e).clone())
                };
                if let Some(app) = first {
                    ui.borrow().launch(&app);
                }
            });
        }
    }

    fn launch(&self, entry: &AppEntry) {
        if let Err(e) = roost_shell_host::apps::launch(entry) {
            eprintln!("roost-shell-gtk: launch {}: {e}", entry.app_id);
            return;
        }
        self.actions.close_overview();
    }

    fn show_results(&self, query: &str) {
        while let Some(child) = self.results.first_child() {
            self.results.remove(&child);
        }
        let hits = crate::logic::rank_apps(self.apps.apps(), query, MAX_RESULTS);
        for hit in &hits {
            let button = app_button(hit, 64, true);
            let entry = (*hit).clone();
            let apps = self.apps.clone();
            let actions = self.actions.clone();
            button.connect_clicked(move |_| {
                let _ = apps;
                if roost_shell_host::apps::launch(&entry).is_ok() {
                    actions.close_overview();
                }
            });
            self.results.append(&button);
        }
        self.results.set_visible(!hits.is_empty());
    }

    fn rebuild_dash(ui: &Rc<RefCell<Self>>) {
        let me = ui.borrow();
        while let Some(child) = me.dash_row.first_child() {
            me.dash_row.remove(&child);
        }
        let running = me.actions.running();
        let mut shown: Vec<String> = Vec::new();
        for id in &me.favorites {
            if let Some(entry) = me.apps.entry(id) {
                shown.push(entry.app_id.clone());
                let button = app_button(entry, 48, false);
                let window = running
                    .iter()
                    .find(|(_, app)| {
                        app.as_deref() == Some(entry.app_id.trim_end_matches(".desktop"))
                    })
                    .map(|(id, _)| *id);
                if window.is_some() {
                    button.add_css_class("running");
                }
                let entry = entry.clone();
                let actions = me.actions.clone();
                button.connect_clicked(move |_| match window {
                    Some(id) => {
                        actions.activate_window(id);
                        actions.close_overview();
                    }
                    None => {
                        if roost_shell_host::apps::launch(&entry).is_ok() {
                            actions.close_overview();
                        }
                    }
                });
                me.dash_row.append(&button);
            }
        }
        me.dash_row
            .append(&gtk::Separator::new(gtk::Orientation::Vertical));
        let show_apps = gtk::ToggleButton::builder()
            .child(&{
                let image = gtk::Image::from_icon_name("view-app-grid-symbolic");
                image.set_pixel_size(32);
                image
            })
            .build();
        show_apps.add_css_class("flat");
        show_apps.add_css_class("overview-app");
        show_apps.update_property(&[gtk::accessible::Property::Label("Show Apps")]);
        {
            let ui = ui.clone();
            show_apps.connect_toggled(move |t| {
                ui.borrow().grid.set_visible(t.is_active());
            });
        }
        me.dash_row.append(&show_apps);
        drop(me);
        Self::rebuild_grid(ui);
    }

    fn rebuild_grid(ui: &Rc<RefCell<Self>>) {
        let me = ui.borrow();
        let flow = gtk::FlowBox::new();
        flow.set_selection_mode(gtk::SelectionMode::None);
        flow.set_max_children_per_line(8);
        flow.set_min_children_per_line(4);
        flow.set_column_spacing(24);
        flow.set_row_spacing(24);
        flow.set_halign(gtk::Align::Center);
        let mut sorted: Vec<&AppEntry> = me.apps.apps().iter().collect();
        sorted.sort_by_key(|a| a.name.to_lowercase());
        for entry in sorted {
            let button = app_button(entry, 96, true);
            let entry = entry.clone();
            let actions = me.actions.clone();
            button.connect_clicked(move |_| {
                if roost_shell_host::apps::launch(&entry).is_ok() {
                    actions.close_overview();
                }
            });
            flow.insert(&button, -1);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .child(&flow)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        me.grid.set_child(Some(&scroller));
    }

    /// Follow the compositor's overview state.
    pub fn set_open(ui: &Rc<RefCell<Self>>, open: bool) {
        if ui.borrow().open == open {
            return;
        }
        ui.borrow_mut().open = open;
        if open {
            Self::rebuild_dash(ui);
            let me = ui.borrow();
            me.entry.set_text("");
            me.results.set_visible(false);
            me.search.set_keyboard_mode(KeyboardMode::Exclusive);
            me.search.present();
            me.dash.present();
            me.entry.grab_focus();
        } else {
            let me = ui.borrow();
            me.search.set_keyboard_mode(KeyboardMode::None);
            me.search.set_visible(false);
            me.dash.set_visible(false);
            me.grid.set_visible(false);
        }
    }
}
