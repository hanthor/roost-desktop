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
use tuna_shell_control::SwitcherThumbnail;
use tuna_shell_host::model::ShellModel;

/// Layer namespace (matches the legacy shell's switcher surface).
pub use tuna_shell_control::SWITCHER_NAMESPACE as NAMESPACE;

type SwitcherItem = (u64, String, Option<String>, usize, Option<String>);

/// What is on screen, to rebuild only on change.
#[derive(PartialEq, Default, Clone)]
struct Shown {
    /// (representative window, title, app id, the app's window count).
    items: Vec<SwitcherItem>,
    selected: Option<u64>,
    all_windows: bool,
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
/// `.switcher-list` padding on each side.
const LIST_PAD: i32 = 12;
/// Between a frame and its title (`.thumbnail-box` spacing).
const BOX_SPACING: i32 = 6;
/// The popup's fade-out, the thumbnails' fade in and out, and the
/// strip's scroll to the focused window (`POPUP_FADE_OUT_TIME`,
/// `THUMBNAIL_FADE_TIME`, `POPUP_SCROLL_TIME`), all ease-out-quad.
const FADE_MS: f64 = 100.0;

/// Where the thumbnail strip sits and how tall its frames are, in the
/// monitor's logical pixels (altTab.js `vfunc_allocate` and `addClones`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StripLayout {
    /// The list's visible box, without the margins kept for its shadow.
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    /// Each frame's height: 256 with room to spare, less to fit.
    frame_height: i32,
    /// Shadow room kept under the list: none when it reaches the bottom.
    shadow_bottom: i32,
}

/// The strip on a `monitor` (width, height) under an app list whose
/// visible bottom edge is `list_bottom`, centred on `center` as far as
/// the monitor allows. `content_width` is the list's natural width and
/// `overhead` its height less one frame (paddings, spacing, title).
/// GNOME shrinks the frames until the list fits the space under the app
/// row and keeps the list's width to the monitor, scrolling the rest.
fn strip_layout(
    monitor: (i32, i32),
    list_bottom: i32,
    center: i32,
    content_width: i32,
    overhead: i32,
) -> StripLayout {
    let (mon_w, mon_h) = monitor;
    let width = content_width.clamp(0, (mon_w - 2 * LIST_MARGIN).max(0));
    let x = (center - width / 2).clamp(LIST_MARGIN, (mon_w - LIST_MARGIN - width).max(LIST_MARGIN));
    let y = list_bottom + POPUP_SPACING;
    // addClones: GNOME's sums leave the list two pixels short of the
    // monitor's bottom edge.
    let frame_height = (mon_h - y - overhead - 2).clamp(1, THUMBNAIL_SIZE);
    let height = overhead + frame_height;
    StripLayout {
        x,
        y,
        width,
        height,
        frame_height,
        shadow_bottom: (mon_h - y - height).clamp(0, LIST_MARGIN),
    }
}

/// Ease-out-quad progress `elapsed_ms` into an animation lasting
/// `duration_ms`; done at once when that is zero (motion policy off).
fn ease_progress(elapsed_ms: f64, duration_ms: f64) -> f64 {
    let p = if duration_ms > 0.0 {
        (elapsed_ms / duration_ms).clamp(0.0, 1.0)
    } else {
        1.0
    };
    1.0 - (1.0 - p).powi(2)
}

/// A fade's length under the motion policy: GNOME's 100 ms times the
/// slow-down factor, kept under Reduced Motion, zero with animations off.
fn fade_ms(policy: tuna_shell_control::MotionPolicy) -> f64 {
    if policy.allows_fades() {
        policy.adjust_ms(FADE_MS)
    } else {
        0.0
    }
}

/// The strip's scroll is motion: Reduced Motion snaps it.
fn scroll_ms(policy: tuna_shell_control::MotionPolicy) -> f64 {
    if policy.allows_motion() {
        policy.adjust_ms(FADE_MS)
    } else {
        0.0
    }
}

/// The first monitor's logical size (1280×800 before one is known).
fn monitor_size() -> (i32, i32) {
    gtk::gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_downcast::<gtk::gdk::Monitor>()
        .map_or((1280, 800), |m| {
            (m.geometry().width(), m.geometry().height())
        })
}

fn clear(row: &gtk::Box) {
    while let Some(child) = row.first_child() {
        row.remove(&child);
    }
}

