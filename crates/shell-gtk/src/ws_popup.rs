//! GNOME 51's workspace switcher popup (workspaceSwitcherPopup.js): a
//! row of workspace dots in an OSD pill near the bottom of the screen,
//! the active one larger and white, shown for 600 ms after a workspace
//! key switches outside the overview. It fades in and out over 100 ms
//! (`ANIMATION_TIME`, ease-out-quad); a key while it shows keeps it
//! opaque.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

/// DISPLAY_TIMEOUT.
const SHOW_FOR: Duration = Duration::from_millis(600);
/// ANIMATION_TIME: the fade in and out.
pub const FADE_MS: f64 = 100.0;

/// The pill's opacity `elapsed_ms` into a fade from `from` to `to`.
pub fn fade_at(from: f64, to: f64, elapsed_ms: f64, enabled: bool) -> f64 {
    let p = if enabled {
        (elapsed_ms / FADE_MS).clamp(0.0, 1.0)
    } else {
        1.0
    };
    from + (to - from) * (1.0 - (1.0 - p).powi(2))
}

/// The fade is opacity, so it runs under fade-only motion too.
fn animations_enabled() -> bool {
    crate::motion::current().allows_fades()
}
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
    pill: gtk::Box,
    dots: gtk::DrawingArea,
    /// Bumped by every fade so a superseded one stops.
    fade: Cell<u64>,
    state: Rc<Cell<(u32, u32)>>,
    hide: RefCell<Option<glib::SourceId>>,
}

impl WorkspacePopup {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("tuna-workspace-popup");
        window.set_title(Some("Workspace Switcher"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::WORKSPACE_POPUP_NAMESPACE));
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
            pill,
            dots,
            fade: Cell::new(0),
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
        // GNOME fades in only when hidden; a repeat key keeps it opaque.
        if self.window.is_visible() {
            self.fade.set(self.fade.get().wrapping_add(1));
            self.pill.set_opacity(1.0);
        } else {
            self.pill.set_opacity(0.0);
            self.window.present();
            self.fade_to(1.0);
        }
        if let Some(id) = self.hide.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(SHOW_FOR, move || {
            if let Some(ui) = weak.upgrade() {
                ui.hide.borrow_mut().take();
                ui.fade_to(0.0);
            }
        });
        *self.hide.borrow_mut() = Some(id);
    }

    /// Ease the pill's opacity to `to`, hiding the window at 0.
    fn fade_to(self: &Rc<Self>, to: f64) {
        let generation = self.fade.get().wrapping_add(1);
        self.fade.set(generation);
        let from = self.pill.opacity();
        let finish = move |ui: &Self| {
            ui.pill.set_opacity(to);
            if to == 0.0 {
                ui.window.set_visible(false);
            }
        };
        if !animations_enabled() {
            finish(self);
            return;
        }
        let weak = Rc::downgrade(self);
        let started = Cell::new(None);
        self.pill.add_tick_callback(move |_, clock| {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if ui.fade.get() != generation {
                return glib::ControlFlow::Break;
            }
            let start = started.get().unwrap_or_else(|| {
                started.set(Some(clock.frame_time()));
                clock.frame_time()
            });
            // GNOME's slow-down factor stretches the fade.
            let elapsed =
                (clock.frame_time() - start) as f64 / 1000.0 / crate::motion::current().slowdown();
            let enabled = animations_enabled();
            if !enabled || elapsed >= FADE_MS {
                finish(&ui);
                return glib::ControlFlow::Break;
            }
            ui.pill.set_opacity(fade_at(from, to, elapsed, enabled));
            glib::ControlFlow::Continue
        });
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

    #[test]
    fn the_pill_fades_over_100_ms_ease_out_quad() {
        assert_eq!(fade_at(0.0, 1.0, 0.0, true), 0.0);
        assert_eq!(fade_at(0.0, 1.0, 50.0, true), 0.75);
        assert_eq!(fade_at(0.0, 1.0, 100.0, true), 1.0);
        assert_eq!(fade_at(1.0, 0.0, 50.0, true), 0.25);
        assert_eq!(fade_at(0.0, 1.0, 0.0, false), 1.0);
    }
}
