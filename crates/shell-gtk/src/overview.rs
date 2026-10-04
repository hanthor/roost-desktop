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

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use roost_shell_host::apps::AppEntry;

use crate::live_apps::LiveApps;

use crate::providers;

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
    /// Search results are showing (or not): the compositor hides the
    /// workspace view meanwhile.
    fn set_search(&self, active: bool);
    /// The app grid is showing (or not): the compositor draws the
    /// workspaces as thumbnails along the top meanwhile.
    fn set_app_grid(&self, active: bool);
    /// Open the overview (org.gnome.Shell FocusSearch, ShowApplications).
    fn open_overview(&self);
}

/// One dash item (`#dash .overview-tile`): the 64px icon in a 76px
/// rounded tile, with GNOME's running dot under it.
fn dash_tile(entry: &AppEntry, running: bool) -> gtk::Button {
    let tile = gtk::Overlay::new();
    let icon = gtk::Box::new(gtk::Orientation::Vertical, 0);
    icon.add_css_class("overview-icon");
    icon.append(&app_icon(entry, 64));
    tile.set_child(Some(&icon));
    if running {
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("app-grid-running-dot");
        dot.set_halign(gtk::Align::Center);
        dot.set_valign(gtk::Align::End);
        tile.add_overlay(&dot);
    }
    let button = gtk::Button::builder().child(&tile).build();
    button.add_css_class("dash-tile");
    if running {
        button.add_css_class("running");
    }
    button.update_property(&[gtk::accessible::Property::Label(&entry.name)]);
    button
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

/// Keep a grid tile at GNOME's 113px: its label ellipsizes inside the
/// 89px icon box instead of widening the tile.
fn fit_tile_label(button: &gtk::Button) {
    let mut child = button.first_child();
    while let Some(widget) = child {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            label.set_max_width_chars(1);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        }
        child = widget.first_child().or_else(|| widget.next_sibling());
    }
    button.set_size_request(113, 113);
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
    /// One section per search provider with hits, in provider order.
    provider_box: gtk::Box,
    /// Providers on the session bus, once it connects.
    remotes: Rc<RefCell<Vec<providers::Remote>>>,
    /// Cancels the in-flight provider search on the next keystroke.
    search_cancel: RefCell<Option<gio::Cancellable>>,
    /// Whether the compositor was last told search is active.
    search_active: std::cell::Cell<bool>,
    /// First provider hit, for Enter when no app matches.
    first_remote_hit: Rc<RefCell<Option<(providers::Remote, String)>>>,
    dash: gtk::ApplicationWindow,
    dash_row: gtk::Box,
    grid: gtk::ApplicationWindow,
    apps: Rc<LiveApps>,
    favorites: Vec<String>,
    actions: Rc<dyn OverviewActions>,
    open: bool,
    /// Open on the app grid once the overview opens (ShowApplications).
    want_apps: std::cell::Cell<bool>,
    /// GNOME's app folder dialog.
    folder_dialog: Rc<crate::folder_dialog::FolderDialog>,
}

