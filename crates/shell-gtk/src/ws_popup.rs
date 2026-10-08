//! GNOME 51's workspace switcher popup (workspaceSwitcherPopup.js): a
//! row of workspace dots in an OSD pill near the bottom of the screen,
//! the active one larger and white, shown for 600 ms after a workspace
//! key switches outside the overview.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

/// DISPLAY_TIMEOUT.
const SHOW_FOR: Duration = Duration::from_millis(600);
/// Each `.ws-switcher-indicator` slot: 32px (dot plus margins).
const SLOT: f64 = 32.0;
/// `.workspace-switcher { spacing: 12px; padding: 12px 18px }`.
const SPACING: f64 = 12.0;
/// Dot diameters: 2.667px padding a side (inactive), 5.333px (active).
const DOT: f64 = 16.0 / 3.0;
const ACTIVE_DOT: f64 = 32.0 / 3.0;

/// The pill's content width for `count` workspaces.
pub fn content_width(count: u32) -> f64 {
    let n = f64::from(count.max(1));
    n * SLOT + (n - 1.0) * SPACING
}

pub struct WorkspacePopup {
    window: gtk::Window,
    dots: gtk::DrawingArea,
    state: Rc<Cell<(u32, u32)>>,
    hide: RefCell<Option<glib::SourceId>>,
}

impl WorkspacePopup {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("roost-workspace-popup");
        window.set_title(Some("Workspace Switcher"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(roost_shell_control::WORKSPACE_POPUP_NAMESPACE));
        window.set_anchor(Edge::Bottom, true);
        // `margin-bottom: 4em` at the shell's 14.666px.
        window.set_margin(Edge::Bottom, 59);
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::None);
        window.connect_realize(|w| {
            if let Some(surface) = w.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
            }
        });
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pill.add_css_class("workspace-switcher");
        let dots = gtk::DrawingArea::new();
        dots.set_content_height(SLOT as i32);
        let state = Rc::new(Cell::new((0u32, 1u32)));
        {
            let state = state.clone();
            dots.set_draw_func(move |_, cr, _, h| {
                let (active, count) = state.get();
                for i in 0..count.max(1) {
                    let cx = f64::from(i) * (SLOT + SPACING) + SLOT / 2.0;
                    let cy = f64::from(h) / 2.0;
                    let (d, alpha) = if i == active {
                        (ACTIVE_DOT, 1.0)
                    } else {
                        (DOT, 0.5)
                    };
                    cr.set_source_rgba(1.0, 1.0, 1.0, alpha);
                    cr.arc(cx, cy, d / 2.0, 0.0, std::f64::consts::TAU);
                    let _ = cr.fill();
                }
            });
        }
        pill.append(&dots);
        window.set_child(Some(&pill));
        Rc::new(Self {
            window,
            dots,
            state,
            hide: RefCell::new(None),
        })
    }

    /// Show workspace `index` of `count`.
    pub fn display(self: &Rc<Self>, index: u32, count: u32) {
        let count = count.max(index + 1);
        self.state.set((index, count));
        self.dots
            .set_content_width(content_width(count).round() as i32);
        self.dots.queue_draw();
        self.window.set_default_size(1, 1);
        self.window.present();
        if let Some(id) = self.hide.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(SHOW_FOR, move || {
            if let Some(ui) = weak.upgrade() {
                ui.hide.borrow_mut().take();
                ui.window.set_visible(false);
            }
        });
        *self.hide.borrow_mut() = Some(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pill_grows_32px_and_12px_a_workspace() {
        assert_eq!(content_width(1), 32.0);
        assert_eq!(content_width(2), 76.0);
        assert_eq!(content_width(3), 120.0);
    }
}
