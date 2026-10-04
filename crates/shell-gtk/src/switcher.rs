//! Alt-Tab switcher, GNOME 51's AppSwitcher (`switch-applications`): a
//! centered list of app tiles in most-recently-used order, one per app,
//! each a 96px icon (smaller when many) over the app's name, the
//! selection highlighted, an arrow under apps with several windows. The compositor owns the keys and the selection
//! (control model `switcher_*`); this module only draws it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::live_apps::LiveApps;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use roost_shell_control::SwitcherThumbnail;
use roost_shell_host::model::ShellModel;

/// Layer namespace (matches the legacy shell's switcher surface).
pub const NAMESPACE: &str = "roost-shell-switcher";

type SwitcherItem = (u64, String, Option<String>, usize, Option<String>);

/// What is on screen, to rebuild only on change.
#[derive(PartialEq, Default, Clone)]
struct Shown {
    /// (representative window, title, app id, the app's window count).
    items: Vec<SwitcherItem>,
    selected: Option<u64>,
}

/// GNOME's ThumbnailSwitcher under the selected app: its windows' frames
/// and titles, the focused one highlighted.
#[derive(PartialEq, Default, Clone)]
struct Thumbs {
    /// (window, title), most recent first.
    windows: Vec<(u64, String)>,
    focused: Option<usize>,
}

/// GNOME pops the thumbnails up this long after an app with several
/// windows is selected (`THUMBNAIL_POPUP_TIME`).
const THUMBNAIL_POPUP: Duration = Duration::from_millis(500);
/// A thumbnail frame's width and largest height (`THUMBNAIL_DEFAULT_SIZE`).
const THUMBNAIL_SIZE: i32 = 256;
/// Between the app list and the thumbnails (`.switcher-popup` spacing).
const POPUP_SPACING: i32 = 24;
/// The `.switcher-list` margin the windows keep for the shadow.
const LIST_MARGIN: i32 = 16;