impl OverviewUi {
    /// Build the (hidden) surfaces.
    pub fn new(
        app: &gtk::Application,
        apps: Rc<LiveApps>,
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
        entry.add_css_class("idle");
        entry.connect_search_changed(|e| {
            if e.text().is_empty() {
                e.add_css_class("idle");
            } else {
                e.remove_css_class("idle");
            }
        });
        entry.set_width_request(370);
        entry.set_halign(gtk::Align::Center);
        entry.update_property(&[gtk::accessible::Property::Label("Search")]);
        let results = gtk::Box::new(gtk::Orientation::Horizontal, 30);
        results.add_css_class("overview-results");
        results.set_halign(gtk::Align::Center);
        results.set_visible(false);
        let provider_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        provider_box.add_css_class("overview-providers");
        provider_box.set_halign(gtk::Align::Center);
        provider_box.set_visible(false);
        column.append(&entry);
        column.append(&results);
        column.append(&provider_box);
        search.set_child(Some(&column));

        // Search providers: discovered once, bound when the bus is up.
        let remotes = Rc::new(RefCell::new(Vec::new()));
        {
            let remotes = remotes.clone();
            let apps = apps.clone();
            gio::bus_get(
                gio::BusType::Session,
                None::<&gio::Cancellable>,
                move |conn| {
                    let Ok(conn) = conn else {
                        return;
                    };
                    let found = providers::discover(&providers::data_dirs());
                    let chosen =
                        providers::select(found, &providers::ProviderSettings::load(), |id| {
                            providers::provider_app(&apps.get(), id).is_some()
                        });
                    eprintln!(
                        "roost-shell-gtk: search providers: {}",
                        chosen
                            .iter()
                            .map(|p| p.desktop_id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    *remotes.borrow_mut() = chosen
                        .into_iter()
                        .map(|info| providers::Remote::new(info, &conn))
                        .collect();
                },
            );
        }

        // Dash: bottom center.
        let dash = layer_window(app, "roost-shell-dash", "roost-overview-dash");
        dash.set_anchor(Edge::Bottom, true);
        dash.set_margin(Edge::Bottom, 12);
        dash.set_keyboard_mode(KeyboardMode::None);
        dash.set_title(Some("Dash"));
        let dash_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dash_row.add_css_class("overview-dash");
        dash.set_child(Some(&dash_row));

        // App grid: between search and dash, over the previews.
        let grid = layer_window(app, "roost-shell-appgrid", "roost-overview-grid");
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            grid.set_anchor(edge, true);
        }
        // GNOME's apps scroll view: 232..673 on 1280x800, below the
        // workspace thumbnails and above the dash.
        grid.set_margin(Edge::Top, 200);
        grid.set_margin(Edge::Bottom, 127);
        grid.set_keyboard_mode(KeyboardMode::None);
        grid.set_title(Some("App Grid"));

        let ui = Rc::new(RefCell::new(Self {
            search,
            entry,
            results,
            provider_box,
            remotes,
            search_cancel: RefCell::new(None),
            search_active: std::cell::Cell::new(false),
            first_remote_hit: Rc::new(RefCell::new(None)),
            dash,
            dash_row,
            grid,
            apps,
            favorites,
            actions,
            open: false,
            want_apps: std::cell::Cell::new(false),
            folder_dialog: crate::folder_dialog::FolderDialog::new(
                app,
                Rc::new(crate::folders::rename),
            ),
        }));
        // An app dragged out of a folder's dialog leaves the folder.
        {
            let weak = Rc::downgrade(&ui);
            ui.borrow()
                .folder_dialog
                .set_on_remove(Rc::new(move |folder, app| {
                    crate::folders::remove_app(&folder, &app);
                    let weak = weak.clone();
                    gtk::glib::idle_add_local_once(move || {
                        if let Some(ui) = weak.upgrade() {
                            Self::rebuild_grid(&ui);
                        }
                    });
                }));
        }
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
                    let apps = me.apps.get();
                    let first = crate::logic::rank_apps(apps.apps(), &entry.text(), 1)
                        .first()
                        .map(|e| (*e).clone());
                    first
                };
                if let Some(app) = first {
                    ui.borrow().launch(&app);
                } else {
                    // No app matched: Enter opens the first provider hit.
                    let me = ui.borrow();
                    let hit = me.first_remote_hit.borrow().clone();
                    if let Some((remote, id)) = hit {
                        remote.activate(&id, providers::terms(&entry.text()));
                        me.actions.close_overview();
                    }
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
        let active = !query.trim().is_empty();
        if self.search_active.replace(active) != active {
            self.actions.set_search(active);
        }
        while let Some(child) = self.results.first_child() {
            self.results.remove(&child);
        }
        let apps = self.apps.get();
        let hits = crate::logic::rank_apps(apps.apps(), query, MAX_RESULTS);
        for (i, hit) in hits.iter().enumerate() {
            let button = app_button(hit, 96, true);
            // GNOME's 145px result tile; the first is Enter's target.
            fit_tile_label(&button);
            button.set_size_request(145, 145);
            if i == 0 {
                button.add_css_class("selected");
            }
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
        self.search_providers(query);
    }

    /// Ask every provider, in order; each fills its own section when it
    /// answers. A newer query cancels this one.
    fn search_providers(&self, query: &str) {
        if let Some(old) = self.search_cancel.borrow_mut().take() {
            old.cancel();
        }
        while let Some(child) = self.provider_box.first_child() {
            self.provider_box.remove(&child);
        }
        *self.first_remote_hit.borrow_mut() = None;
        let terms = providers::terms(query);
        if terms.is_empty() {
            self.provider_box.set_visible(false);
            return;
        }
        let cancel = gio::Cancellable::new();
        *self.search_cancel.borrow_mut() = Some(cancel.clone());
        for remote in self.remotes.borrow().iter() {
            // Sections exist up front so answers keep provider order.
            let section = gtk::Box::new(gtk::Orientation::Horizontal, 18);
            section.add_css_class("provider-section");
            section.set_visible(false);
            self.provider_box.append(&section);
            let (section2, box2) = (section.clone(), self.provider_box.clone());
            let (remote2, terms2) = (remote.clone(), terms.clone());
            let app = providers::provider_app(&self.apps.get(), &remote.info.desktop_id).cloned();
            let actions = self.actions.clone();
            let first = self.first_remote_hit.clone();
            remote.search(terms.clone(), &cancel, move |metas| {
                if metas.is_empty() {
                    return;
                }
                fill_section(&section2, &remote2, app.as_ref(), &metas, &terms2, actions);
                section2.set_visible(true);
                box2.set_visible(true);
                let mut first = first.borrow_mut();
                if first.is_none() {
                    *first = Some((remote2.clone(), metas[0].id.clone()));
                }
            });
        }
    }

    fn rebuild_dash(ui: &Rc<RefCell<Self>>) {
        let me = ui.borrow();
        while let Some(child) = me.dash_row.first_child() {
            me.dash_row.remove(&child);
        }
        // GNOME's dash (dash.js): favorites, then running apps that are
        // not favorites after a separator, then Show Apps.
        // Running apps in the order their windows appeared.
        let mut running = me.actions.running();
        running.sort_by_key(|(id, _)| *id);
        let apps = me.apps.get();
        let window_of = |entry: &AppEntry| {
            running
                .iter()
                .find(|(_, app)| app.as_deref() == Some(entry.app_id.trim_end_matches(".desktop")))
                .map(|(id, _)| *id)
        };
        let mut shown: Vec<AppEntry> = me
            .favorites
            .iter()
            .filter_map(|id| providers::provider_app(&apps, id).cloned())
            .collect();
        let favorites = shown.len();
        // Running windows no installed app claims become window-backed
        // apps, as GNOME's WindowTracker makes them: one per app id (or
        // window), with the generic icon.
        let mut orphans: Vec<(u64, String)> = Vec::new();
        for (window, app) in &running {
            match app
                .as_deref()
                .and_then(|app| providers::provider_app(&apps, app))
            {
                Some(entry) => {
                    if !shown.iter().any(|e| e.app_id == entry.app_id) {
                        shown.push(entry.clone());
                    }
                }
                None => {
                    let key = app.clone().unwrap_or_else(|| format!("window:{window}"));
                    if !orphans.iter().any(|(_, k)| *k == key) {
                        orphans.push((*window, key));
                    }
                }
            }
        }
        let orphans_need_separator = favorites == shown.len() && !orphans.is_empty();
        for (i, entry) in shown.iter().enumerate() {
            if i == favorites {
                let separator = gtk::Separator::new(gtk::Orientation::Vertical);
                separator.add_css_class("dash-separator");
                separator.set_valign(gtk::Align::Center);
                me.dash_row.append(&separator);
            }
            let window = window_of(entry);
            let button = dash_tile(entry, window.is_some());
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
        if orphans_need_separator {
            let separator = gtk::Separator::new(gtk::Orientation::Vertical);
            separator.add_css_class("dash-separator");
            separator.set_valign(gtk::Align::Center);
            me.dash_row.append(&separator);
        }
        for (window, name) in orphans {
            let entry = AppEntry {
                app_id: name.clone(),
                name,
                generic_name: None,
                keywords: Vec::new(),
                argv: Vec::new(),
                icon: Some("application-x-executable".to_owned()),
                categories: Vec::new(),
            };
            let button = dash_tile(&entry, true);
            let actions = me.actions.clone();
            button.connect_clicked(move |_| {
                actions.activate_window(window);
                actions.close_overview();
            });
            me.dash_row.append(&button);
        }
        let show_apps = gtk::ToggleButton::builder()
            .child(&{
                let icon = gtk::Box::new(gtk::Orientation::Vertical, 0);
                icon.add_css_class("overview-icon");
                // GNOME draws the 16px grid glyph at the full 64px.
                let image = gtk::Image::from_icon_name("view-app-grid-symbolic");
                image.set_pixel_size(64);
                icon.append(&image);
                icon
            })
            .build();
        show_apps.add_css_class("dash-tile");
        show_apps.update_property(&[gtk::accessible::Property::Label("Show Apps")]);
        {
            let ui = ui.clone();
            show_apps.connect_toggled(move |t| {
                let me = ui.borrow();
                me.grid.set_visible(t.is_active());
                me.actions.set_app_grid(t.is_active());
            });
        }
        me.dash_row.append(&show_apps);
        drop(apps);
        drop(me);
        Self::rebuild_grid(ui);
    }

    fn rebuild_grid(ui: &Rc<RefCell<Self>>) {
        let me = ui.borrow();
        // GNOME's paged icon grid: its mode (columns x rows) from the
        // area's shape, 113px tiles 12px apart, 24px below the top.
        let (mon_w, mon_h) = gtk::gdk::Display::default()
            .and_then(|d| d.monitors().item(0))
            .and_downcast::<gtk::gdk::Monitor>()
            .map(|m| (m.geometry().width(), m.geometry().height()))
            .unwrap_or((1280, 800));
        // The grid sits between the search entry and the dash.
        let (columns, rows) = crate::logic::grid_mode(mon_w, mon_h - 250);
        let per_page = columns * rows;
        let new_page = || {
            let flow = gtk::FlowBox::new();
            flow.set_selection_mode(gtk::SelectionMode::None);
            flow.set_homogeneous(true);
            flow.set_max_children_per_line(columns as u32);
            flow.set_min_children_per_line(columns as u32);
            flow.set_column_spacing(12);
            flow.set_row_spacing(12);
            flow.set_halign(gtk::Align::Center);
            flow.set_valign(gtk::Align::Start);
            flow.set_margin_top(24);
            flow.set_hexpand(true);
            flow.set_vexpand(true);
            // All columns even when fewer apps fill them: GNOME's grid
            // starts at the same column however many apps there are.
            flow.set_size_request(columns as i32 * 113 + (columns as i32 - 1) * 12, -1);
            flow.add_css_class("icon-grid");
            flow
        };
        let mut pages: Vec<gtk::FlowBox> = vec![new_page()];
        let mut on_page = 0usize;
        let mut add = |widget: &gtk::Widget| {
            if on_page == per_page {
                pages.push(new_page());
                on_page = 0;
            }
            if let Some(page) = pages.last() {
                page.insert(widget, -1);
            }
            on_page += 1;
        };
        // GNOME's app-folders: folders and loose apps share one
        // alphabetical grid; a folder opens its apps in a popover.
        let apps = me.apps.get();
        crate::folders::ensure_defaults(apps.apps());
        let folders = crate::folders::load();
        let (filled, loose) = crate::folders::arrange(&folders, apps.apps(), &me.favorites);
        enum Item<'a> {
            Folder(crate::folders::Folder, Vec<&'a AppEntry>),
            App(&'a AppEntry),
        }
        // Keyed as app-picker-layout keys them: desktop ids, folder ids.
        let unordered: Vec<(String, String, Item)> = filled
            .into_iter()
            .map(|(f, members)| (f.id.clone(), f.name.clone(), Item::Folder(f, members)))
            .chain(loose.into_iter().map(|a| {
                (
                    format!("{}.desktop", a.app_id),
                    a.name.clone(),
                    Item::App(a),
                )
            }))
            .collect();
        // GNOME's order: the saved layout, then the rest by name.
        let keys: Vec<(&str, &str)> = unordered
            .iter()
            .map(|(id, name, _)| (id.as_str(), name.as_str()))
            .collect();
        let ranks = crate::logic::grid_order(&keys, &crate::folders::picker_layout());
        let mut slots: Vec<Option<(String, String, Item)>> =
            unordered.into_iter().map(Some).collect();
        let items: Vec<(String, Item)> = ranks
            .into_iter()
            .filter_map(|i| slots[i].take())
            .map(|(id, _, item)| (id, item))
            .collect();
        let order: Rc<Vec<String>> = Rc::new(items.iter().map(|(id, _)| id.clone()).collect());
        let weak_ui = Rc::downgrade(ui);
        // GNOME's drop between tiles moves the dragged item there and
        // saves the grid (app-picker-layout).
        let reorder = {
            let (order, weak_ui) = (order.clone(), weak_ui.clone());
            Rc::new(
                move |text: &str, target: &str, edge: crate::logic::DropEdge| {
                    let source = match text.split_once(':') {
                        Some(("app", id)) => format!("{id}.desktop"),
                        Some(("folder", id)) => id.to_owned(),
                        _ => return None,
                    };
                    let index = order.iter().position(|id| id == target)?;
                    let moved = crate::logic::grid_reorder(&order, &source, index, edge, columns)?;
                    crate::folders::save_picker_layout(&crate::logic::grid_pages(&moved, per_page));
                    Some(weak_ui.clone())
                },
            )
        };
        let live_apps = me.apps.clone();
        let launch_button = |entry: &AppEntry, size: i32, actions: Rc<dyn OverviewActions>| {
            let key = format!("{}.desktop", entry.app_id);
            let button = app_button(entry, size, true);
            button.add_css_class("grid-tile");
            fit_tile_label(&button);
            // GNOME's grid edits: drag an app onto another to make a
            // folder of the two.
            drag_source(&button, format!("app:{}", entry.app_id));
            {
                let (target, weak_ui, live_apps, reorder) = (
                    entry.clone(),
                    weak_ui.clone(),
                    live_apps.clone(),
                    reorder.clone(),
                );
                on_drop(&button, move |text, edge| {
                    if edge != crate::logic::DropEdge::OnIcon {
                        return reorder(text, &key, edge);
                    }
                    let dragged = text.strip_prefix("app:")?.to_owned();
                    if dragged == target.app_id {
                        return None;
                    }
                    let apps = live_apps.get();
                    let other = apps.apps().iter().find(|a| a.app_id == dragged)?;
                    let folder = crate::folders::create(&[&target, other])?;
                    // The folder takes the target's place in a saved grid.
                    let pages = crate::logic::grid_pages_replace(
                        &crate::folders::picker_layout(),
                        &key,
                        &folder,
                        &format!("{dragged}.desktop"),
                    );
                    if let Some(pages) = pages {
                        crate::folders::save_picker_layout(&pages);
                    }
                    Some(weak_ui.clone())
                });
            }
            let entry = entry.clone();
            button.connect_clicked(move |_| {
                if roost_shell_host::apps::launch(&entry).is_ok() {
                    actions.close_overview();
                }
            });
            button
        };
        for (_, item) in items {
            match item {
                Item::App(entry) => {
                    add(launch_button(entry, 64, me.actions.clone()).upcast_ref());
                }
                Item::Folder(folder, members) => {
                    // GNOME's createFolderIcon: a 64px square of four
                    // 32px cells, the first apps' icons at 40% (25px).
                    let collage = gtk::Grid::builder()
                        .row_homogeneous(true)
                        .column_homogeneous(true)
                        .halign(gtk::Align::Center)
                        .build();
                    collage.add_css_class("folder-collage");
                    for i in 0..4 {
                        let cell = gtk::Box::new(gtk::Orientation::Vertical, 0);
                        cell.set_size_request(32, 32);
                        if let Some(entry) = members.get(i) {
                            let icon = app_icon(entry, 25);
                            icon.set_halign(gtk::Align::Center);
                            icon.set_valign(gtk::Align::Center);
                            icon.set_vexpand(true);
                            cell.append(&icon);
                        }
                        collage.attach(&cell, (i % 2) as i32, (i / 2) as i32, 1, 1);
                    }
                    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
                    column.append(&collage);
                    column.append(&gtk::Label::new(Some(&folder.name)));
                    let button = gtk::Button::builder()
                        .child(&column)
                        .valign(gtk::Align::Start)
                        .build();
                    button.add_css_class("flat");
                    button.add_css_class("overview-app");
                    button.add_css_class("app-folder");
                    button.add_css_class("grid-tile");
                    button.update_property(&[gtk::accessible::Property::Label(&folder.name)]);
                    // An app dropped on a folder joins it; between tiles,
                    // folders move like apps.
                    drag_source(&button, format!("folder:{}", folder.id));
                    {
                        let (fid, weak_ui, reorder) =
                            (folder.id.clone(), weak_ui.clone(), reorder.clone());
                        on_drop(&button, move |text, edge| {
                            if edge != crate::logic::DropEdge::OnIcon {
                                return reorder(text, &fid, edge);
                            }
                            let dragged = text.strip_prefix("app:")?;
                            crate::folders::add_app(&fid, dragged);
                            Some(weak_ui.clone())
                        });
                    }
                    // GNOME's folder dialog with the apps as large tiles.
                    {
                        let dialog = me.folder_dialog.clone();
                        let actions = me.actions.clone();
                        let members: Vec<AppEntry> = members.iter().map(|e| (*e).clone()).collect();
                        let (id, name) = (folder.id.clone(), folder.name.clone());
                        button.connect_clicked(move |_| {
                            let tiles = members
                                .iter()
                                .map(|entry| {
                                    let tile = app_button(entry, 96, true);
                                    tile.add_css_class("folder-tile");
                                    // Dragged out onto the shade, it leaves.
                                    drag_source(&tile, format!("folder-app:{id}:{}", entry.app_id));
                                    let (entry, actions, dialog) =
                                        (entry.clone(), actions.clone(), dialog.clone());
                                    tile.connect_clicked(move |_| {
                                        if roost_shell_host::apps::launch(&entry).is_ok() {
                                            dialog.close();
                                            actions.close_overview();
                                        }
                                    });
                                    tile.upcast::<gtk::Widget>()
                                })
                                .collect();
                            dialog.open(&id, &name, tiles);
                        });
                    }
                    add(button.upcast_ref());
                }
            }
        }
        me.grid.set_child(Some(&paged_grid(pages)));
    }

    /// Who to tell when the folder dialog shades the overview.
    pub fn set_folder_shade(ui: &Rc<RefCell<Self>>, f: crate::folder_dialog::OnShade) {
        ui.borrow().folder_dialog.set_on_shade(f);
    }

    /// GNOME's `Main.overview.showApps()`: the overview on its app grid.
    pub fn show_apps(ui: &Rc<RefCell<Self>>) {
        let me = ui.borrow();
        if me.open {
            if let Some(toggle) = me
                .dash_row
                .last_child()
                .and_then(|w| w.downcast::<gtk::ToggleButton>().ok())
            {
                drop(me);
                toggle.set_active(true);
            }
        } else {
            me.want_apps.set(true);
            me.actions.open_overview();
        }
    }

    /// GNOME's toggle-application-view: the app grid, or out of the
    /// overview when the grid already shows.
    pub fn toggle_apps(ui: &Rc<RefCell<Self>>) {
        let showing = {
            let me = ui.borrow();
            me.open && me.grid.is_visible()
        };
        if showing {
            ui.borrow().actions.close_overview();
        } else {
            Self::show_apps(ui);
        }
    }

    /// GNOME's `Main.overview.focusSearch()`: the overview with the
    /// search entry focused.
    pub fn focus_search(ui: &Rc<RefCell<Self>>) {
        let me = ui.borrow();
        if me.open {
            if let Some(toggle) = me
                .dash_row
                .last_child()
                .and_then(|w| w.downcast::<gtk::ToggleButton>().ok())
            {
                toggle.set_active(false);
            }
            me.entry.grab_focus();
        } else {
            me.actions.open_overview();
        }
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
            // The compositor resets search on close; start clean.
            me.search_active.set(false);
            me.entry.set_text("");
            me.results.set_visible(false);
            me.search.set_keyboard_mode(KeyboardMode::Exclusive);
            me.search.present();
            me.dash.present();
            me.entry.grab_focus();
            if me.want_apps.replace(false) {
                drop(me);
                Self::show_apps(ui);
            }
        } else {
            let me = ui.borrow();
            me.folder_dialog.close();
            me.search.set_keyboard_mode(KeyboardMode::None);
            me.search.set_visible(false);
            me.dash.set_visible(false);
            me.grid.set_visible(false);
        }
    }
}

/// One provider's results: its app on the left (opens the app on the
/// search), result rows on the right (each opens itself).
fn fill_section(
    section: &gtk::Box,
    remote: &providers::Remote,
    app: Option<&AppEntry>,
    metas: &[providers::ResultMeta],
    terms: &[String],
    actions: Rc<dyn OverviewActions>,
) {
    let name = app
        .map(|a| a.name.clone())
        .unwrap_or_else(|| remote.info.desktop_id.clone());
    let provider = match app {
        Some(app) => app_button(app, 48, true),
        None => gtk::Button::with_label(&name),
    };
    provider.add_css_class("provider-app");
    provider.set_valign(gtk::Align::Start);
    {
        let (remote, terms, actions) = (remote.clone(), terms.to_vec(), actions.clone());
        provider.connect_clicked(move |_| {
            remote.launch_search(terms.clone());
            actions.close_overview();
        });
    }
    section.append(&provider);
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 4);
    for meta in metas {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let icon = match &meta.icon {
            Some(icon) => gtk::Image::from_gicon(icon),
            None => gtk::Image::from_icon_name("text-x-generic"),
        };
        icon.set_pixel_size(32);
        row.append(&icon);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let title = gtk::Label::new(Some(&meta.name));
        title.set_halign(gtk::Align::Start);
        title.add_css_class("provider-result-name");
        text.append(&title);
        if let Some(desc) = &meta.description {
            let d = gtk::Label::new(Some(desc));
            d.set_halign(gtk::Align::Start);
            d.add_css_class("caption");
            d.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            d.set_max_width_chars(48);
            text.append(&d);
        }
        row.append(&text);
        let button = gtk::Button::builder().child(&row).build();
        button.add_css_class("provider-result");
        button.update_property(&[gtk::accessible::Property::Label(&meta.name)]);
        let (remote, terms, actions, id) = (
            remote.clone(),
            terms.to_vec(),
            actions.clone(),
            meta.id.clone(),
        );
        button.connect_clicked(move |_| {
            remote.activate(&id, terms.clone());
            actions.close_overview();
        });
        rows.append(&button);
    }
    section.append(&rows);
}

/// GNOME's paged app grid (appDisplay.js): the pages side by side,
/// flipped by the wheel, a swipe, PageUp/PageDown, the side arrows or
/// the indicators; arrows and indicators only with more than one page.
fn paged_grid(pages: Vec<gtk::FlowBox>) -> gtk::Widget {
    use libadwaita as adw;
    let carousel = adw::Carousel::new();
    carousel.set_allow_scroll_wheel(true);
    carousel.set_hexpand(true);
    carousel.set_vexpand(true);
    let n = pages.len();
    for page in &pages {
        carousel.append(page);
    }
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&carousel));
    column.append(&overlay);
    if n <= 1 {
        return column.upcast();
    }
    // The page indicators (pageIndicators.js): 10px dots, the inactive
    // ones at 2/3 size and half opacity.
    let indicators = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    indicators.add_css_class("page-indicators");
    indicators.set_halign(gtk::Align::Center);
    let dots: Vec<gtk::Button> = (0..n)
        .map(|i| {
            let dot = gtk::Box::new(gtk::Orientation::Vertical, 0);
            dot.add_css_class("page-indicator-icon");
            let button = gtk::Button::builder().child(&dot).build();
            button.add_css_class("page-indicator");
            button.update_property(&[gtk::accessible::Property::Label(&format!("Page {}", i + 1))]);
            let carousel = carousel.clone();
            button.connect_clicked(move |_| {
                carousel.scroll_to(&carousel.nth_page(i as u32), true);
            });
            indicators.append(&button);
            button
        })
        .collect();
    column.append(&indicators);
    // The side arrows (page-navigation-arrow).
    let arrow = |icon: &str, label: &str, align: gtk::Align, step: i32| {
        let button = gtk::Button::from_icon_name(icon);
        button.add_css_class("page-navigation-arrow");
        button.set_halign(align);
        button.set_valign(gtk::Align::Center);
        button.update_property(&[gtk::accessible::Property::Label(label)]);
        let carousel = carousel.clone();
        button.connect_clicked(move |_| step_page(&carousel, step));
        overlay.add_overlay(&button);
        button
    };
    let previous = arrow(
        "go-previous-symbolic",
        "Previous Page",
        gtk::Align::Start,
        -1,
    );
    let next = arrow("go-next-symbolic", "Next Page", gtk::Align::End, 1);
    // GNOME's page hints (.page-navigation-hint): while an item is
    // dragged they take the arrows' place at the sides, a tenth of the
    // grid wide each, lit (.dnd) while the drag is over them.
    let hint = |side: &str, align: gtk::Align| {
        let hint = gtk::Box::new(gtk::Orientation::Vertical, 0);
        hint.add_css_class("page-navigation-hint");
        hint.add_css_class(side);
        hint.set_halign(align);
        hint.set_can_target(false);
        hint.set_visible(false);
        overlay.add_overlay(&hint);
        hint
    };
    let pager = Rc::new(DragPager {
        carousel: carousel.clone(),
        previous_hint: hint("previous", gtk::Align::Start),
        next_hint: hint("next", gtk::Align::End),
        previous: previous.clone(),
        next: next.clone(),
        pages: n,
        dragging: Cell::new(false),
        initial: RefCell::new(None),
        repeat: RefCell::new(None),
        overshoot: Cell::new(-1.0),
    });
    let sync = {
        let (dots, pager) = (dots.clone(), pager.clone());
        move |page: u32| {
            for (i, dot) in dots.iter().enumerate() {
                if i as u32 == page {
                    dot.add_css_class("active");
                } else {
                    dot.remove_css_class("active");
                }
            }
            pager.sync(page);
        }
    };
    sync(0);
    carousel.connect_page_changed(move |_, page| sync(page));
    let motion = gtk::DropControllerMotion::new();
    {
        let pager = pager.clone();
        motion.connect_enter(move |m, x, _| {
            let width = m.widget().map_or(0, |w| w.width());
            pager.begin(width);
            pager.motion(x, width);
        });
    }
    {
        let pager = pager.clone();
        motion.connect_motion(move |m, x, _| {
            let width = m.widget().map_or(0, |w| w.width());
            pager.motion(x, width);
        });
    }
    motion.connect_leave(move |_| pager.end());
    overlay.add_controller(motion);
    // PageUp / PageDown, as GNOME's grid binds them.
    let keys = gtk::EventControllerKey::new();
    {
        let carousel = carousel.clone();
        keys.connect_key_pressed(move |_, key, _, _| match key {
            gtk::gdk::Key::Page_Down => {
                step_page(&carousel, 1);
                gtk::glib::Propagation::Stop
            }
            gtk::gdk::Key::Page_Up => {
                step_page(&carousel, -1);
                gtk::glib::Propagation::Stop
            }
            _ => gtk::glib::Propagation::Proceed,
        });
    }
    column.add_controller(keys);
    column.upcast()
}

