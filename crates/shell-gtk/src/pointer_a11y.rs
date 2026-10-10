//! GNOME's pointer accessibility in the shell (#350): the
//! `org.gnome.desktop.a11y.mouse` settings the compositor's pointer aids
//! follow, the hover-click chooser in the panel (GNOME Shell's
//! dwellClick.js) and the pie timer drawn at the pointer while a hover
//! or simulated secondary click counts down (pointerA11yTimeout.js).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tuna_shell_control::{DwellClick, DwellDirection, PointerAids, PointerTimeoutKind};

pub const SCHEMA: &str = "org.gnome.desktop.a11y.mouse";

/// GNOME's pie timer is 60 px across.
const PIE_SIZE: i32 = 60;
/// Its success zoom-out.
const PIE_DONE_MS: u64 = 150;

fn direction(name: &str) -> Option<DwellDirection> {
    match name {
        "left" => Some(DwellDirection::Left),
        "right" => Some(DwellDirection::Right),
        "up" => Some(DwellDirection::Up),
        "down" => Some(DwellDirection::Down),
        _ => None,
    }
}

fn seconds_ms(seconds: f64) -> u32 {
    (seconds * 1000.0).round().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// GNOME's pointer accessibility settings, flattened for the compositor.
/// Without the schema both aids stay off (GNOME's defaults).
pub fn read(settings: Option<&gio::Settings>) -> PointerAids {
    let Some(s) = settings else {
        return PointerAids::default();
    };
    PointerAids {
        secondary_click: s.boolean("secondary-click-enabled"),
        secondary_click_ms: seconds_ms(s.double("secondary-click-time")),
        dwell_click: s.boolean("dwell-click-enabled"),
        dwell_ms: seconds_ms(s.double("dwell-time")),
        dwell_threshold: s.int("dwell-threshold").max(0) as u32,
        dwell_gesture: s.string("dwell-mode") == "gesture",
        gesture_single: direction(&s.string("dwell-gesture-single")),
        gesture_double: direction(&s.string("dwell-gesture-double")),
        gesture_drag: direction(&s.string("dwell-gesture-drag")),
        gesture_secondary: direction(&s.string("dwell-gesture-secondary")),
    }
}

/// GNOME's chooser entries: label and icon, in its menu order.
pub const CLICK_MODES: [(DwellClick, &str, &str); 4] = [
    (
        DwellClick::Primary,
        "Single Click",
        "pointer-primary-click-symbolic",
    ),
    (
        DwellClick::Double,
        "Double Click",
        "pointer-double-click-symbolic",
    ),
    (DwellClick::Drag, "Drag", "pointer-drag-symbolic"),
    (
        DwellClick::Secondary,
        "Secondary Click",
        "pointer-secondary-click-symbolic",
    ),
];

pub fn icon_of(click: DwellClick) -> &'static str {
    CLICK_MODES
        .iter()
        .find(|(c, _, _)| *c == click)
        .map(|(_, _, icon)| *icon)
        .unwrap_or("pointer-primary-click-symbolic")
}

/// The chooser shows only while hover click is on in `window` mode.
pub fn chooser_visible(enabled: bool, mode: &str) -> bool {
    enabled && mode == "window"
}

/// The panel's hover-click chooser.
pub struct DwellChooser {
    pub button: gtk::MenuButton,
    icon: gtk::Image,
    _settings: Option<gio::Settings>,
}

