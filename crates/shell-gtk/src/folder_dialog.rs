//! GNOME 51's app folder dialog (appDisplay.js AppFolderDialog): opening
//! a folder in the app grid dims the overview and shows a 720px rounded
//! dialog with the folder's name, a rename button, and its apps in a
//! centred 3x3 grid of large tiles. Escape or a click outside closes it.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

/// `.app-folder-dialog`: 722x720 (a 1px edge each side of 720).
const DIALOG_W: i32 = 722;
const DIALOG_H: i32 = 720;
/// The panel stays clear of the shade; the dialog sits 24px under it.
const PANEL_H: i32 = 32;
const TOP: i32 = 24;

/// Renames the open folder.
pub type Rename = Rc<dyn Fn(&str, &str)>;
/// Hears the shade go on (true) or off.
pub type OnShade = Rc<dyn Fn(bool)>;

pub struct FolderDialog {
    window: gtk::Window,
    dialog: gtk::Box,
    title: gtk::Label,
    entry: gtk::Entry,
    name: gtk::Stack,
    edit: gtk::ToggleButton,
    grid: gtk::Grid,
    folder: RefCell<Option<String>>,
    rename: Rename,
    /// Tells the top bar the overview behind it is shaded (or not).
    on_shade: RefCell<Option<OnShade>>,
}

impl FolderDialog {
    pub fn new(app: &gtk::Application, rename: Rename) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("roost-folder-dialog");
        window.set_title(Some("App Folder"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("roost-folder-dialog"));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_margin(Edge::Top, PANEL_H);
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);

        let shade = gtk::Box::new(gtk::Orientation::Vertical, 0);
        shade.add_css_class("folder-dialog-shade");
        let dialog = gtk::Box::new(gtk::Orientation::Vertical, 0);
        dialog.add_css_class("app-folder-dialog");
        dialog.set_size_request(DIALOG_W, DIALOG_H);
        dialog.set_halign(gtk::Align::Center);
        dialog.set_valign(gtk::Align::Start);
        dialog.set_margin_top(TOP);
        dialog.set_vexpand(false);

        // The name row (.folder-name-container): the name, or its entry
        // while renaming, and the rename button at the right.
        let header = gtk::CenterBox::new();
        header.add_css_class("folder-name-container");
        let title = gtk::Label::new(None);
        title.add_css_class("folder-name-label");
        let entry = gtk::Entry::new();
        entry.add_css_class("folder-name-entry");
        let name = gtk::Stack::new();
        name.add_named(&title, Some("label"));
        name.add_named(&entry, Some("entry"));
        name.set_valign(gtk::Align::Center);
        header.set_center_widget(Some(&name));
        let edit = gtk::ToggleButton::builder()
            .icon_name("document-edit-symbolic")
            .valign(gtk::Align::Center)
            .build();
        edit.add_css_class("icon-button");
        edit.update_property(&[gtk::accessible::Property::Label("Rename Folder")]);
        header.set_end_widget(Some(&edit));
        dialog.append(&header);

        let grid = gtk::Grid::builder()
            .row_spacing(12)
            .column_spacing(12)
            .row_homogeneous(true)
            .column_homogeneous(true)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .vexpand(true)
            .build();
        grid.add_css_class("icon-grid");
        dialog.append(&grid);
        shade.append(&dialog);
        window.set_child(Some(&shade));

        let ui = Rc::new(Self {
            window,
            dialog,
            title,
            entry,
            name,
            edit,
            grid,
            folder: RefCell::new(None),
            rename,
            on_shade: RefCell::new(None),
        });
        {
            let weak = Rc::downgrade(&ui);
            ui.edit.connect_toggled(move |b| {
                let Some(ui) = weak.upgrade() else { return };
                if b.is_active() {
                    ui.entry.set_text(&ui.title.text());
                    ui.name.set_visible_child_name("entry");
                    ui.entry.grab_focus();
                } else {
                    ui.commit_rename();
                }
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            ui.entry.connect_activate(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.edit.set_active(false);
                }
            });
        }
        // A click on the shade, outside the dialog, closes it.
        let click = gtk::GestureClick::new();
        {
            let weak = Rc::downgrade(&ui);
            click.connect_pressed(move |_, _, x, y| {
                let Some(ui) = weak.upgrade() else { return };
                let inside = ui.dialog.compute_bounds(&ui.window).is_some_and(|b| {
                    b.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
                });
                if !inside {
                    ui.close();
                }
            });
        }
        ui.window.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(&ui);
            keys.connect_key_pressed(move |_, key, _, _| {
                let Some(ui) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if key == gtk::gdk::Key::Escape {
                    if ui.edit.is_active() {
                        // Escape leaves renaming without saving.
                        ui.name.set_visible_child_name("label");
                        ui.edit.set_active(false);
                    } else {
                        ui.close();
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        ui.window.add_controller(keys);
        ui
    }

    /// Who to tell when the shade goes on or off (the top bar dims the
    /// overview behind it too).
    pub fn set_on_shade(&self, f: OnShade) {
        *self.on_shade.borrow_mut() = Some(f);
    }

    fn shade(&self, on: bool) {
        let f = self.on_shade.borrow().clone();
        if let Some(f) = f {
            f(on);
        }
    }

    /// Show folder `id` named `name` with `tiles` (its apps), three a row.
    pub fn open(&self, id: &str, name: &str, tiles: Vec<gtk::Widget>) {
        *self.folder.borrow_mut() = Some(id.to_owned());
        self.title.set_label(name);
        self.name.set_visible_child_name("label");
        self.edit.set_active(false);
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        // GNOME's folder pages are 3x3; the first page shows.
        for (i, tile) in tiles.into_iter().take(9).enumerate() {
            self.grid
                .attach(&tile, (i % 3) as i32, (i / 3) as i32, 1, 1);
        }
        // Empty cells keep the grid's 3x3 size, so the tiles start where
        // GNOME's do.
        let used = self.grid.observe_children().n_items() as usize;
        for i in used..9 {
            let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
            spacer.set_size_request(145, 145);
            self.grid
                .attach(&spacer, (i % 3) as i32, (i / 3) as i32, 1, 1);
        }
        self.window.present();
        self.shade(true);
        // GNOME focuses the rename button (focus ring and all).
        self.edit.grab_focus();
    }

    /// Close the dialog (after a launch too).
    pub fn close(&self) {
        if self.edit.is_active() {
            self.edit.set_active(false);
        }
        if self.window.is_visible() {
            self.shade(false);
        }
        self.window.set_visible(false);
    }

    fn commit_rename(&self) {
        self.name.set_visible_child_name("label");
        let new = self.entry.text().trim().to_owned();
        if new.is_empty() || new == self.title.text() {
            return;
        }
        if let Some(id) = self.folder.borrow().as_deref() {
            (self.rename)(id, &new);
        }
        self.title.set_label(&new);
    }
}