/// One page forward or back, clamped.
/// GNOME's page switching during a drag (appDisplay.js): bumping the
/// pointer within 20px of the grid's edge turns the page at once, and
/// hovering a page hint turns it after a second; either repeats every
/// second while the pointer stays.
struct DragPager {
    carousel: libadwaita::Carousel,
    previous_hint: gtk::Box,
    next_hint: gtk::Box,
    previous: gtk::Button,
    next: gtk::Button,
    pages: usize,
    dragging: Cell<bool>,
    initial: RefCell<Option<glib::SourceId>>,
    repeat: RefCell<Option<glib::SourceId>>,
    /// Where the pointer last bumped the edge (-1: not since leaving).
    overshoot: Cell<f64>,
}

/// `DRAG_PAGE_SWITCH_IMMEDIATELY_THRESHOLD_PX`.
const DRAG_EDGE_PX: f64 = 20.0;
/// `DRAG_PAGE_SWITCH_INITIAL_TIMEOUT` and `_REPEAT_TIMEOUT`.
const DRAG_PAGE_SWITCH: std::time::Duration = std::time::Duration::from_millis(1000);

impl DragPager {
    fn page(&self) -> u32 {
        self.carousel.position().round() as u32
    }

    /// Arrows outside a drag, hints (where a page lies) during one.
    fn sync(&self, page: u32) {
        let before = page > 0;
        let after = (page as usize) + 1 < self.pages;
        let dragging = self.dragging.get();
        self.previous.set_visible(before && !dragging);
        self.next.set_visible(after && !dragging);
        self.previous_hint.set_visible(before && dragging);
        self.next_hint.set_visible(after && dragging);
    }