pub struct SwitcherUi {
    window: gtk::ApplicationWindow,
    row: gtk::Box,
    apps: Rc<LiveApps>,
    shown: RefCell<Shown>,
    /// The thumbnails' own surface, placed under the selected icon.
    thumbs_window: gtk::ApplicationWindow,
    thumbs_row: gtk::Box,
    thumbs: RefCell<Thumbs>,
    /// The frames last laid out, for the compositor to draw into.
    frames: RefCell<Vec<(u64, gtk::Box)>>,
    /// When the selected app was selected (for the popup delay).
    selected_since: Cell<Option<(u64, Instant)>>,
    /// The tiles by representative window, to place the thumbnails.
    tiles: RefCell<Vec<(u64, gtk::Box)>>,
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
        // The thumbnails: a second list, anchored top-left and moved by
        // its margins to sit centred under the selected icon.
        let thumbs_window = gtk::ApplicationWindow::new(app);
        thumbs_window.add_css_class("roost-switcher");
        thumbs_window.init_layer_shell();
        thumbs_window.set_layer(Layer::Overlay);
        thumbs_window.set_namespace(Some("roost-shell-switcher-thumbnails"));
        thumbs_window.set_keyboard_mode(KeyboardMode::None);
        thumbs_window.set_exclusive_zone(-1);
        thumbs_window.set_anchor(Edge::Top, true);
        thumbs_window.set_anchor(Edge::Left, true);
        thumbs_window.set_title(Some("Switch Windows"));
        let thumbs_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        thumbs_list.add_css_class("switcher-list");
        thumbs_list.add_css_class("thumbnail-switcher");
        let thumbs_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        thumbs_list.append(&thumbs_row);
        thumbs_window.set_child(Some(&thumbs_list));
        Rc::new(Self {
            window,
            row,
            apps,
            shown: Default::default(),
            thumbs_window,
            thumbs_row,
            thumbs: Default::default(),
            frames: Default::default(),
            selected_since: Cell::new(None),
            tiles: Default::default(),
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
                            w.icon.clone(),
                        )
                    })
                    .collect(),
                selected: model.switcher_app(),
            }
        } else {
            Shown::default()
        };
        if *self.shown.borrow() != now {
            if now.items.is_empty() {
                self.window.set_visible(false);
            } else {
                self.rebuild(&now);
                self.window.present();
            }
            *self.shown.borrow_mut() = now.clone();
        }
        self.sync_thumbnails(model, &now);
    }

    /// GNOME's thumbnails: shown while they hold the focus, or once an
    /// app with several windows has been selected for half a second.
    fn sync_thumbnails(&self, model: &ShellModel, now: &Shown) {
        let app = now.selected;
        let since = match (self.selected_since.get(), app) {
            (Some((id, t)), Some(a)) if id == a => Some(t),
            (_, Some(a)) => {
                let t = Instant::now();
                self.selected_since.set(Some((a, t)));
                Some(t)
            }
            _ => {
                self.selected_since.set(None);
                None
            }
        };
        let windows: Vec<(u64, String)> = app
            .map(|a| {
                model
                    .app_windows(a)
                    .into_iter()
                    .filter_map(|id| {
                        let w = model.windows().iter().find(|w| w.id == id)?;
                        Some((id, w.title.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let focused = model.switcher_window();
        let popped = windows.len() > 1 && since.is_some_and(|t| t.elapsed() >= THUMBNAIL_POPUP);
        let wanted = if focused.is_some() || popped {
            Thumbs { windows, focused }
        } else {
            Thumbs::default()
        };
        if *self.thumbs.borrow() != wanted {
            self.rebuild_thumbnails(&wanted);
            *self.thumbs.borrow_mut() = wanted;
        } else if self.thumbs_window.is_visible() {
            self.place_thumbnails();
        }
    }

    fn rebuild_thumbnails(&self, thumbs: &Thumbs) {
        while let Some(child) = self.thumbs_row.first_child() {
            self.thumbs_row.remove(&child);
        }
        self.frames.borrow_mut().clear();
        if thumbs.windows.is_empty() {
            self.thumbs_window.set_visible(false);
            return;
        }
        let monitor = gtk::gdk::Display::default()
            .and_then(|d| d.monitors().item(0))
            .and_downcast::<gtk::gdk::Monitor>()
            .map(|m| m.geometry());
        let mon_h = monitor.map_or(800, |g| g.height());
        // Under the app list: its bottom is the centred switcher's.
        let list_h = self.window.height();
        let list_bottom = (mon_h + list_h) / 2 - LIST_MARGIN;
        let top = list_bottom + POPUP_SPACING;
        // altTab.js addClones: the height left under `top`, less the
        // label, the item's and the list's paddings (GNOME adds their
        // horizontal and vertical sums) and the spacing, at most 256;
        // the frame then gets the vertical paddings back, less spacing.
        const LABEL: i32 = 20;
        const ITEM_PAD: i32 = 6;
        const LIST_PAD: i32 = 12;
        const SPACING: i32 = 6;
        let padding = 4 * ITEM_PAD + 4 * LIST_PAD;
        let avail = (mon_h - top - LABEL - padding - SPACING).min(THUMBNAIL_SIZE);
        let bin_h = (avail + 2 * ITEM_PAD + 2 * LIST_PAD - SPACING).clamp(48, THUMBNAIL_SIZE);
        for (i, (id, title)) in thumbs.windows.iter().enumerate() {
            let item = gtk::Box::new(gtk::Orientation::Vertical, 0);
            item.add_css_class("item-box");
            if thumbs.focused == Some(i) {
                item.add_css_class("selected");
            }
            let thumbnail_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
            thumbnail_box.add_css_class("thumbnail-box");
            let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
            frame.add_css_class("thumbnail");
            frame.set_size_request(THUMBNAIL_SIZE, bin_h);
            thumbnail_box.append(&frame);
            let label = gtk::Label::new(Some(title));
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(1);
            label.set_hexpand(true);
            thumbnail_box.append(&label);
            item.append(&thumbnail_box);
            item.set_accessible_role(gtk::AccessibleRole::ListItem);
            item.update_property(&[gtk::accessible::Property::Label(title)]);
            item.update_state(&[gtk::accessible::State::Selected(Some(
                thumbs.focused == Some(i),
            ))]);
            self.thumbs_row.append(&item);
            self.frames.borrow_mut().push((*id, frame));
        }
        self.place_thumbnails();
        self.thumbs_window.present();
    }

    /// Centred under the selected icon, kept on the monitor, 24px under
    /// the app list (altTab.js `vfunc_allocate`). Runs every tick while
    /// shown: the icons may not be laid out when the thumbnails open.
    fn place_thumbnails(&self) {
        let monitor = gtk::gdk::Display::default()
            .and_then(|d| d.monitors().item(0))
            .and_downcast::<gtk::gdk::Monitor>()
            .map(|m| m.geometry());
        let (mon_w, mon_h) = monitor.map_or((1280, 800), |g| (g.width(), g.height()));
        let (list_w, list_h) = (self.window.width(), self.window.height());
        let list_x = (mon_w - list_w) / 2;
        let top = (mon_h + list_h) / 2 - LIST_MARGIN + POPUP_SPACING;
        let (natural, _) = {
            let (_, nat, _, _) = self
                .thumbs_window
                .child()
                .map(|c| c.measure(gtk::Orientation::Horizontal, -1))
                .unwrap_or((0, 0, -1, -1));
            (nat, 0)
        };
        let center = self
            .shown
            .borrow()
            .selected
            .and_then(|sel| {
                self.tiles
                    .borrow()
                    .iter()
                    .find(|(id, _)| *id == sel)
                    .and_then(|(_, tile)| {
                        tile.compute_point(&self.window, &gtk::graphene::Point::new(0.0, 0.0))
                            .map(|p| list_x + p.x() as i32 + tile.width() / 2)
                    })
            })
            .unwrap_or(mon_w / 2);
        let x = (center - natural / 2).clamp(0, (mon_w - natural).max(0));
        if self.thumbs_window.margin(Edge::Left) != x {
            self.thumbs_window.set_margin(Edge::Left, x);
        }
        if self.thumbs_window.margin(Edge::Top) != top - LIST_MARGIN {
            self.thumbs_window.set_margin(Edge::Top, top - LIST_MARGIN);
        }
    }

    /// Where the thumbnails' frames sit on screen, once laid out (none
    /// while hidden): what the compositor draws the windows into.
    pub fn thumbnail_frames(&self) -> Vec<SwitcherThumbnail> {
        if !self.thumbs_window.is_visible() {
            return Vec::new();
        }
        let x0 = self.thumbs_window.margin(Edge::Left);
        let y0 = self.thumbs_window.margin(Edge::Top);
        self.frames
            .borrow()
            .iter()
            .filter_map(|(window, frame)| {
                if frame.width() == 0 {
                    return None;
                }
                let p = frame
                    .compute_point(&self.thumbs_window, &gtk::graphene::Point::new(0.0, 0.0))?;
                Some(SwitcherThumbnail {
                    window: *window,
                    x: x0 + p.x() as i32,
                    y: y0 + p.y() as i32,
                    width: frame.width(),
                    height: frame.height(),
                })
            })
            .collect()
    }

    fn rebuild(&self, now: &Shown) {
        while let Some(child) = self.row.first_child() {
            self.row.remove(&child);
        }
        self.tiles.borrow_mut().clear();
        let apps = self.apps.get();
        let width = gtk::gdk::Display::default()
            .and_then(|d| d.monitors().item(0))
            .and_downcast::<gtk::gdk::Monitor>()
            .map(|m| m.geometry().width())
            .unwrap_or(1280);
        let icon_size = crate::logic::switcher_icon_size(now.items.len(), width);
        for (id, title, app_id, windows, window_icon) in &now.items {
            let entry = app_id.as_deref().and_then(|a| {
                apps.entry(a.trim_end_matches(".desktop"))
                    .or_else(|| apps.entry(a))
            });
            let icon = match entry.and_then(|e| e.icon.clone()).or_else(|| {
                entry
                    .is_none()
                    .then(|| {
                        window_icon
                            .clone()
                            .filter(|icon| crate::usable_window_icon(icon))
                    })
                    .flatten()
            }) {
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
            self.tiles.borrow_mut().push((*id, tile.clone()));
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
