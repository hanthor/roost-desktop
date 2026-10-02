//! GNOME 51's screenshot UI (screenshot.js), opened by Print: the screen
//! frozen and dimmed outside the selection, a white-bordered selection
//! with round corner handles, and the panel: Selection / Screen / Window,
//! the photo and video switch, the capture button, the pointer toggle,
//! and the close button. Capturing saves to ~/Pictures/Screenshots, puts
//! the image on the clipboard and says so, as GNOME does.
//!
//! Not yet: screen recording (the video switch is greyed) and GNOME's
//! window picker (Window takes the focused window).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

/// `.screenshot-ui-area-selector-handle`: 24px.
const HANDLE: f64 = 24.0;
/// The panel: 329x168, 4em above the bottom.
const PANEL_W: i32 = 329;
const PANEL_H: i32 = 168;
const PANEL_BOTTOM: i32 = 59;

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

/// Says a screenshot was taken: (summary, body).
pub type Notify = Rc<dyn Fn(&str, &str)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Selection,
    Screen,
    Window,
}

pub struct ScreenshotUi {
    window: gtk::Window,
    canvas: gtk::DrawingArea,
    fixed: gtk::Fixed,
    panel: gtk::Box,
    close: gtk::Button,
    types: Vec<(Mode, gtk::ToggleButton)>,
    pointer: gtk::ToggleButton,
    frozen: RefCell<Option<gdk::Texture>>,
    selection: Cell<Option<Rect>>,
    mode: Cell<Mode>,
    grab: Cell<Option<(Grab, Rect, f64, f64)>>,
    /// Takes the focused window's screenshot (Window mode).
    shoot_window: Rc<dyn Fn()>,
    notify: Notify,
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
    pub fn new(app: &gtk::Application, shoot_window: Rc<dyn Fn()>, notify: Notify) -> Rc<Self> {
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
        // The canvas takes the drags; the panel stays clickable on top.
        fixed.set_can_target(true);
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
        cast.update_property(&[gtk::accessible::Property::Label("Screencast")]);
        // Screen recording is not built yet.
        cast.set_sensitive(false);
        shot.set_can_target(false);
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
            pointer,
            frozen: RefCell::new(None),
            selection: Cell::new(None),
            mode: Cell::new(Mode::Selection),
            grab: Cell::new(None),
            shoot_window,
            notify,
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
        // Drawing, moving and resizing the selection.
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
        ui.canvas.add_controller(drag_gesture);
        // GNOME's keys: Escape closes, Enter/Space captures, S/C/W pick
        // the mode, P toggles the pointer.
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
                    gdk::Key::w | gdk::Key::W => ui.set_mode(Mode::Window),
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
        self.canvas.queue_draw();
    }

    /// Freeze the screen and show the UI.
    pub fn open(self: &Rc<Self>) {
        let dir = glib::user_runtime_dir();
        let path = dir.join("roost-screenshot-ui.png");
        let weak = Rc::downgrade(self);
        gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |conn| {
            let Ok(conn) = conn else { return };
            let path2 = path.clone();
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
                    ui.show();
                },
            );
        });
    }

    fn show(self: &Rc<Self>) {
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
        self.set_mode(self.mode.get());
        self.window.present();
    }

    fn close(&self) {
        self.window.set_visible(false);
        self.frozen.borrow_mut().take();
    }

    fn draw(&self, cr: &gtk::cairo::Context, w: f64, h: f64) {
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
            Mode::Window => {
                // GNOME's window picker is not built: the screen dims and
                // the capture takes the focused window.
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
                cr.paint().ok();
            }
        }
    }

    fn capture(self: &Rc<Self>) {
        match self.mode.get() {
            Mode::Window => {
                self.close();
                (self.shoot_window)();
            }
            Mode::Screen | Mode::Selection => {
                let Some(texture) = self.frozen.borrow().clone() else {
                    return;
                };
                let (w, h) = (self.canvas.width().max(1), self.canvas.height().max(1));
                let crop = if self.mode.get() == Mode::Screen {
                    Rect {
                        x: 0.0,
                        y: 0.0,
                        w: f64::from(w),
                        h: f64::from(h),
                    }
                } else {
                    match self.selection.get() {
                        Some(sel) if sel.w >= 1.0 && sel.h >= 1.0 => sel,
                        _ => return,
                    }
                };
                self.close();
                match save(&texture, crop, f64::from(w), f64::from(h)) {
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
}