    fn begin(&self, width: i32) {
        // A tenth of the grid each (PAGE_PREVIEW_RATIO / 2).
        let w = (f64::from(width) * 0.1) as i32;
        self.previous_hint.set_size_request(w, -1);
        self.next_hint.set_size_request(w, -1);
        self.dragging.set(true);
        self.sync(self.page());
    }

    fn end(&self) {
        self.reset();
        self.dragging.set(false);
        self.previous_hint.remove_css_class("dnd");
        self.next_hint.remove_css_class("dnd");
        self.sync(self.page());
    }

    fn reset(&self) {
        if let Some(id) = self.initial.borrow_mut().take() {
            id.remove();
        }
        if let Some(id) = self.repeat.borrow_mut().take() {
            id.remove();
        }
        self.overshoot.set(-1.0);
    }

    fn turn_and_repeat(self: &Rc<Self>, step: i32) {
        step_page(&self.carousel, step);
        if let Some(id) = self.repeat.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(DRAG_PAGE_SWITCH, move || match weak.upgrade() {
            Some(pager) => {
                step_page(&pager.carousel, step);
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        *self.repeat.borrow_mut() = Some(id);
    }

    fn motion(self: &Rc<Self>, x: f64, width: i32) {
        let width = f64::from(width);
        // 1) The edge: at once (_dragMaybeSwitchPageImmediately).
        if x > DRAG_EDGE_PX && x < width - DRAG_EDGE_PX {
            let last = self.overshoot.get();
            if last >= 0.0 && (last - x).abs() > DRAG_EDGE_PX {
                self.reset();
            }
        } else if self.overshoot.get() < 0.0 {
            self.reset();
            self.turn_and_repeat(if x <= DRAG_EDGE_PX { -1 } else { 1 });
            self.overshoot.set(x);
            return;
        } else {
            return;
        }
        // 2) A hint: after a second (_maybeSetupDragPageSwitchInitialTimeout).
        let over_previous =
            self.previous_hint.is_visible() && x < f64::from(self.previous_hint.width());
        let over_next =
            self.next_hint.is_visible() && x > width - f64::from(self.next_hint.width());
        for (hint, over) in [
            (&self.previous_hint, over_previous),
            (&self.next_hint, over_next),
        ] {
            if over {
                hint.add_css_class("dnd");
            } else {
                hint.remove_css_class("dnd");
            }
        }
        if !over_previous && !over_next {
            if self.overshoot.get() < 0.0 {
                self.reset();
            }
            return;
        }
        if self.initial.borrow().is_some() || self.repeat.borrow().is_some() {
            return;
        }
        let step = if over_previous { -1 } else { 1 };
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(DRAG_PAGE_SWITCH, move || {
            if let Some(pager) = weak.upgrade() {
                pager.initial.borrow_mut().take();
                pager.turn_and_repeat(step);
            }
        });
        *self.initial.borrow_mut() = Some(id);
    }
}

fn step_page(carousel: &libadwaita::Carousel, step: i32) {
    let current = carousel.position().round() as i32;
    let target = (current + step).clamp(0, carousel.n_pages() as i32 - 1);
    let page = carousel.nth_page(target as u32);
    carousel.scroll_to(&page, true);
}

/// Make `widget` a drag source of `text`, the widget itself as the
/// icon (GNOME drags the tile).
fn drag_source(widget: &impl IsA<gtk::Widget>, text: String) {
    let source = gtk::DragSource::new();
    source.set_actions(gtk::gdk::DragAction::MOVE);
    source.set_content(Some(&gtk::gdk::ContentProvider::for_value(
        &text.to_value(),
    )));
    let w = widget.clone().upcast::<gtk::Widget>();
    source.connect_drag_begin(move |source, _| {
        let paintable = gtk::WidgetPaintable::new(Some(&w));
        source.set_icon(Some(&paintable), w.width() / 2, w.height() / 2);
    });
    widget.add_controller(source);
}

/// Accept a dropped string on `widget`: `act` edits the folders and
/// returns the overview to rebuild (or `None` to refuse the drop).
fn on_drop(
    widget: &impl IsA<gtk::Widget>,
    act: impl Fn(&str, crate::logic::DropEdge) -> Option<std::rc::Weak<RefCell<OverviewUi>>> + 'static,
) {
    let target = gtk::DropTarget::new(gtk::glib::Type::STRING, gtk::gdk::DragAction::MOVE);
    target.connect_drop(move |t, value, x, _| {
        let Ok(text) = value.get::<String>() else {
            return false;
        };
        let width = t.widget().map_or(0, |w| w.width());
        let edge = crate::logic::drop_edge(x, f64::from(width));
        let Some(ui) = act(&text, edge) else {
            return false;
        };
        // Rebuilt after the drop finishes: the tiles go away with it.
        gtk::glib::idle_add_local_once(move || {
            if let Some(ui) = ui.upgrade() {
                OverviewUi::rebuild_grid(&ui);
            }
        });
        true
    });
    widget.add_controller(target);
}
