//! GNOME 51's window preview chrome in the overview (windowPreview.js):
//! each preview's app icon, 70% over its bottom edge, and on the hovered
//! one the window title in a caption pill below and a round close button
//! on its top-right corner. The compositor draws the previews and tells
//! the shell where they are (`OverviewPreviews`); this click-through
//! overlay draws the chrome, taking input only on the close button.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use roost_shell_control::PreviewInfo;

/// `ICON_SIZE`, `ICON_OVERLAP` and `ICON_TITLE_SPACING`.
const ICON_SIZE: i32 = 64;
const ICON_OVERLAP: f64 = 0.7;
const ICON_TITLE_SPACING: f64 = 6.0;
/// The close button (`.window-close`): 32px.
const CLOSE_SIZE: i32 = 32;

/// What one preview shows: its window's title and app icon.
pub struct PreviewWindow {
    pub title: String,
    pub icon: gio::Icon,
}

pub struct PreviewChrome {
    window: gtk::ApplicationWindow,
    fixed: gtk::Fixed,
    shown: RefCell<(Vec<PreviewInfo>, Option<u64>)>,
    close: Rc<dyn Fn(u64)>,
}

impl PreviewChrome {
    pub fn new(app: &gtk::Application, close: Rc<dyn Fn(u64)>) -> Rc<Self> {
        let window = gtk::ApplicationWindow::new(app);
        window.add_css_class("roost-overview-chrome");
        window.init_layer_shell();
        window.set_layer(Layer::Top);
        window.set_namespace(Some(roost_shell_control::PREVIEW_CHROME_NAMESPACE));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        // Output coordinates, as the compositor sends them: ignore the
        // bar's exclusive zone.
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_title(Some("Window Previews"));
        let fixed = gtk::Fixed::new();
        window.set_child(Some(&fixed));
        // Click-through from the first frame: no input until a close
        // button needs it.
        window.connect_realize(|w| {
            if let Some(surface) = w.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
            }
        });
        Rc::new(Self {
            window,
            fixed,
            shown: RefCell::new((Vec::new(), None)),
            close,
        })
    }

    /// Show `previews` (empty hides the overlay); `windows` gives each
    /// preview's title and icon by window id.
    pub fn update(
        self: &Rc<Self>,
        previews: &[PreviewInfo],
        hovered: Option<u64>,
        windows: &dyn Fn(u64) -> Option<PreviewWindow>,
    ) {
        {
            let shown = self.shown.borrow();
            if shown.0 == previews && shown.1 == hovered {
                return;
            }
        }
        *self.shown.borrow_mut() = (previews.to_vec(), hovered);
        while let Some(child) = self.fixed.first_child() {
            self.fixed.remove(&child);
        }
        if previews.is_empty() {
            self.window.set_visible(false);
            return;
        }
        let mut input = None;
        for p in previews {
            let Some(info) = windows(p.window) else {
                continue;
            };
            let bottom = f64::from(p.y + p.height);
            let center_x = f64::from(p.x) + f64::from(p.width) / 2.0;
            let icon = gtk::Image::from_gicon(&info.icon);
            icon.set_pixel_size(ICON_SIZE);
            icon.add_css_class("window-icon");
            self.fixed.put(
                &icon,
                center_x - f64::from(ICON_SIZE) / 2.0,
                bottom - f64::from(ICON_SIZE) * ICON_OVERLAP,
            );
            if hovered != Some(p.window) {
                continue;
            }
            // Centered under the preview, never wider than it.
            let caption = gtk::Label::new(Some(&info.title));
            caption.add_css_class("window-caption");
            caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
            caption.set_max_width_chars(((p.width - 24) / 8).max(1));
            caption.set_halign(gtk::Align::Center);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.set_size_request(p.width, -1);
            row.append(&caption);
            caption.set_hexpand(true);
            self.fixed.put(
                &row,
                f64::from(p.x),
                bottom + f64::from(ICON_SIZE) * (1.0 - ICON_OVERLAP) + ICON_TITLE_SPACING,
            );
            let close = gtk::Button::from_icon_name("preview-close-symbolic");
            close.add_css_class("window-close");
            close.update_property(&[gtk::accessible::Property::Label("Close")]);
            let (x, y) = (p.x + p.width - CLOSE_SIZE / 2, p.y - CLOSE_SIZE / 2);
            self.fixed.put(&close, f64::from(x), f64::from(y));
            {
                let (close_fn, id) = (self.close.clone(), p.window);
                close.connect_clicked(move |_| close_fn(id));
            }
            input = Some(gtk::cairo::RectangleInt::new(x, y, CLOSE_SIZE, CLOSE_SIZE));
        }
        self.window.set_visible(true);
        // Clicks fall through to the compositor's previews except on
        // the close button.
        let region = match input {
            Some(rect) => gtk::cairo::Region::create_rectangle(&rect),
            None => gtk::cairo::Region::create(),
        };
        if let Some(surface) = self.window.surface() {
            surface.set_input_region(Some(&region));
        }
    }
}
