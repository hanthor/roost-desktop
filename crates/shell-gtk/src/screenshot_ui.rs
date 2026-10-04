//! GNOME 51's screenshot UI (screenshot.js), opened by Print: the screen
//! frozen and dimmed outside the selection, a white-bordered selection
//! with round corner handles, and the panel: Selection / Screen / Window,
//! the photo and video switch, the capture button, the pointer toggle,
//! and the close button. Capturing saves to ~/Pictures/Screenshots, puts
//! the image on the clipboard and says so, as GNOME does.
//!
//! Window shows GNOME's window selector (`UIWindowSelector`): the active
//! workspace's windows, pictured when the UI opened and spread out as
//! the overview spreads them, on the dark system background; the focused
//! one starts selected, the selection ringed in the accent color with a
//! check, and Capture saves that window alone at full size. The video
//! switch records the selection or the screen instead
//! (`screencast.rs`), with GNOME's recording indicator in the panel.
//!
//! Not yet: the pointer in screenshots and recordings, and one window
//! selector per monitor (the primary one shows them all).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::screencast::Recorder;

/// `.screenshot-ui-area-selector-handle`: 24px.
const HANDLE: f64 = 24.0;
/// The panel: 329x168, 4em above the bottom.
const PANEL_W: i32 = 329;
const PANEL_H: i32 = 168;
const PANEL_BOTTOM: i32 = 59;
/// `.screenshot-ui-window-selector-window-border`: 6px, outside the
/// window.
const WINDOW_BORDER: i32 = 6;
/// `.screenshot-ui-window-selector`: `$system_base_color` (dark).
const SELECTOR_BACKGROUND: (f64, f64, f64) = (34.0 / 255.0, 34.0 / 255.0, 38.0 / 255.0);

/// A rectangle in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// GNOME's first selection: a quarter of the screen, centred.
pub fn initial_selection(width: f64, height: f64) -> Rect {
    let (w, h) = ((width / 4.0).round(), (height / 4.0).round());
    Rect {
        x: ((width - w) / 2.0).round(),
        y: ((height - h) / 2.0).round(),
        w,
        h,
    }
}

/// What a press at a point grabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    /// A corner handle: 0 top-left, 1 top-right, 2 bottom-left, 3 bottom-right.
    Corner(u8),
    /// Inside the selection: move it.
    Move,
    /// Elsewhere: draw a new one.
    New,
}

/// What a press at (`x`, `y`) grabs on `sel`.
pub fn hit(sel: &Rect, x: f64, y: f64) -> Grab {
    let corners = [
        (sel.x, sel.y),
        (sel.x + sel.w, sel.y),
        (sel.x, sel.y + sel.h),
        (sel.x + sel.w, sel.y + sel.h),
    ];
    for (i, (cx, cy)) in corners.iter().enumerate() {
        if (x - cx).hypot(y - cy) <= HANDLE / 2.0 + 4.0 {
            return Grab::Corner(i as u8);
        }
    }
    if x >= sel.x && x <= sel.x + sel.w && y >= sel.y && y <= sel.y + sel.h {
        Grab::Move
    } else {
        Grab::New
    }
}

/// The selection after dragging `grab` from `start` (the selection
/// then), pressed at `press` and moved by `delta`, kept on a `screen`
/// sized output.
pub fn drag(
    grab: Grab,
    start: Rect,
    (px, py): (f64, f64),
    (dx, dy): (f64, f64),
    (width, height): (f64, f64),
) -> Rect {
    let span = |a: f64, b: f64| (a.min(b), (a - b).abs());
    match grab {
        Grab::Move => Rect {
            x: (start.x + dx).clamp(0.0, (width - start.w).max(0.0)),
            y: (start.y + dy).clamp(0.0, (height - start.h).max(0.0)),
            ..start
        },
        Grab::New => {
            let (x, w) = span(px, (px + dx).clamp(0.0, width));
            let (y, h) = span(py, (py + dy).clamp(0.0, height));
            Rect { x, y, w, h }
        }
        Grab::Corner(c) => {
            // The opposite corner stays put.
            let (fx, fy) = (
                if c % 2 == 0 {
                    start.x + start.w
                } else {
                    start.x
                },
                if c < 2 { start.y + start.h } else { start.y },
            );
            let (mx, my) = (
                if c % 2 == 0 {
                    start.x
                } else {
                    start.x + start.w
                },
                if c < 2 { start.y } else { start.y + start.h },
            );
            let (x, w) = span(fx, (mx + dx).clamp(0.0, width));
            let (y, h) = span(fy, (my + dy).clamp(0.0, height));
            Rect { x, y, w, h }
        }
    }
}

