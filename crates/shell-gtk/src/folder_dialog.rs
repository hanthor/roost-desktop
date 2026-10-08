//! GNOME 51's app folder dialog (appDisplay.js AppFolderDialog): opening
//! a folder in the app grid dims the overview and shows a 720px rounded
//! dialog with the folder's name, a rename button, and its apps in a
//! centred 3x3 grid of large tiles. Escape or a click outside closes it.
//! It zooms out of its folder's icon and back into it over 200 ms
//! (`_zoomAndFadeIn`, `_zoomAndFadeOut`), the shade darkening with it,
//! while the icon in the grid fades out and back.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::grid_animation::{self as anim, Slot, Zoom};

/// Called with (folder id, app id) when an app is dragged out of the
/// folder onto the shade.
type OnRemove = Rc<dyn Fn(String, String)>;

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
    shade_box: gtk::Box,
    zoom: Zoom,
    dialog: gtk::Box,
    /// The dialog's zoom, fade and shade.
    slot: Rc<Slot>,
    /// The folder's icon in the grid, and its fade.
    source: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    source_slot: Rc<Slot>,
    title: gtk::Label,
    entry: gtk::Entry,
    name: gtk::Stack,
    edit: gtk::ToggleButton,
    grid: gtk::Grid,
    folder: RefCell<Option<String>>,
    rename: Rename,
    /// Tells the top bar the overview behind it is shaded (or not).
    on_shade: RefCell<Option<OnShade>>,
    /// An app dragged out of the folder: (folder id, app id).
    on_remove: RefCell<Option<OnRemove>>,
}

