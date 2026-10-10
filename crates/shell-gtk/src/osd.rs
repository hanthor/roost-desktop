//! GNOME 51's on-screen display (`osdWindow.js`): the pill near the bottom
//! of the screen that shows a volume or brightness level, or a label such
//! as a keyboard layout. Shown through `org.gnome.Shell.ShowOSD`
//! (gnome-settings-daemon's media keys) and hidden 1.5 s after the last
//! call, as GNOME does.

use std::cell::{Cell, RefCell};
use std::f64::consts::TAU;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::transient;

/// GNOME's HIDE_TIMEOUT.
const HIDE_AFTER: Duration = Duration::from_millis(1500);
/// `.osd-window { margin-bottom: 4em }` at the 14.666px shell font.
const BOTTOM_MARGIN: i32 = 59;
/// `.osd-window .level`: 160px wide, a 6px bar.
const LEVEL_WIDTH: i32 = 160;
const LEVEL_HEIGHT: f64 = 6.0;
/// `-barlevel-overdrive-separator-width`.
const SEPARATOR_WIDTH: f64 = 3.0;

/// One ShowOSD request, as GNOME's shellDBus.js unpacks it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OsdRequest {
    pub icon: Option<String>,
    pub label: Option<String>,
    pub level: Option<f64>,
    pub max_level: Option<f64>,
}

pub struct OsdUi {
    window: gtk::Window,
    icon: gtk::Image,
    label: gtk::Label,
    level: gtk::DrawingArea,
    value: Rc<Cell<(f64, f64)>>,
    hide: RefCell<Option<glib::SourceId>>,
    /// The window's fade in and out.
    fade: transient::Player,
    /// The level bar easing to a new value.
    level_anim: transient::Player,
}

impl OsdUi {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("tuna-osd");
        window.set_title(Some("On-Screen Display"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::OSD_NAMESPACE));
        window.set_anchor(Edge::Bottom, true);
        window.set_margin(Edge::Bottom, BOTTOM_MARGIN);
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::None);
        // Click-through, like GNOME's OSD (not reactive).
        window.connect_realize(|w| {
            if let Some(surface) = w.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
            }
        });

        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        pill.add_css_class("osd-window");
        let icon = gtk::Image::new();
        icon.set_pixel_size(32);
        icon.set_valign(gtk::Align::Center);
        pill.append(&icon);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        column.set_valign(gtk::Align::Center);
        let label = gtk::Label::new(None);
        label.add_css_class("osd-label");
        column.append(&label);
        let level = gtk::DrawingArea::new();
        level.add_css_class("level");
        level.set_content_width(LEVEL_WIDTH);
        level.set_content_height(LEVEL_HEIGHT as i32);
        let value = Rc::new(Cell::new((0.0, 1.0)));
        {
            let value = value.clone();
            level.set_draw_func(move |_, cr, w, h| {
                let (v, max) = value.get();
                draw_bar_level(cr, f64::from(w), f64::from(h), v, max);
            });
        }
        column.append(&level);
        pill.append(&column);
        window.set_child(Some(&pill));
        Rc::new(Self {
            window,
            icon,
            label,
            level,
            value,
            hide: RefCell::new(None),
            fade: transient::Player::new("osd"),
            level_anim: transient::Player::new("osd-level"),
        })
    }

    /// Show `request` on every monitor's OSD (Tuna Desktop draws one), as
    /// GNOME's `showAll`. Without an icon nothing shows, as in GNOME.
    pub fn show(self: &Rc<Self>, request: &OsdRequest) {
        let Some(icon) = request
            .icon
            .as_deref()
            .and_then(|s| gio::Icon::for_string(s).ok())
        else {
            return;
        };
        self.icon.set_from_gicon(&icon);
        self.label.set_visible(request.label.is_some());
        self.label.set_label(request.label.as_deref().unwrap_or(""));
        self.level.set_visible(request.level.is_some());
        let max = request.max_level.filter(|m| *m > 0.0).unwrap_or(1.0);
        let level = request.level.unwrap_or(0.0);
        // A showing OSD eases the bar to the new level (`setLevel`); a
        // hidden one jumps there.
        let shown = self.window.is_visible() && self.window.opacity() > 0.0;
        let from = if shown { self.value.get().0 } else { level };
        self.value.set((from, max));
        // The first visible row carries no bottom margin (St's
        // :first-child on `.level`).
        if request.label.is_some() {
            self.level.add_css_class("after-label");
        } else {
            self.level.remove_css_class("after-label");
        }
        self.level.queue_draw();
        // Shrink to the new content: a layer surface keeps its last size
        // unless asked for its natural one.
        self.window.set_default_size(1, 1);
        // Fade in from wherever a fade-out left it.
        let opacity = if self.window.is_visible() {
            self.window.opacity()
        } else {
            0.0
        };
        self.window.set_opacity(opacity);
        self.window.present();
        if opacity < 1.0 {
            let window = self.window.downgrade();
            self.fade.play(
                &self.window,
                transient::OSD_MS,
                move |t| {
                    if let Some(w) = window.upgrade() {
                        w.set_opacity(transient::fade(opacity, 1.0, t));
                    }
                },
                || {},
            );
        } else {
            // Cancel a fade-out that has not moved yet.
            self.fade.stop();
        }
        {
            let (value, area) = (self.value.clone(), self.level.downgrade());
            self.level_anim.play(
                &self.level,
                transient::OSD_MS,
                move |t| {
                    value.set((transient::fade(from, level, t), max));
                    if let Some(area) = area.upgrade() {
                        area.queue_draw();
                    }
                },
                || {},
            );
        }
        if let Some(id) = self.hide.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(HIDE_AFTER, move || {
            if let Some(ui) = weak.upgrade() {
                ui.hide.borrow_mut().take();
                ui.fade_out();
            }
        });
        *self.hide.borrow_mut() = Some(id);
    }

    /// GNOME's `_hide`: fade out, then hide.
    fn fade_out(&self) {
        let from = self.window.opacity();
        let (step, done) = (self.window.downgrade(), self.window.downgrade());
        self.fade.play(
            &self.window,
            transient::OSD_MS,
            move |t| {
                if let Some(w) = step.upgrade() {
                    w.set_opacity(transient::fade(from, 0.0, t));
                }
            },
            move || {
                if let Some(w) = done.upgrade() {
                    w.set_visible(false);
                }
            },
        );
    }
}