/// GNOME's file name: `Screenshot From 2026-10-02 03-18-00.png`.
pub fn file_name(now: &jiff::civil::DateTime) -> String {
    format!("Screenshot From {}.png", now.strftime("%Y-%m-%d %H-%M-%S"))
}

/// One window as the compositor's `ScreenshotWindows` answers it.
#[derive(Debug, Clone, PartialEq)]
pub struct ShotWindow {
    pub id: u64,
    pub title: String,
    pub focused: bool,
    /// Its slot in the selector (logical pixels).
    pub slot: Rect,
    pub path: String,
}

/// Unpack a `ScreenshotWindows` reply, `(a(tsbiiiis))`.
pub fn parse_windows(reply: &glib::Variant) -> Vec<ShotWindow> {
    let Some(list) = reply.try_child_value(0) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|w| w.get::<(u64, String, bool, i32, i32, i32, i32, String)>())
        .map(|(id, title, focused, x, y, w, h, path)| ShotWindow {
            id,
            title,
            focused,
            slot: Rect {
                x: f64::from(x),
                y: f64::from(y),
                w: f64::from(w),
                h: f64::from(h),
            },
            path,
        })
        .collect()
}

/// Which window starts selected: the focused one, else the first (as
/// GNOME checks the window with focus).
pub fn initially_selected(windows: &[ShotWindow]) -> Option<usize> {
    windows
        .iter()
        .position(|w| w.focused)
        .or((!windows.is_empty()).then_some(0))
}

/// Says a screenshot was taken: (summary, body).
pub type Notify = Rc<dyn Fn(&str, &str)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Selection,
    Screen,
    Window,
}

/// One window in the selector: its picture and its button.
struct PickerWindow {
    texture: gdk::Texture,
    button: gtk::ToggleButton,
}

pub struct ScreenshotUi {
    window: gtk::Window,
    canvas: gtk::DrawingArea,
    fixed: gtk::Fixed,
    panel: gtk::Box,
    close: gtk::Button,
    types: Vec<(Mode, gtk::ToggleButton)>,
    cast: gtk::ToggleButton,
    capture: gtk::Button,
    pointer: gtk::ToggleButton,
    frozen: RefCell<Option<gdk::Texture>>,
    /// The window selector's windows, pictured on open.
    windows: RefCell<Vec<PickerWindow>>,
    selection: Cell<Option<Rect>>,
    mode: Cell<Mode>,
    grab: Cell<Option<(Grab, Rect, f64, f64)>>,
    /// Takes the focused window's screenshot (Window mode, when the
    /// compositor cannot picture every window).
    shoot_window: Rc<dyn Fn()>,
    notify: Notify,
    recorder: Rc<Recorder>,
}