impl FolderDialog {
    pub fn new(app: &gtk::Application, rename: Rename) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("tuna-folder-dialog");
        window.set_title(Some("App Folder"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::FOLDER_DIALOG_NAMESPACE));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_margin(Edge::Top, PANEL_H);
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);

        // The shade under the dialog, so each fades on its own.
        let overlay = gtk::Overlay::new();
        let shade = gtk::Box::new(gtk::Orientation::Vertical, 0);
        shade.add_css_class("folder-dialog-shade");
        shade.set_hexpand(true);
        shade.set_vexpand(true);
        overlay.set_child(Some(&shade));
        let dialog = gtk::Box::new(gtk::Orientation::Vertical, 0);
        dialog.add_css_class("app-folder-dialog");
        dialog.set_size_request(DIALOG_W, DIALOG_H);
        dialog.set_vexpand(false);
        let zoom = Zoom::new(&dialog);
        zoom.set_pivot(0.0, 0.0);
        zoom.set_halign(gtk::Align::Center);
        zoom.set_valign(gtk::Align::Start);
        zoom.set_margin_top(TOP);
        overlay.add_overlay(&zoom);

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
        window.set_child(Some(&overlay));

        let ui = Rc::new(Self {
            window,
            shade_box: shade,
            zoom,
            dialog,
            slot: Rc::new(Slot::default()),
            source: RefCell::new(None),
            source_slot: Rc::new(Slot::default()),
            title,
            entry,
            name,
            edit,
            grid,
            folder: RefCell::new(None),
            rename,
            on_shade: RefCell::new(None),
            on_remove: RefCell::new(None),
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
        // An app dragged out of the dialog, onto the shade, leaves the
        // folder (GNOME's FolderView removeApp); dropped back on the
        // dialog it stays.
        let drop = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
        {
            let weak = Rc::downgrade(&ui);
            drop.connect_drop(move |_, value, x, y| {
                let Some(ui) = weak.upgrade() else {
                    return false;
                };
                let Ok(text) = value.get::<String>() else {
                    return false;
                };
                let Some((folder, app)) = text
                    .strip_prefix("folder-app:")
                    .and_then(|rest| rest.split_once(':'))
                else {
                    return false;
                };
                let inside = ui.dialog.compute_bounds(&ui.window).is_some_and(|b| {
                    b.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
                });
                if !inside {
                    let f = ui.on_remove.borrow().clone();
                    if let Some(f) = f {
                        f(folder.to_owned(), app.to_owned());
                    }
                    ui.close();
                }
                true
            });
        }
        ui.window.add_controller(drop);
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

    /// Who to tell when an app is dragged out of the folder.
    pub fn set_on_remove(&self, f: OnRemove) {
        *self.on_remove.borrow_mut() = Some(f);
    }

    fn shade(&self, on: bool) {
        let f = self.on_shade.borrow().clone();
        if let Some(f) = f {
            f(on);
        }
    }

    /// Show folder `id` named `name` with `tiles` (its apps), three a
    /// row, zooming out of `source` (its icon in the grid).
    pub fn open(&self, id: &str, name: &str, tiles: Vec<gtk::Widget>, source: &gtk::Widget) {
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
        let from = anim::screen_bounds(source);
        self.set_source(Some(source));
        self.window.present();
        self.shade(true);
        // GNOME focuses the rename button (focus ring and all).
        self.edit.grab_focus();
        self.zoom_in(from);
    }

    fn set_source(&self, source: Option<&gtk::Widget>) {
        let old = self.source.replace(source.map(|s| s.downgrade()));
        if let Some(old) = old.and_then(|w| w.upgrade()) {
            if Some(&old) != source {
                self.source_slot.stop();
                old.set_opacity(1.0);
            }
        }
    }

    /// Where the dialog rests on the output.
    fn resting_box() -> (f64, f64, f64, f64) {
        let (mx, my, mw, _) = anim::monitor_bounds();
        (
            mx + (mw - f64::from(DIALOG_W)) / 2.0,
            my + f64::from(PANEL_H + TOP),
            f64::from(DIALOG_W),
            f64::from(DIALOG_H),
        )
    }

    /// Fade the folder's icon in the grid out (opening) or back.
    fn fade_source(&self, visible: bool) {
        let Some(source) = self.source.borrow().as_ref().and_then(|w| w.upgrade()) else {
            return;
        };
        let timing = if visible {
            anim::FOLDER_ICON_SHOW
        } else {
            anim::FOLDER_ICON_HIDE
        };
        let policy = anim::motion();
        let from = source.opacity();
        let to = if visible { 1.0 } else { 0.0 };
        let total = if source.is_mapped() {
            timing.end_ms(policy)
        } else {
            0.0
        };
        let weak = source.downgrade();
        self.source_slot.start(
            &source,
            "folder-icon",
            total,
            move |elapsed| {
                if let Some(source) = weak.upgrade() {
                    source.set_opacity(anim::lerp(from, to, timing.at(elapsed, policy)));
                }
            },
            || {},
        );
    }

    /// `_zoomAndFadeIn`: from the folder's icon, transparent over a
    /// clear shade, to the dialog in place over the shade.
    fn zoom_in(&self, from: Option<(f64, f64, f64, f64)>) {
        let policy = anim::motion();
        let dialog = Self::resting_box();
        let source = from.unwrap_or(dialog);
        let total = [anim::FOLDER_ZOOM, anim::FOLDER_FADE_IN, anim::FOLDER_SHADE]
            .iter()
            .map(|t| t.end_ms(policy))
            .fold(0.0, f64::max);
        let (zoom, shade) = (self.zoom.downgrade(), self.shade_box.downgrade());
        self.slot.start(
            &self.zoom,
            "folder-zoom-in",
            total,
            move |elapsed| {
                let (Some(zoom), Some(shade)) = (zoom.upgrade(), shade.upgrade()) else {
                    return;
                };
                let (shift, scale) =
                    anim::folder_zoom(source, dialog, anim::FOLDER_ZOOM.at(elapsed, policy));
                zoom.set(shift, scale, anim::FOLDER_FADE_IN.at(elapsed, policy));
                shade.set_opacity(anim::FOLDER_SHADE.at(elapsed, policy));
            },
            || {},
        );
        self.fade_source(false);
    }

    /// Close the dialog: back into its folder's icon when that still
    /// shows (`_zoomAndFadeOut`), at once when it does not.
    pub fn close(&self) {
        let source = self.source.borrow().as_ref().and_then(|w| w.upgrade());
        let target = source
            .as_ref()
            .filter(|s| s.is_mapped())
            .and_then(anim::screen_bounds);
        match target {
            Some(target) if self.window.is_visible() => self.zoom_out(target),
            _ => self.close_now(),
        }
    }

    /// Close the dialog without animating (the overview went, or an app
    /// launched from it).
    pub fn close_now(&self) {
        self.slot.stop();
        self.end_rename();
        if self.window.is_visible() {
            self.shade(false);
        }
        self.window.set_visible(false);
        self.zoom.set((0.0, 0.0), (1.0, 1.0), 1.0);
        self.shade_box.set_opacity(1.0);
        self.source_slot.stop();
        self.set_source(None);
    }

    fn end_rename(&self) {
        if self.edit.is_active() {
            self.edit.set_active(false);
        }
    }

    fn zoom_out(&self, target: (f64, f64, f64, f64)) {
        self.end_rename();
        self.shade(false);
        let policy = anim::motion();
        let dialog = Self::resting_box();
        let (from_shift, from_scale, from_alpha) =
            (self.zoom.shift(), self.zoom.scale(), self.zoom.alpha());
        let from_shade = self.shade_box.opacity();
        let (to_shift, to_scale) = anim::folder_zoom(target, dialog, 0.0);
        let total = [anim::FOLDER_ZOOM, anim::FOLDER_FADE_OUT, anim::FOLDER_SHADE]
            .iter()
            .map(|t| t.end_ms(policy))
            .fold(0.0, f64::max);
        let (zoom, shade) = (self.zoom.downgrade(), self.shade_box.downgrade());
        let step = move |elapsed: f64| {
            let (Some(zoom), Some(shade)) = (zoom.upgrade(), shade.upgrade()) else {
                return;
            };
            let t = anim::FOLDER_ZOOM.at(elapsed, policy);
            let fade = anim::FOLDER_FADE_OUT.at(elapsed, policy);
            zoom.set(
                (
                    anim::lerp(from_shift.0, to_shift.0, t),
                    anim::lerp(from_shift.1, to_shift.1, t),
                ),
                (
                    anim::lerp(from_scale.0, to_scale.0, t),
                    anim::lerp(from_scale.1, to_scale.1, t),
                ),
                anim::lerp(from_alpha, 0.0, fade),
            );
            shade.set_opacity(anim::lerp(
                from_shade,
                0.0,
                anim::FOLDER_SHADE.at(elapsed, policy),
            ));
        };
        let (window, zoom, shade) = (
            self.window.downgrade(),
            self.zoom.downgrade(),
            self.shade_box.downgrade(),
        );
        self.slot
            .start(&self.zoom, "folder-zoom-out", total, step, move || {
                if let Some(window) = window.upgrade() {
                    window.set_visible(false);
                }
                if let (Some(zoom), Some(shade)) = (zoom.upgrade(), shade.upgrade()) {
                    zoom.set((0.0, 0.0), (1.0, 1.0), 1.0);
                    shade.set_opacity(1.0);
                }
            });
        self.fade_source(true);
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