impl DwellChooser {
    /// `pick` sends the chosen click to the compositor.
    pub fn new(pick: Rc<dyn Fn(DwellClick)>) -> Rc<Self> {
        let icon = gtk::Image::from_icon_name(icon_of(DwellClick::Primary));
        icon.add_css_class("system-status-icon");
        let popover = gtk::Popover::new();
        popover.add_css_class("panel-menu");
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let button = gtk::MenuButton::builder()
            .child(&icon)
            .popover(&popover)
            .always_show_arrow(false)
            .build();
        button.add_css_class("panel-button");
        button.update_property(&[gtk::accessible::Property::Label("Dwell Click")]);
        for (click, label, icon_name) in CLICK_MODES {
            let row = gtk::Button::new();
            row.add_css_class("flat");
            row.add_css_class("popup-menu-item");
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            content.append(&gtk::Image::from_icon_name(icon_name));
            content.append(&gtk::Label::new(Some(label)));
            row.set_child(Some(&content));
            row.update_property(&[gtk::accessible::Property::Label(label)]);
            let (pick, popover, icon) = (pick.clone(), popover.clone(), icon.clone());
            row.connect_clicked(move |_| {
                pick(click);
                icon.set_icon_name(Some(icon_of(click)));
                popover.popdown();
            });
            list.append(&row);
        }
        popover.set_child(Some(&list));
        let settings = crate::settings(SCHEMA);
        let sync = {
            let button = button.clone();
            move |s: &gio::Settings| {
                button.set_visible(chooser_visible(
                    s.boolean("dwell-click-enabled"),
                    &s.string("dwell-mode"),
                ));
            }
        };
        match settings.as_ref() {
            Some(s) => {
                sync(s);
                s.connect_changed(None, move |s, _| sync(s));
            }
            None => button.set_visible(false),
        }
        Rc::new(Self {
            button,
            icon,
            _settings: settings,
        })
    }

    /// The compositor reports the click the next dwell makes.
    pub fn set_click_type(&self, click: DwellClick) {
        self.icon.set_icon_name(Some(icon_of(click)));
    }
}

/// GNOME's pie timer: a 60 px pie filling clockwise from the top over
/// the timeout, around the point it started at.
pub struct PieTimer {
    window: gtk::Window,
    area: gtk::DrawingArea,
    /// Start, duration, and the success zoom when it ran out.
    run: Rc<Cell<Option<(Instant, Duration)>>>,
    done: Rc<Cell<Option<Instant>>>,
    tick: RefCell<Option<glib::SourceId>>,
}

/// The pie's angle (0 to a full turn) `elapsed` into `duration`.
pub fn pie_angle(elapsed: Duration, duration: Duration) -> f64 {
    if duration.is_zero() {
        return std::f64::consts::TAU;
    }
    (elapsed.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0) * std::f64::consts::TAU
}