fn icon_label_button(icon: &str, label: &str) -> gtk::ToggleButton {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(32);
    column.append(&image);
    let text = gtk::Label::new(Some(label));
    text.add_css_class("screenshot-ui-type-label");
    column.append(&text);
    let button = gtk::ToggleButton::builder().child(&column).build();
    button.add_css_class("screenshot-ui-type-button");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

impl ScreenshotUi {
    pub fn new(
        app: &gtk::Application,
        shoot_window: Rc<dyn Fn()>,
        notify: Notify,
        recorder: Rc<Recorder>,
    ) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("roost-screenshot-ui");
        window.set_title(Some("Screenshot"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("roost-screenshot-ui"));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);

        let canvas = gtk::DrawingArea::new();
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        let fixed = gtk::Fixed::new();
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&canvas));
        overlay.add_overlay(&fixed);
        window.set_child(Some(&overlay));

        // The panel.
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 12);
        panel.add_css_class("screenshot-ui-panel");
        panel.set_size_request(PANEL_W, PANEL_H);
        let type_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let types = vec![
            (
                Mode::Selection,
                icon_label_button("screenshot-ui-area-symbolic", "Selection"),
            ),
            (
                Mode::Screen,
                icon_label_button("screenshot-ui-display-symbolic", "Screen"),
            ),
            (
                Mode::Window,
                icon_label_button("screenshot-ui-window-symbolic", "Window"),
            ),
        ];
        for (_, b) in &types {
            // GNOME's mode buttons: 89x76 each, 12px apart.
            b.set_size_request(89, 76);
            type_row.append(b);
        }
        panel.append(&type_row);
        let bottom = gtk::CenterBox::new();
        let shot_cast = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        shot_cast.add_css_class("screenshot-ui-shot-cast-container");
        shot_cast.set_valign(gtk::Align::Center);
        let shot = gtk::ToggleButton::builder()
            .child(&gtk::Image::from_icon_name("camera-photo-symbolic"))
            .active(true)
            .build();
        shot.add_css_class("screenshot-ui-shot-cast-button");
        shot.update_property(&[gtk::accessible::Property::Label("Screenshot")]);
        let cast = gtk::ToggleButton::builder()
            .child(&gtk::Image::from_icon_name("camera-web-symbolic"))
            .build();
        cast.add_css_class("screenshot-ui-shot-cast-button");
        cast.update_property(&[gtk::accessible::Property::Label("Record Screen")]);
        // The photo and video switch: one of the two is always on.
        cast.set_group(Some(&shot));
        // GNOME shows the switch only where screencasts work.
        cast.set_sensitive(crate::screencast::supported());
        shot_cast.append(&shot);
        shot_cast.append(&cast);
        bottom.set_start_widget(Some(&shot_cast));
        let capture = gtk::Button::new();
        capture.add_css_class("screenshot-ui-capture-button");
        let circle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        circle.add_css_class("screenshot-ui-capture-button-circle");
        capture.set_child(Some(&circle));
        capture.set_valign(gtk::Align::Center);
        capture.set_halign(gtk::Align::Center);
        capture.update_property(&[gtk::accessible::Property::Label("Capture")]);
        bottom.set_center_widget(Some(&capture));
        let pointer = gtk::ToggleButton::builder()
            .child(&gtk::Image::from_icon_name(
                "screenshot-ui-show-pointer-symbolic",
            ))
            .build();
        pointer.add_css_class("screenshot-ui-show-pointer-button");
        pointer.set_valign(gtk::Align::Center);
        pointer.update_property(&[gtk::accessible::Property::Label("Show Pointer")]);
        bottom.set_end_widget(Some(&pointer));
        panel.append(&bottom);
        fixed.put(&panel, 0.0, 0.0);

        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("screenshot-ui-close-button");
        close.update_property(&[gtk::accessible::Property::Label("Close")]);
        fixed.put(&close, 0.0, 0.0);

        let ui = Rc::new(Self {
            window,
            canvas,
            fixed,
            panel,
            close,
            types,
            cast,
            capture: capture.clone(),
            pointer,
            frozen: RefCell::new(None),
            windows: RefCell::new(Vec::new()),
            selection: Cell::new(None),
            mode: Cell::new(Mode::Selection),
            grab: Cell::new(None),
            shoot_window,
            notify,
            recorder,
        });

        {
            let weak = Rc::downgrade(&ui);
            ui.canvas.set_draw_func(move |_, cr, w, h| {
                if let Some(ui) = weak.upgrade() {
                    ui.draw(cr, f64::from(w), f64::from(h));
                }
            });
        }
        for (mode, button) in &ui.types {
            let (weak, mode) = (Rc::downgrade(&ui), *mode);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.set_mode(mode);
                }
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            ui.cast.connect_toggled(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.sync_cast();
                }
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            capture.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.capture();
                }
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            ui.close.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.close();
                }
            });
        }
        // Drawing, moving and resizing the selection. The gesture sits
        // on the overlay, so presses the panel and window buttons leave
        // unclaimed reach it wherever they land.
        let drag_gesture = gtk::GestureDrag::new();
        {
            let weak = Rc::downgrade(&ui);
            drag_gesture.connect_drag_begin(move |_, x, y| {
                let Some(ui) = weak.upgrade() else { return };
                if ui.mode.get() != Mode::Selection {
                    return;
                }
                let sel = ui.selection.get().unwrap_or(Rect {
                    x,
                    y,
                    w: 0.0,
                    h: 0.0,
                });
                ui.grab.set(Some((hit(&sel, x, y), sel, x, y)));
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            drag_gesture.connect_drag_update(move |_, dx, dy| {
                let Some(ui) = weak.upgrade() else { return };
                let Some((grab, start, px, py)) = ui.grab.get() else {
                    return;
                };
                let (w, h) = (f64::from(ui.canvas.width()), f64::from(ui.canvas.height()));
                ui.selection
                    .set(Some(drag(grab, start, (px, py), (dx, dy), (w, h))));
                ui.canvas.queue_draw();
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            drag_gesture.connect_drag_end(move |_, _, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.grab.set(None);
                }
            });
        }
        overlay.add_controller(drag_gesture);
        // GNOME's keys: Escape closes, Enter/Space captures, S/C/W pick
        // the mode, V switches between screenshot and screencast, P
        // toggles the pointer.
        let keys = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(&ui);
            keys.connect_key_pressed(move |_, key, _, _| {
                let Some(ui) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                match key {
                    gdk::Key::Escape => ui.close(),
                    gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space => ui.capture(),
                    gdk::Key::s | gdk::Key::S => ui.set_mode(Mode::Selection),
                    gdk::Key::c | gdk::Key::C => ui.set_mode(Mode::Screen),
                    gdk::Key::w | gdk::Key::W => {
                        if !ui.cast.is_active() {
                            ui.set_mode(Mode::Window);
                        }
                    }
                    gdk::Key::v | gdk::Key::V => {
                        if ui.cast.is_sensitive() {
                            ui.set_cast(!ui.cast.is_active());
                        }
                    }
                    gdk::Key::p | gdk::Key::P => ui.pointer.set_active(!ui.pointer.is_active()),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            });
        }
        ui.window.add_controller(keys);
        ui.set_mode(Mode::Selection);
        ui
    }

    fn set_mode(&self, mode: Mode) {
        self.mode.set(mode);
        for (m, button) in &self.types {
            button.set_active(*m == mode);
        }
        for w in self.windows.borrow().iter() {
            w.button.set_visible(mode == Mode::Window);
        }
        self.canvas.queue_draw();
    }

    /// Switch between screenshot and screencast.
    fn set_cast(&self, cast: bool) {
        if cast {
            self.cast.set_active(true);
        } else if let Some(shot) = self.shot_button() {
            shot.set_active(true);
        }
        self.sync_cast();
    }

    fn shot_button(&self) -> Option<gtk::ToggleButton> {
        self.cast
            .prev_sibling()
            .and_then(|w| w.downcast::<gtk::ToggleButton>().ok())
    }

    /// GNOME's screencast mode (`_onCastButtonToggled`): the frozen
    /// screen gives way to the live one, the capture button turns red,
    /// and Window is off (recording a window is not supported).
    fn sync_cast(&self) {
        let cast = self.cast.is_active();
        if cast {
            self.capture.add_css_class("cast");
            if self.mode.get() == Mode::Window {
                self.set_mode(Mode::Selection);
            }
        } else {
            self.capture.remove_css_class("cast");
        }
        for (m, button) in &self.types {
            if *m == Mode::Window {
                button.set_sensitive(!cast);
            }
        }
        self.canvas.queue_draw();
    }

    /// Freeze the screen and show the UI.
    pub fn open(self: &Rc<Self>) {
        self.open_in(false);
    }

    /// GNOME's `show-screen-recording-ui`: the UI in screencast mode, or
    /// the recording stopped when one is running.
    pub fn open_recording(self: &Rc<Self>) {
        if self.recorder.in_progress() {
            self.recorder.stop(|_| {});
            return;
        }
        self.open_in(true);
    }

    fn open_in(self: &Rc<Self>, cast: bool) {
        if self.window.is_visible() {
            return;
        }
        let dir = glib::user_runtime_dir();
        let path = dir.join("roost-screenshot-ui.png");
        let windows_dir = dir.join("roost-screenshot-windows");
        let weak = Rc::downgrade(self);
        gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |conn| {
            let Ok(conn) = conn else { return };
            let path2 = path.clone();
            let conn2 = conn.clone();
            conn.call(
                Some("org.gnome.Shell.Screenshot"),
                "/org/gnome/Shell/Screenshot",
                "org.gnome.Shell.Screenshot",
                "Screenshot",
                Some(&(false, false, path.to_string_lossy().as_ref()).to_variant()),
                glib::VariantTy::new("(bs)").ok(),
                gio::DBusCallFlags::NONE,
                10_000,
                gio::Cancellable::NONE,
                move |reply| {
                    let Some(ui) = weak.upgrade() else { return };
                    if reply.is_err() {
                        eprintln!("roost-shell-gtk: screenshot UI could not freeze the screen");
                        return;
                    }
                    let texture = gdk::Texture::from_filename(&path2).ok();
                    let _ = std::fs::remove_file(&path2);
                    let Some(texture) = texture else { return };
                    *ui.frozen.borrow_mut() = Some(texture);
                    // Every window's picture for the window selector, as
                    // GNOME takes them when the UI opens.
                    let weak = Rc::downgrade(&ui);
                    conn2.call(
                        Some("org.gnome.Shell.Screenshot"),
                        "/org/gnome/Shell/Screenshot",
                        "org.roost.Screenshot",
                        "ScreenshotWindows",
                        Some(&(windows_dir.to_string_lossy().as_ref(),).to_variant()),
                        glib::VariantTy::new("(a(tsbiiiis))").ok(),
                        gio::DBusCallFlags::NONE,
                        10_000,
                        gio::Cancellable::NONE,
                        move |reply| {
                            let Some(ui) = weak.upgrade() else { return };
                            let windows = match reply {
                                Ok(reply) => parse_windows(&reply),
                                Err(e) => {
                                    eprintln!("roost-shell-gtk: no window selector: {e}");
                                    Vec::new()
                                }
                            };
                            ui.set_windows(windows);
                            ui.show(cast);
                        },
                    );
                },
            );
        });
    }

    /// Build the window selector's buttons (screenshot.js
    /// `UIWindowSelectorWindow`): the picture in its slot, the 6px border
    /// around it and the check in the middle.
    fn set_windows(self: &Rc<Self>, shots: Vec<ShotWindow>) {
        for old in self.windows.borrow_mut().drain(..) {
            self.fixed.remove(&old.button);
        }
        let selected = initially_selected(&shots);
        let mut group: Option<gtk::ToggleButton> = None;
        let mut windows = Vec::new();
        for (index, shot) in shots.into_iter().enumerate() {
            let texture = gdk::Texture::from_filename(&shot.path).ok();
            let _ = std::fs::remove_file(&shot.path);
            let Some(texture) = texture else { continue };
            let (w, h) = (
                shot.slot.w.round().max(1.0) as i32,
                shot.slot.h.round().max(1.0) as i32,
            );
            let picture = gtk::DrawingArea::new();
            picture.set_content_width(w);
            picture.set_content_height(h);
            if let Ok(surface) = texture_surface(&texture) {
                let (tw, th) = (f64::from(texture.width()), f64::from(texture.height()));
                picture.set_draw_func(move |_, cr, w, h| {
                    cr.scale(f64::from(w) / tw, f64::from(h) / th);
                    let _ = cr.set_source_surface(&surface, 0.0, 0.0);
                    cr.source().set_filter(gtk::cairo::Filter::Good);
                    let _ = cr.paint();
                });
            }
            let check = gtk::Image::from_icon_name("object-select-symbolic");
            check.add_css_class("screenshot-ui-window-selector-check");
            check.set_halign(gtk::Align::Center);
            check.set_valign(gtk::Align::Center);
            let content = gtk::Overlay::new();
            content.set_child(Some(&picture));
            content.add_overlay(&check);
            let button = gtk::ToggleButton::builder().child(&content).build();
            button.add_css_class("screenshot-ui-window-selector-window");
            let title = if shot.title.is_empty() {
                "Window".to_owned()
            } else {
                shot.title.clone()
            };
            button.update_property(&[gtk::accessible::Property::Label(&title)]);
            button.set_tooltip_text(Some(&title));
            button.set_size_request(w + 2 * WINDOW_BORDER, h + 2 * WINDOW_BORDER);
            match &group {
                Some(first) => button.set_group(Some(first)),
                None => group = Some(button.clone()),
            }
            button.set_active(Some(index) == selected);
            // Focusing a window selects it, as in GNOME.
            button.connect_has_focus_notify(|b| {
                if b.has_focus() {
                    b.set_active(true);
                }
            });
            button.set_visible(self.mode.get() == Mode::Window);
            self.fixed.put(
                &button,
                shot.slot.x - f64::from(WINDOW_BORDER),
                shot.slot.y - f64::from(WINDOW_BORDER),
            );
            windows.push(PickerWindow { texture, button });
        }
        *self.windows.borrow_mut() = windows;
        // The panel and the close button stay above the windows.
        self.panel.insert_before(&self.fixed, None::<&gtk::Widget>);
        self.close.insert_before(&self.fixed, None::<&gtk::Widget>);
    }

    fn show(self: &Rc<Self>, cast: bool) {
        let (w, h) = self
            .frozen
            .borrow()
            .as_ref()
            .map(|t| (t.width(), t.height()))
            .unwrap_or((1280, 800));
        let monitor = WidgetExt::display(&self.window)
            .monitors()
            .item(0)
            .and_then(|m| m.downcast::<gdk::Monitor>().ok())
            .map(|m| m.geometry())
            .map(|g| (g.width(), g.height()))
            .unwrap_or((w, h));
        if self.selection.get().is_none() {
            self.selection.set(Some(initial_selection(
                f64::from(monitor.0),
                f64::from(monitor.1),
            )));
        }
        let px = (monitor.0 - PANEL_W) / 2;
        let py = monitor.1 - PANEL_BOTTOM - PANEL_H;
        self.fixed.move_(&self.panel, f64::from(px), f64::from(py));
        self.fixed.move_(
            &self.close,
            f64::from(px + PANEL_W - 30),
            f64::from(py - 18),
        );
        self.set_cast(cast);
        self.set_mode(self.mode.get());
        self.window.present();
    }

    fn close(&self) {
        self.window.set_visible(false);
        self.frozen.borrow_mut().take();
        for old in self.windows.borrow_mut().drain(..) {
            self.fixed.remove(&old.button);
        }
    }

    fn draw(&self, cr: &gtk::cairo::Context, w: f64, h: f64) {
        if self.mode.get() == Mode::Window {
            // The window selector covers the screen.
            let (r, g, b) = SELECTOR_BACKGROUND;
            cr.set_source_rgb(r, g, b);
            let _ = cr.paint();
            return;
        }
        // Screencast mode shows the live screen through the UI.
        if !self.cast.is_active() {
            let Some(texture) = self.frozen.borrow().clone() else {
                return;
            };
            // The frozen screen, scaled to the output's logical size.
            let sx = w / f64::from(texture.width());
            let sy = h / f64::from(texture.height());
            cr.save().ok();
            cr.scale(sx, sy);
            if let Ok(surface) = texture_surface(&texture) {
                let _ = cr.set_source_surface(&surface, 0.0, 0.0);
                let _ = cr.paint();
            }
            cr.restore().ok();
        }
        match self.mode.get() {
            Mode::Selection => {
                let Some(sel) = self.selection.get() else {
                    return;
                };
                // Shade outside the selection at 50% (four rectangles).
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
                cr.rectangle(0.0, 0.0, w, sel.y);
                cr.rectangle(0.0, sel.y + sel.h, w, h - sel.y - sel.h);
                cr.rectangle(0.0, sel.y, sel.x, sel.h);
                cr.rectangle(sel.x + sel.w, sel.y, w - sel.x - sel.w, sel.h);
                let _ = cr.fill();
                // The 2px white border, inside the selection.
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.set_line_width(2.0);
                cr.rectangle(
                    sel.x + 1.0,
                    sel.y + 1.0,
                    (sel.w - 2.0).max(0.0),
                    (sel.h - 2.0).max(0.0),
                );
                let _ = cr.stroke();
                // The handles: 24px white discs on a soft shadow.
                for (cx, cy) in [
                    (sel.x, sel.y),
                    (sel.x + sel.w, sel.y),
                    (sel.x, sel.y + sel.h),
                    (sel.x + sel.w, sel.y + sel.h),
                ] {
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.2);
                    cr.arc(cx, cy + 1.0, HANDLE / 2.0 + 2.0, 0.0, std::f64::consts::TAU);
                    let _ = cr.fill();
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    cr.arc(cx, cy, HANDLE / 2.0, 0.0, std::f64::consts::TAU);
                    let _ = cr.fill();
                }
            }
            Mode::Screen => {
                // The whole screen selected: a 2px white frame.
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.set_line_width(2.0);
                cr.rectangle(1.0, 1.0, w - 2.0, h - 2.0);
                let _ = cr.stroke();
            }
            Mode::Window => {}
        }
    }

    fn capture(self: &Rc<Self>) {
        let (w, h) = (self.canvas.width().max(1), self.canvas.height().max(1));
        let full = Rect {
            x: 0.0,
            y: 0.0,
            w: f64::from(w),
            h: f64::from(h),
        };
        if self.cast.is_active() {
            let area = match self.mode.get() {
                Mode::Selection => match self.selection.get() {
                    Some(sel) if sel.w >= 1.0 && sel.h >= 1.0 => sel,
                    _ => return,
                },
                _ => full,
            };
            // Close first, so the UI is not recorded.
            self.close();
            let recorder = self.recorder.clone();
            let area = (
                area.x.round() as i32,
                area.y.round() as i32,
                area.w.round() as i32,
                area.h.round() as i32,
            );
            glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                recorder.start(Some(area), crate::screencast::TEMPLATE, None, |result| {
                    if let Err(e) = result {
                        eprintln!("roost-shell-gtk: screencast did not start: {e}");
                    }
                });
            });
            return;
        }
        match self.mode.get() {
            Mode::Window => {
                let chosen = self
                    .windows
                    .borrow()
                    .iter()
                    .find(|w| w.button.is_active())
                    .map(|w| w.texture.clone());
                self.close();
                match chosen {
                    // The window's own picture, at its full size.
                    Some(texture) => {
                        let (tw, th) = (f64::from(texture.width()), f64::from(texture.height()));
                        let whole = Rect {
                            x: 0.0,
                            y: 0.0,
                            w: tw,
                            h: th,
                        };
                        self.saved(save(&texture, whole, tw, th));
                    }
                    // No selector (the compositor could not picture the
                    // windows): the focused window, as before.
                    None => (self.shoot_window)(),
                }
            }
            Mode::Screen | Mode::Selection => {
                let Some(texture) = self.frozen.borrow().clone() else {
                    return;
                };
                let crop = if self.mode.get() == Mode::Screen {
                    full
                } else {
                    match self.selection.get() {
                        Some(sel) if sel.w >= 1.0 && sel.h >= 1.0 => sel,
                        _ => return,
                    }
                };
                self.close();
                self.saved(save(&texture, crop, f64::from(w), f64::from(h)));
            }
        }
    }

    /// Put a saved screenshot on the clipboard and say so.
    fn saved(&self, result: Option<(std::path::PathBuf, gdk::Texture)>) {
        match result {
            Some((_, cropped)) => {
                if let Some(display) = gdk::Display::default() {
                    display.clipboard().set_texture(&cropped);
                }
                (self.notify)(
                    "Screenshot captured",
                    "You can paste the image from the clipboard.",
                );
            }
            None => eprintln!("roost-shell-gtk: screenshot UI could not save"),
        }
    }
}