/// GNOME's BarLevel (`barLevel.js` vfunc_repaint, left to right): a 10%
/// white track, the level in white with round ends, and past 100% (a
/// `max` above 1) the overdrive in red behind a 3px separator.
fn draw_bar_level(cr: &gtk::cairo::Context, width: f64, height: f64, value: f64, max: f64) {
    let radius = width.min(LEVEL_HEIGHT) / 2.0;
    let top = (height - LEVEL_HEIGHT) / 2.0;
    let bottom = (height + LEVEL_HEIGHT) / 2.0;
    let overdrive_start = 1.0;
    let progress = if max > 0.0 { value / max } else { 0.0 };
    let end_x = radius + (width - 2.0 * radius) * progress;
    let separator_x = radius + (width - 2.0 * radius) * (overdrive_start / max);
    let overdrive = (overdrive_start - max).abs() > f64::EPSILON;
    let separator_w = if overdrive { SEPARATOR_WIDTH } else { 0.0 };
    let (arc_start, arc_end) = (radius, width - radius);
    let track = (1.0, 1.0, 1.0, 0.1);
    let active = (1.0, 1.0, 1.0, 1.0);
    let red = (
        0xc0 as f64 / 255.0,
        0x1c as f64 / 255.0,
        0x28 as f64 / 255.0,
        1.0,
    );
    let set = |c: (f64, f64, f64, f64)| cr.set_source_rgba(c.0, c.1, c.2, c.3);

    // Track, from the level's end to the right.
    cr.arc(arc_end, height / 2.0, radius, TAU * 0.75, TAU * 0.25);
    cr.line_to(end_x, bottom);
    cr.line_to(end_x, top);
    cr.line_to(arc_end, top);
    set(track);
    let _ = cr.fill();

    // The level up to the separator.
    let x = end_x.min(separator_x - separator_w / 2.0);
    cr.arc(arc_start, height / 2.0, radius, TAU * 0.25, TAU * 0.75);
    cr.line_to(x, top);
    cr.line_to(x, bottom);
    cr.line_to(arc_start, bottom);
    if value > 0.0 {
        set(active);
    }
    let _ = cr.fill();

    // Overdrive.
    let x = end_x.min(separator_x) + separator_w / 2.0;
    if value > overdrive_start {
        cr.move_to(x, top);
        cr.line_to(end_x, top);
        cr.line_to(end_x, bottom);
        cr.line_to(x, bottom);
        cr.line_to(x, top);
        set(red);
        let _ = cr.fill();
    }

    // The level's round end.
    if value > 0.0 {
        set(if value <= overdrive_start {
            active
        } else {
            red
        });
        cr.arc(end_x, height / 2.0, radius, TAU * 0.75, TAU * 0.25);
        cr.line_to(end_x.floor(), bottom);
        cr.line_to(end_x.floor(), top);
        cr.line_to(end_x, top);
        let _ = cr.fill();
    }

    // The separator.
    if overdrive {
        cr.rectangle(
            separator_x - separator_w / 2.0,
            top,
            separator_w,
            LEVEL_HEIGHT,
        );
        set(if value <= overdrive_start {
            active
        } else {
            track
        });
        let _ = cr.fill();
    }
}