/// Ease `widget`'s opacity to `to` (instantly when the motion policy
/// drops fades),
/// then run `done`. A newer fade or [`stop`] on the same `generation`
/// cancels it. `name` labels the log lines the shell proof reads.
fn fade(
    widget: &impl IsA<gtk::Widget>,
    to: f64,
    generation: &Rc<Cell<u64>>,
    name: &'static str,
    done: impl FnOnce() + 'static,
) {
    let current = generation.get().wrapping_add(1);
    generation.set(current);
    let widget = widget.as_ref().clone();
    let from = widget.opacity();
    let duration = fade_ms(crate::motion::current());
    let enabled = duration > 0.0;
    if from != to {
        eprintln!("tuna-shell-gtk: switcher fade {name} start animated={enabled}");
    }
    if !enabled || from == to {
        widget.set_opacity(to);
        if from != to {
            eprintln!("tuna-shell-gtk: switcher fade {name} settled");
        }
        done();
        return;
    }
    let generation = generation.clone();
    let started = Instant::now();
    let mut done = Some(done);
    gtk::glib::timeout_add_local(Duration::from_millis(16), move || {
        if generation.get() != current {
            return gtk::glib::ControlFlow::Break;
        }
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        let t = ease_progress(elapsed, duration.min(fade_ms(crate::motion::current())));
        widget.set_opacity(from + (to - from) * t);
        if t < 1.0 {
            return gtk::glib::ControlFlow::Continue;
        }
        eprintln!("tuna-shell-gtk: switcher fade {name} settled");
        if let Some(done) = done.take() {
            done();
        }
        gtk::glib::ControlFlow::Break
    });
}

/// Cancel a running fade and show `widget` fully opaque.
fn stop(widget: &impl IsA<gtk::Widget>, generation: &Cell<u64>) {
    generation.set(generation.get().wrapping_add(1));
    widget.as_ref().set_opacity(1.0);
}

pub struct SwitcherUi {
    window: gtk::ApplicationWindow,
    row: gtk::Box,
    apps: Rc<LiveApps>,
    shown: RefCell<Shown>,
    /// The thumbnails' own surface, placed under the selected icon.
    thumbs_window: gtk::ApplicationWindow,
    thumbs_list: gtk::Box,
    /// Holds the frames; scrolls when they are wider than the monitor.
    thumbs_scroller: gtk::ScrolledWindow,
    thumbs_row: gtk::Box,
    thumbs: RefCell<Thumbs>,
    /// The frames' current height, refitted while shown.
    frame_height: Cell<i32>,
    /// Where the strip is scrolling to (`POPUP_SCROLL_TIME`).
    scroll_target: Cell<Option<f64>>,
    scroll_generation: Rc<Cell<u64>>,
    /// The popup's and the thumbnails' running fades.
    popup_fade: Rc<Cell<u64>>,
    thumbs_fade: Rc<Cell<u64>>,
    /// The frames last laid out, for the compositor to draw into.
    frames: RefCell<Vec<(u64, gtk::Box)>>,
    window_frames: RefCell<Vec<(u64, gtk::Box)>>,
    /// When the selected app was selected (for the popup delay).
    selected_since: Cell<Option<(u64, Instant)>>,
    /// The tiles by representative window, to place the thumbnails.
    tiles: RefCell<Vec<(u64, gtk::Box)>>,
}