/// A cairo surface holding `texture`'s pixels.
fn texture_surface(texture: &gdk::Texture) -> Result<gtk::cairo::ImageSurface, gtk::cairo::Error> {
    let (w, h) = (texture.width(), texture.height());
    let mut surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h)?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface
            .data()
            .map_err(|_| gtk::cairo::Error::SurfaceFinished)?;
        texture.download(&mut data, stride);
    }
    surface.mark_dirty();
    Ok(surface)
}

/// Crop `texture` (shown at `w` x `h`) to `crop`, save it where GNOME
/// saves screenshots, and return the path and the cropped image.
fn save(
    texture: &gdk::Texture,
    crop: Rect,
    w: f64,
    h: f64,
) -> Option<(std::path::PathBuf, gdk::Texture)> {
    let sx = f64::from(texture.width()) / w;
    let sy = f64::from(texture.height()) / h;
    let (x, y) = ((crop.x * sx).round() as i32, (crop.y * sy).round() as i32);
    let cw = ((crop.w * sx).round() as i32).clamp(1, texture.width() - x);
    let ch = ((crop.h * sy).round() as i32).clamp(1, texture.height() - y);
    let surface = texture_surface(texture).ok()?;
    let mut out = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, cw, ch).ok()?;
    {
        let cr = gtk::cairo::Context::new(&out).ok()?;
        cr.set_source_surface(&surface, -f64::from(x), -f64::from(y))
            .ok()?;
        cr.paint().ok()?;
    }
    out.flush();
    let stride = out.stride() as usize;
    let bytes = glib::Bytes::from(&out.data().ok()?[..]);
    let cropped: gdk::Texture = gdk::MemoryTexture::new(
        cw,
        ch,
        gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &bytes,
        stride,
    )
    .upcast();
    // Where the compositor's screenshots go too: XDG_PICTURES_DIR, the
    // user's Pictures directory, else ~/Pictures.
    let pictures = std::env::var_os("XDG_PICTURES_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| glib::user_special_dir(glib::UserDirectory::Pictures))
        .unwrap_or_else(|| glib::home_dir().join("Pictures"));
    let dir = pictures.join("Screenshots");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(file_name(&jiff::Zoned::now().datetime()));
    cropped.save_to_png(&path).ok()?;
    Some((path, cropped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_selection_is_a_centred_quarter() {
        assert_eq!(
            initial_selection(1280.0, 800.0),
            Rect {
                x: 480.0,
                y: 300.0,
                w: 320.0,
                h: 200.0
            }
        );
    }

    #[test]
    fn drags_move_resize_and_draw() {
        let sel = initial_selection(1280.0, 800.0);
        assert_eq!(hit(&sel, 480.0, 300.0), Grab::Corner(0));
        assert_eq!(hit(&sel, 800.0, 500.0), Grab::Corner(3));
        assert_eq!(hit(&sel, 640.0, 400.0), Grab::Move);
        assert_eq!(hit(&sel, 100.0, 100.0), Grab::New);
        let moved = drag(
            Grab::Move,
            sel,
            (640.0, 400.0),
            (10.0, -20.0),
            (1280.0, 800.0),
        );
        assert_eq!((moved.x, moved.y, moved.w), (490.0, 280.0, 320.0));
        // Pulling the bottom-right corner past the top-left flips it.
        let flipped = drag(
            Grab::Corner(3),
            sel,
            (800.0, 500.0),
            (-400.0, -300.0),
            (1280.0, 800.0),
        );
        assert_eq!(
            flipped,
            Rect {
                x: 400.0,
                y: 200.0,
                w: 80.0,
                h: 100.0
            }
        );
        let drawn = drag(
            Grab::New,
            sel,
            (100.0, 100.0),
            (50.0, 40.0),
            (1280.0, 800.0),
        );
        assert_eq!(
            drawn,
            Rect {
                x: 100.0,
                y: 100.0,
                w: 50.0,
                h: 40.0
            }
        );
        // Moves stay on the screen.
        let edge = drag(
            Grab::Move,
            sel,
            (640.0, 400.0),
            (2000.0, 0.0),
            (1280.0, 800.0),
        );
        assert_eq!(edge.x, 960.0);
    }

    #[test]
    fn files_are_named_like_gnome() {
        let t = jiff::civil::date(2026, 10, 2).at(3, 18, 0, 0);
        assert_eq!(file_name(&t), "Screenshot From 2026-10-02 03-18-00.png");
    }

    #[test]
    fn the_window_selector_reads_the_compositors_answer() {
        let windows = vec![
            (
                3u64,
                "Alpha".to_owned(),
                false,
                100i32,
                120i32,
                400i32,
                300i32,
                "/run/a.png".to_owned(),
            ),
            (
                5u64,
                "Beta".to_owned(),
                true,
                520,
                120,
                400,
                300,
                "/run/b.png".to_owned(),
            ),
        ];
        let reply = (windows,).to_variant();
        assert_eq!(reply.type_().as_str(), "(a(tsbiiiis))");
        let parsed = parse_windows(&reply);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].title, "Beta");
        assert_eq!(
            parsed[1].slot,
            Rect {
                x: 520.0,
                y: 120.0,
                w: 400.0,
                h: 300.0
            }
        );
        // The focused window starts selected; with none, the first.
        assert_eq!(initially_selected(&parsed), Some(1));
        let mut unfocused = parsed.clone();
        unfocused[1].focused = false;
        assert_eq!(initially_selected(&unfocused), Some(0));
        assert_eq!(initially_selected(&[]), None);
    }
}