impl PieTimer {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.set_title(Some("Pointer Accessibility"));
        window.add_css_class("tuna-pie-timer");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::POINTER_A11Y_NAMESPACE));
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Left, true);
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::None);
        // Never in the pointer's way: it sits right under it.
        window.connect_realize(|w| {
            if let Some(surface) = w.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
            }
        });
        let area = gtk::DrawingArea::new();
        // Twice the pie, so the success zoom has room to grow.
        area.set_content_width(2 * PIE_SIZE);
        area.set_content_height(2 * PIE_SIZE);
        window.set_child(Some(&area));
        let run: Rc<Cell<Option<(Instant, Duration)>>> = Rc::default();
        let done: Rc<Cell<Option<Instant>>> = Rc::default();
        {
            let (run, done) = (run.clone(), done.clone());
            area.set_draw_func(move |_, cr, w, h| {
                let Some((start, duration)) = run.get() else {
                    return;
                };
                let angle = pie_angle(start.elapsed(), duration);
                // The success zoom: grow to twice the size, fading out.
                let (scale, alpha) = match done.get() {
                    Some(at) => {
                        let t = (at.elapsed().as_millis() as f64 / PIE_DONE_MS as f64).min(1.0);
                        let eased = 1.0 - (1.0 - t) * (1.0 - t);
                        (1.0 + eased, 1.0 - eased)
                    }
                    // GNOME fades in over the first quarter.
                    None => {
                        let quarter = duration.as_secs_f64() / 4.0;
                        let t = if quarter > 0.0 {
                            (start.elapsed().as_secs_f64() / quarter).min(1.0)
                        } else {
                            1.0
                        };
                        (1.0, t * t)
                    }
                };
                let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
                let border = 3.0;
                let radius = (f64::from(PIE_SIZE) / 2.0 - border) * scale;
                let start_angle = 3.0 * std::f64::consts::FRAC_PI_2;
                cr.translate(cx, cy);
                if angle < std::f64::consts::TAU {
                    cr.move_to(0.0, 0.0);
                }
                cr.arc(0.0, 0.0, radius, start_angle, start_angle + angle);
                if angle < std::f64::consts::TAU {
                    cr.line_to(0.0, 0.0);
                }
                cr.close_path();
                // GNOME's default accent: a light fill, an accent border.
                cr.set_source_rgba(0.208, 0.518, 0.894, 0.3 * alpha);
                let _ = cr.fill_preserve();
                cr.set_line_width(border);
                cr.set_source_rgba(0.208, 0.518, 0.894, alpha);
                let _ = cr.stroke();
            });
        }
        Rc::new(Self {
            window,
            area,
            run,
            done,
            tick: RefCell::default(),
        })
    }

    /// A timeout started at `(x, y)`.
    pub fn start(self: &Rc<Self>, x: i32, y: i32, duration_ms: u32) {
        self.stop_tick();
        // Centered on the point (the surface is twice the pie).
        self.window.set_margin(Edge::Left, (x - PIE_SIZE).max(0));
        self.window.set_margin(Edge::Top, (y - PIE_SIZE).max(0));
        self.run.set(Some((
            Instant::now(),
            Duration::from_millis(u64::from(duration_ms)),
        )));
        self.done.set(None);
        self.window.present();
        let me = Rc::downgrade(self);
        let id = glib::timeout_add_local(Duration::from_millis(16), move || {
            let Some(me) = me.upgrade() else {
                return glib::ControlFlow::Break;
            };
            me.area.queue_draw();
            if me
                .done
                .get()
                .is_some_and(|at| at.elapsed() >= Duration::from_millis(PIE_DONE_MS))
            {
                me.tick.borrow_mut().take();
                me.hide();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.tick.borrow_mut() = Some(id);
    }

    /// The timeout ended: zoom out when it acted, vanish when cancelled.
    pub fn stop(&self, clicked: bool) {
        if self.run.get().is_none() {
            return;
        }
        if clicked && self.tick.borrow().is_some() {
            self.done.set(Some(Instant::now()));
        } else {
            self.stop_tick();
            self.hide();
        }
    }

    fn hide(&self) {
        self.run.set(None);
        self.done.set(None);
        self.window.set_visible(false);
    }

    fn stop_tick(&self) {
        if let Some(id) = self.tick.borrow_mut().take() {
            id.remove();
        }
    }
}

/// What the shell does with a pointer timeout report.
pub fn on_timeout(
    pie: &Rc<PieTimer>,
    _kind: PointerTimeoutKind,
    duration_ms: Option<u32>,
    clicked: bool,
    x: i32,
    y: i32,
) {
    match duration_ms {
        Some(ms) => pie.start(x, y, ms),
        None => pie.stop(clicked),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooser_matches_gnome() {
        let labels: Vec<&str> = CLICK_MODES.iter().map(|(_, l, _)| *l).collect();
        assert_eq!(
            labels,
            ["Single Click", "Double Click", "Drag", "Secondary Click"]
        );
        assert_eq!(icon_of(DwellClick::Drag), "pointer-drag-symbolic");
        assert!(chooser_visible(true, "window"));
        assert!(!chooser_visible(true, "gesture"));
        assert!(!chooser_visible(false, "window"));
    }

    #[test]
    fn settings_convert_like_gnome() {
        assert_eq!(seconds_ms(1.2), 1200);
        assert_eq!(seconds_ms(-1.0), 0);
        assert_eq!(direction("up"), Some(DwellDirection::Up));
        assert_eq!(direction("none"), None);
        assert_eq!(read(None), PointerAids::default());
    }

    #[test]
    fn pie_fills_a_turn_over_the_timeout() {
        let d = Duration::from_millis(1000);
        assert_eq!(pie_angle(Duration::ZERO, d), 0.0);
        assert!((pie_angle(Duration::from_millis(500), d) - std::f64::consts::PI).abs() < 1e-9);
        assert_eq!(
            pie_angle(Duration::from_millis(1500), d),
            std::f64::consts::TAU
        );
        assert_eq!(
            pie_angle(Duration::ZERO, Duration::ZERO),
            std::f64::consts::TAU
        );
    }
}