impl SwitcherUi {
    pub fn new(app: &gtk::Application, apps: Rc<LiveApps>) -> Rc<Self> {
        let window = gtk::ApplicationWindow::new(app);
        window.add_css_class("tuna-switcher");
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
        thumbs_window.add_css_class("tuna-switcher");
        thumbs_window.init_layer_shell();
        thumbs_window.set_layer(Layer::Overlay);
        thumbs_window.set_namespace(Some(tuna_shell_control::SWITCHER_THUMBNAILS_NAMESPACE));
        thumbs_window.set_keyboard_mode(KeyboardMode::None);
        thumbs_window.set_exclusive_zone(-1);
        thumbs_window.set_anchor(Edge::Top, true);
        thumbs_window.set_anchor(Edge::Left, true);
        thumbs_window.set_title(Some("Switch Windows"));
        let thumbs_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        thumbs_list.add_css_class("switcher-list");
        thumbs_list.add_css_class("thumbnail-switcher");
        let thumbs_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        // GNOME's SwitcherList scrolls, without scrollbars, when its
        // items are wider than the monitor.
        let thumbs_scroller = gtk::ScrolledWindow::new();
        thumbs_scroller.set_policy(gtk::PolicyType::External, gtk::PolicyType::Never);
        thumbs_scroller.set_propagate_natural_width(true);
        thumbs_scroller.set_propagate_natural_height(true);
        thumbs_scroller.set_child(Some(&thumbs_row));
        thumbs_list.append(&thumbs_scroller);
        thumbs_window.set_child(Some(&thumbs_list));
        Rc::new(Self {
            window,
            row,
            apps,
            shown: Default::default(),
            thumbs_window,
            thumbs_list,
            thumbs_scroller,
            thumbs_row,
            thumbs: Default::default(),
            frame_height: Cell::new(THUMBNAIL_SIZE),
            scroll_target: Cell::new(None),
            scroll_generation: Default::default(),
            popup_fade: Default::default(),
            thumbs_fade: Default::default(),
            frames: Default::default(),
            window_frames: Default::default(),
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
                all_windows: model.switcher_all_windows(),
            }
        } else {
            Shown::default()
        };
        if *self.shown.borrow() != now {
            if now.items.is_empty() {
                // switcherPopup.js fadeAndDestroy: 100 ms to transparent.
                if self.window.is_visible() {
                    let window = self.window.clone();
                    fade(
                        &self.window,
                        0.0,
                        &self.popup_fade,
                        "popup-out",
                        move || {
                            window.set_visible(false);
                            window.set_opacity(1.0);
                        },
                    );
                }
            } else {
                stop(&self.window, &self.popup_fade);
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
            .filter(|_| !now.all_windows)
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
        let current = self.thumbs.borrow().clone();
        if current == wanted {
            if !wanted.windows.is_empty() {
                self.place_thumbnails();
            }
        } else if !wanted.windows.is_empty() && current.windows == wanted.windows {
            // Only the focus moved: keep the strip, move the highlight.
            self.highlight_thumbnail(wanted.focused);
            *self.thumbs.borrow_mut() = wanted;
            self.place_thumbnails();
        } else {
            self.rebuild_thumbnails(&wanted);
            *self.thumbs.borrow_mut() = wanted;
        }
    }

    fn rebuild_thumbnails(&self, thumbs: &Thumbs) {
        self.frames.borrow_mut().clear();
        self.scroll_target.set(None);
        self.scroll_generation
            .set(self.scroll_generation.get().wrapping_add(1));
        if thumbs.windows.is_empty() {
            // altTab.js _destroyThumbnails: fade out, then go. The
            // compositor stops drawing the windows at once.
            let (window, row) = (self.thumbs_window.clone(), self.thumbs_row.clone());
            if window.is_visible() {
                fade(
                    &self.thumbs_window,
                    0.0,
                    &self.thumbs_fade,
                    "thumbnails-out",
                    move || {
                        window.set_visible(false);
                        clear(&row);
                    },
                );
            } else {
                clear(&row);
            }
            return;
        }
        clear(&self.thumbs_row);
        let frame_height = self.frame_height.get();
        for (id, title) in &thumbs.windows {
            let item = gtk::Box::new(gtk::Orientation::Vertical, 0);
            item.add_css_class("item-box");
            let thumbnail_box = gtk::Box::new(gtk::Orientation::Vertical, BOX_SPACING);
            thumbnail_box.add_css_class("thumbnail-box");
            let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
            frame.add_css_class("thumbnail");
            frame.set_size_request(THUMBNAIL_SIZE, frame_height);
            thumbnail_box.append(&frame);
            let label = gtk::Label::new(Some(title));
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(1);
            label.set_hexpand(true);
            thumbnail_box.append(&label);
            item.append(&thumbnail_box);
            item.set_accessible_role(gtk::AccessibleRole::ListItem);
            item.update_property(&[gtk::accessible::Property::Label(title)]);
            self.thumbs_row.append(&item);
            self.frames.borrow_mut().push((*id, frame));
        }
        self.highlight_thumbnail(thumbs.focused);
        self.thumbs_scroller.hadjustment().set_value(0.0);
        self.place_thumbnails();
        // altTab.js _createThumbnails: fade in from transparent (or back
        // from a fade-out still running).
        if !self.thumbs_window.is_visible() {
            self.thumbs_window.set_opacity(0.0);
        }
        self.thumbs_window.present();
        fade(
            &self.thumbs_window,
            1.0,
            &self.thumbs_fade,
            "thumbnails-in",
            || {},
        );
    }

    /// Mark the focused thumbnail selected, for sight and screen readers.
    fn highlight_thumbnail(&self, focused: Option<usize>) {
        let mut child = self.thumbs_row.first_child();
        let mut i = 0;
        while let Some(item) = child {
            let selected = focused == Some(i);
            if selected {
                item.add_css_class("selected");
            } else {
                item.remove_css_class("selected");
            }
            item.update_state(&[gtk::accessible::State::Selected(Some(selected))]);
            child = item.next_sibling();
            i += 1;
        }
    }

    /// Centred under the selected icon, 24px under the app list, its
    /// frames shrunk to fit the monitor below it and its width kept to
    /// the monitor (altTab.js `vfunc_allocate`, `addClones`). Runs every
    /// tick while shown: the icons may not be laid out when the
    /// thumbnails open.
    fn place_thumbnails(&self) {
        let (mon_w, mon_h) = monitor_size();
        // The app list's size from its content: the window may not be
        // allocated yet on the tick that opens both.
        let (list_w, list_h) = self.window.child().map_or((0, 0), |c| {
            (
                c.measure(gtk::Orientation::Horizontal, -1).1,
                c.measure(gtk::Orientation::Vertical, -1).1,
            )
        });
        let list_x = (mon_w - list_w) / 2;
        let list_bottom = (mon_h - list_h) / 2 + list_h - LIST_MARGIN;
        let row_w = self.thumbs_row.measure(gtk::Orientation::Horizontal, -1).1;
        let row_h = self.thumbs_row.measure(gtk::Orientation::Vertical, -1).1;
        let overhead = row_h - self.frame_height.get() + 2 * LIST_PAD;
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
        let layout = strip_layout(
            (mon_w, mon_h),
            list_bottom,
            center,
            row_w + 2 * LIST_PAD,
            overhead,
        );
        let mut resized = false;
        if layout.frame_height != self.frame_height.get() {
            self.frame_height.set(layout.frame_height);
            for (_, frame) in self.frames.borrow().iter() {
                frame.set_size_request(THUMBNAIL_SIZE, layout.frame_height);
            }
            resized = true;
        }
        let content = layout.width - 2 * LIST_PAD;
        if self.thumbs_scroller.max_content_width() != content {
            // Both bounds: the surface is sized to its minimum, and a
            // scroller's own minimum is nothing. Keep min <= max on the way.
            self.thumbs_scroller.set_min_content_width(-1);
            self.thumbs_scroller.set_max_content_width(content);
            self.thumbs_scroller.set_min_content_width(content);
            resized = true;
        }
        if self.thumbs_list.margin_bottom() != layout.shadow_bottom {
            self.thumbs_list.set_margin_bottom(layout.shadow_bottom);
            resized = true;
        }
        if resized {
            // A layer surface keeps its last size unless asked again.
            self.thumbs_window.set_default_size(1, 1);
        }
        if self.thumbs_window.margin(Edge::Left) != layout.x - LIST_MARGIN {
            self.thumbs_window
                .set_margin(Edge::Left, layout.x - LIST_MARGIN);
        }
        if self.thumbs_window.margin(Edge::Top) != layout.y - LIST_MARGIN {
            self.thumbs_window
                .set_margin(Edge::Top, layout.y - LIST_MARGIN);
        }
        self.scroll_to_focused();
    }

    /// Keep the focused thumbnail in view, scrolling 100 ms ease-out-quad
    /// as GNOME's SwitcherList does (`_scrollToLeft`, `_scrollToRight`).
    fn scroll_to_focused(&self) {
        let Some(focused) = self.thumbs.borrow().focused else {
            return;
        };
        let Some(item) = self
            .frames
            .borrow()
            .get(focused)
            // frame -> .thumbnail-box -> .item-box
            .and_then(|(_, frame)| frame.parent()?.parent())
        else {
            return;
        };
        let adjustment = self.thumbs_scroller.hadjustment();
        let page = adjustment.page_size();
        if page <= 0.0 || item.width() == 0 {
            return;
        }
        let Some(at) = item.compute_point(&self.thumbs_row, &gtk::graphene::Point::new(0.0, 0.0))
        else {
            return;
        };
        let (x1, x2) = (
            f64::from(at.x()),
            f64::from(at.x()) + f64::from(item.width()),
        );
        let value = self.scroll_target.get().unwrap_or(adjustment.value());
        let target = if x1 < value {
            x1.max(0.0)
        } else if x2 > value + page {
            (x2 - page).min(adjustment.upper())
        } else {
            return;
        };
        self.scroll_target.set(Some(target));
        let current = self.scroll_generation.get().wrapping_add(1);
        self.scroll_generation.set(current);
        let from = adjustment.value();
        let duration = scroll_ms(crate::motion::current());
        if duration <= 0.0 {
            adjustment.set_value(target);
            return;
        }
        let generation = self.scroll_generation.clone();
        let started = Instant::now();
        gtk::glib::timeout_add_local(Duration::from_millis(16), move || {
            if generation.get() != current {
                return gtk::glib::ControlFlow::Break;
            }
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            let t = ease_progress(elapsed, duration.min(scroll_ms(crate::motion::current())));
            adjustment.set_value(from + (target - from) * t);
            if t < 1.0 {
                gtk::glib::ControlFlow::Continue
            } else {
                gtk::glib::ControlFlow::Break
            }
        });
    }

    /// Where the thumbnails' frames sit on screen, once laid out (none
    /// while hidden): what the compositor draws the windows into.
    pub fn thumbnail_frames(&self) -> Vec<SwitcherThumbnail> {
        if self.window.is_visible() && self.shown.borrow().all_windows {
            let geometry = gtk::gdk::Display::default()
                .and_then(|d| d.monitors().item(0))
                .and_downcast::<gtk::gdk::Monitor>()
                .map(|m| m.geometry());
            let (width, height) = geometry.map_or((1280, 800), |g| (g.width(), g.height()));
            let x0 = (width - self.window.width()) / 2;
            let y0 = (height - self.window.height()) / 2;
            return self
                .window_frames
                .borrow()
                .iter()
                .filter_map(|(window, frame)| {
                    if frame.width() == 0 || frame.height() == 0 {
                        return None;
                    }
                    let p =
                        frame.compute_point(&self.window, &gtk::graphene::Point::new(0.0, 0.0))?;
                    Some(SwitcherThumbnail {
                        window: *window,
                        x: x0 + p.x() as i32,
                        y: y0 + p.y() as i32,
                        width: frame.width(),
                        height: frame.height(),
                    })
                })
                .collect();
        }
        // Nothing once the switcher closes, while its surfaces fade.
        if !self.thumbs_window.is_visible() || self.shown.borrow().items.is_empty() {
            return Vec::new();
        }
        let x0 = self.thumbs_window.margin(Edge::Left);
        let y0 = self.thumbs_window.margin(Edge::Top);
        let view = self.thumbs_scroller.width();
        self.frames
            .borrow()
            .iter()
            .filter_map(|(window, frame)| {
                if frame.width() == 0 {
                    return None;
                }
                // Frames scrolled out of the strip draw nothing.
                let v = frame
                    .compute_point(&self.thumbs_scroller, &gtk::graphene::Point::new(0.0, 0.0))?;
                if v.x() < 0.0 || v.x() as i32 + frame.width() > view {
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
        self.window_frames.borrow_mut().clear();
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
            icon.set_pixel_size(if now.all_windows { 32 } else { icon_size });
            // The app's name (window-backed apps: their app id), as
            // GNOME labels the tile.
            let name = if now.all_windows {
                title.clone()
            } else {
                entry
                    .map(|e| e.name.clone())
                    .or_else(|| app_id.clone())
                    .unwrap_or_else(|| title.clone())
            };
            let label = gtk::Label::new(Some(&name));
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(1);
            label.set_hexpand(true);
            let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
            tile.add_css_class("item-box");
            if now.all_windows {
                let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
                frame.add_css_class("thumbnail");
                frame.set_size_request(160, 100);
                tile.append(&frame);
                self.window_frames.borrow_mut().push((*id, frame));
            } else {
                tile.set_size_request(icon_size + 31, icon_size + 31);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `.item-box` and `.thumbnail-box` padding on each side.
    const ITEM_PAD: i32 = 6;
    const BOX_PAD: i32 = 2;
    /// Paddings, spacing and a 20px title around one frame.
    const OVERHEAD: i32 = 2 * LIST_PAD + 2 * ITEM_PAD + 2 * BOX_PAD + BOX_SPACING + 20;

    fn inside(layout: &StripLayout, (w, h): (i32, i32)) -> bool {
        layout.x >= 0
            && layout.x + layout.width <= w
            && layout.y >= 0
            && layout.y + layout.height + layout.shadow_bottom <= h
    }

    #[test]
    fn strip_shrinks_to_fit_under_the_app_list_at_1280x800() {
        // The bug's frame: the app list ends at y=475 on 1280x800.
        let layout = strip_layout((1280, 800), 475, 500, 2 * 284 + 12 + 24, OVERHEAD);
        assert_eq!(layout.y, 499);
        // GNOME's addClones leaves the list 2px short of the bottom.
        assert_eq!(layout.y + layout.height, 798);
        assert_eq!(layout.frame_height, 800 - 499 - OVERHEAD - 2);
        assert!(layout.frame_height < THUMBNAIL_SIZE);
        assert_eq!(layout.shadow_bottom, 2);
        assert!(inside(&layout, (1280, 800)));
    }

    #[test]
    fn strip_keeps_full_size_frames_with_room() {
        let layout = strip_layout((1920, 1200), 675, 960, 604, OVERHEAD);
        assert_eq!(layout.frame_height, THUMBNAIL_SIZE);
        assert_eq!(layout.shadow_bottom, LIST_MARGIN);
        assert_eq!(layout.x, 960 - 302);
        assert!(inside(&layout, (1920, 1200)));
    }

    #[test]
    fn strip_fits_at_125_percent_scale() {
        // 1280x800 at 125% is 1024x640 logical; a 24px title.
        let monitor = (1024, 640);
        let layout = strip_layout(monitor, 395, 400, 600, OVERHEAD + 4);
        assert!(layout.frame_height < THUMBNAIL_SIZE);
        assert!(inside(&layout, monitor));
    }

    #[test]
    fn many_windows_keep_the_strip_on_the_monitor() {
        // Twelve 284px items are far wider than the monitor: the list
        // takes the monitor's width less its shadow and scrolls.
        let content = 12 * 284 + 11 * 12 + 24;
        let layout = strip_layout((1280, 800), 475, 1200, content, OVERHEAD);
        assert_eq!(layout.width, 1280 - 2 * LIST_MARGIN);
        assert_eq!(layout.x, LIST_MARGIN);
        assert!(inside(&layout, (1280, 800)));
    }

    #[test]
    fn strip_centres_under_the_icon_but_clamps_to_the_edges() {
        let left = strip_layout((1280, 800), 475, 10, 600, OVERHEAD);
        assert_eq!(left.x, LIST_MARGIN);
        let right = strip_layout((1280, 800), 475, 1270, 600, OVERHEAD);
        assert_eq!(right.x + right.width, 1280 - LIST_MARGIN);
        let centred = strip_layout((1280, 800), 475, 640, 600, OVERHEAD);
        assert_eq!(centred.x, 340);
    }

    #[test]
    fn fades_are_100_ms_ease_out_quad_or_instant() {
        assert_eq!(ease_progress(0.0, 100.0), 0.0);
        assert_eq!(ease_progress(50.0, 100.0), 0.75);
        assert_eq!(ease_progress(100.0, 100.0), 1.0);
        assert_eq!(ease_progress(250.0, 100.0), 1.0);
        assert_eq!(ease_progress(0.0, 0.0), 1.0);
    }

    #[test]
    fn fades_follow_the_motion_policy() {
        use tuna_shell_control::{MotionLevel, MotionPolicy};
        let full = MotionPolicy::new(MotionLevel::Full, 1.0);
        assert_eq!(fade_ms(full), 100.0);
        assert_eq!(scroll_ms(full), 100.0);
        // Reduced Motion keeps the fades and snaps the scroll.
        let reduced = MotionPolicy::new(MotionLevel::FadeOnly, 1.0);
        assert_eq!(fade_ms(reduced), 100.0);
        assert_eq!(scroll_ms(reduced), 0.0);
        let off = MotionPolicy::new(MotionLevel::Off, 1.0);
        assert_eq!(fade_ms(off), 0.0);
        assert_eq!(scroll_ms(off), 0.0);
        // GNOME_SHELL_SLOWDOWN_FACTOR stretches them.
        assert_eq!(fade_ms(MotionPolicy::new(MotionLevel::Full, 2.0)), 200.0);
    }
}
