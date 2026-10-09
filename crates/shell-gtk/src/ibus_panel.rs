//! GNOME Shell's IBus candidate popup (`js/ui/ibusCandidatePopup.js`)
//! and the panel half of `js/misc/ibusManager.js`.
//!
//! GNOME Shell is IBus's *panel*: it starts `ibus-daemon --panel
//! disable` (so no `ibus-ui-gtk3` runs) and owns
//! `org.freedesktop.IBus.Panel` on IBus's own bus through
//! `IBus.PanelService`. The daemon then routes to that name what an
//! input context cannot show itself: `tuna-ibus-bridge`, like GNOME
//! Shell's input method, declares preedit but not lookup-table or
//! auxiliary-text capabilities, so the engine's candidates arrive here
//! (`UpdateLookupTable`, `ShowLookupTable`, `HideLookupTable`,
//! `UpdateAuxiliaryText`...) together with the focused context's
//! `SetCursorLocation` (global coordinates, from the bridge).
//!
//! The popup is GNOME's: `.candidate-popup-content` holding the
//! preedit and auxiliary texts and the candidate area, one
//! `.candidate-box` per candidate on the page (an index label and the
//! text), the cursor's row `:selected`, page buttons when there is
//! more than one page, vertical unless the table asks for horizontal.
//! It sits 6px (the boxpointer's `-arrow-rise`) under the cursor
//! rectangle. A click on a candidate is the panel's `CandidateClicked`
//! signal, a page button `PageUp`/`PageDown`, the wheel
//! `CursorUp`/`CursorDown`, as `IBus.PanelService` emits them.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
pub use tuna_shell_host::ibus::text as ibus_text;
use tuna_shell_host::ibus::{address as ibus_address, debug};

const PANEL_NAME: &str = "org.freedesktop.IBus.Panel";
const PANEL_PATH: &str = "/org/freedesktop/IBus/Panel";
/// IBUS_BUS_NAME_FLAG_ALLOW_REPLACEMENT | IBUS_BUS_NAME_FLAG_REPLACE_EXISTING,
/// as `IBus.PanelService` requests the name.
const NAME_FLAGS: u32 = 1 | 2;
/// ibusCandidatePopup.js MAX_CANDIDATES_PER_PAGE.
const MAX_CANDIDATES_PER_PAGE: usize = 16;
/// ibusCandidatePopup.js DEFAULT_INDEX_LABELS.
const DEFAULT_INDEX_LABELS: [&str; MAX_CANDIDATES_PER_PAGE] = [
    "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "a", "b", "c", "d", "e", "f",
];
/// IBUS_ORIENTATION_HORIZONTAL (VERTICAL and SYSTEM draw vertically).
const HORIZONTAL: i32 = 0;
/// `.popup-menu-boxpointer { -arrow-rise: $base_padding }`: the gap
/// between the cursor rectangle and the popup.
const ARROW_RISE: i32 = 6;
/// Transparent room around the content for its box-shadow (the
/// content's CSS margin in style.css).
const SHADOW: i32 = 6;
/// How often to look for IBus's bus while not connected to it.
const RETRY: Duration = Duration::from_secs(2);

/// org.freedesktop.IBus.Panel, as ibus's `ibuspanelservice.c` declares it.
const XML: &str = r#"<node>
  <interface name="org.freedesktop.IBus.Panel">
    <method name="UpdatePreeditText"><arg direction="in" type="v"/><arg direction="in" type="u"/><arg direction="in" type="b"/></method>
    <method name="ShowPreeditText"/>
    <method name="HidePreeditText"/>
    <method name="UpdateAuxiliaryText"><arg direction="in" type="v"/><arg direction="in" type="b"/></method>
    <method name="ShowAuxiliaryText"/>
    <method name="HideAuxiliaryText"/>
    <method name="UpdateLookupTable"><arg direction="in" type="v"/><arg direction="in" type="b"/></method>
    <method name="ShowLookupTable"/>
    <method name="HideLookupTable"/>
    <method name="CursorUpLookupTable"/>
    <method name="CursorDownLookupTable"/>
    <method name="PageUpLookupTable"/>
    <method name="PageDownLookupTable"/>
    <method name="CandidateClickedLookupTable"><arg direction="in" type="u"/><arg direction="in" type="u"/><arg direction="in" type="u"/></method>
    <method name="RegisterProperties"><arg direction="in" type="v"/></method>
    <method name="UpdateProperty"><arg direction="in" type="v"/></method>
    <method name="FocusIn"><arg direction="in" type="o"/></method>
    <method name="FocusOut"><arg direction="in" type="o"/></method>
    <method name="DestroyContext"><arg direction="in" type="o"/></method>
    <method name="SetCursorLocation"><arg direction="in" type="i"/><arg direction="in" type="i"/><arg direction="in" type="i"/><arg direction="in" type="i"/></method>
    <method name="SetCursorLocationRelative"><arg direction="in" type="i"/><arg direction="in" type="i"/><arg direction="in" type="i"/><arg direction="in" type="i"/></method>
    <method name="Reset"/>
    <method name="StartSetup"/>
    <method name="StateChanged"/>
    <method name="HideLanguageBar"/>
    <method name="ShowLanguageBar"/>
    <method name="ContentType"><arg direction="in" type="u"/><arg direction="in" type="u"/></method>
    <method name="PanelExtensionReceived"><arg direction="in" type="v"/></method>
    <method name="ProcessKeyEvent"><arg direction="in" type="u"/><arg direction="in" type="u"/><arg direction="in" type="u"/><arg direction="out" type="b"/></method>
    <method name="CommitTextReceived"><arg direction="in" type="v"/></method>
    <signal name="CursorUp"/>
    <signal name="CursorDown"/>
    <signal name="PageUp"/>
    <signal name="PageDown"/>
    <signal name="PropertyActivate"><arg type="s"/><arg type="i"/></signal>
    <signal name="PropertyShow"><arg type="s"/></signal>
    <signal name="PropertyHide"><arg type="s"/></signal>
    <signal name="CandidateClicked"><arg type="u"/><arg type="u"/><arg type="u"/></signal>
  </interface>
</node>"#;

/// An IBusLookupTable as the panel receives it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LookupTable {
    pub page_size: u32,
    pub cursor_pos: u32,
    pub cursor_visible: bool,
    pub round: bool,
    pub orientation: i32,
    /// Every candidate, not just the page's.
    pub candidates: Vec<String>,
    /// Index labels for the page's rows (often none).
    pub labels: Vec<String>,
}

/// A serialized IBusLookupTable: `v` holding `("IBusLookupTable",
/// a{sv}, u page_size, u cursor_pos, b cursor_visible, b round,
/// i orientation, av candidates, av labels)` (ibuslookuptable.c).
pub fn lookup_table(value: &glib::Variant) -> Option<LookupTable> {
    let mut table = value.clone();
    while table.is_type(glib::VariantTy::VARIANT) {
        table = table.as_variant()?;
    }
    if table.try_child_value(0)?.str()? != "IBusLookupTable" {
        return None;
    }
    let texts = |list: glib::Variant| -> Vec<String> {
        (0..list.n_children())
            .map(|i| ibus_text(&list.child_value(i)).unwrap_or_default())
            .collect()
    };
    Some(LookupTable {
        page_size: table.try_child_value(2)?.get()?,
        cursor_pos: table.try_child_value(3)?.get()?,
        cursor_visible: table.try_child_value(4)?.get()?,
        round: table.try_child_value(5)?.get()?,
        orientation: table.try_child_value(6)?.get()?,
        candidates: texts(table.try_child_value(7)?),
        labels: texts(table.try_child_value(8)?),
    })
}

/// What the candidate area shows of a table: its current page.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub indexes: Vec<String>,
    pub candidates: Vec<String>,
    /// The highlighted row on this page.
    pub cursor: Option<usize>,
    pub vertical: bool,
    /// Previous and next enabled, when there is more than one page.
    pub buttons: Option<(bool, bool)>,
}

impl LookupTable {
    /// ibusCandidatePopup.js's update-lookup-table handler.
    pub fn page(&self) -> Page {
        let n = self.candidates.len();
        let page_size = (self.page_size as usize).max(1);
        let pages = n.div_ceil(page_size);
        let page = self.cursor_pos as usize / page_size;
        let start = (page * page_size).min(n);
        let end = ((page + 1) * page_size).min(n);
        let candidates: Vec<String> = self.candidates[start..end]
            .iter()
            .take(MAX_CANDIDATES_PER_PAGE)
            .cloned()
            .collect();
        let indexes = (0..candidates.len())
            .map(|i| {
                self.labels
                    .get(i)
                    .filter(|l| !l.is_empty())
                    .cloned()
                    .unwrap_or_else(|| DEFAULT_INDEX_LABELS[i].to_owned())
            })
            .collect();
        let row = self.cursor_pos as usize % page_size;
        Page {
            indexes,
            cursor: (self.cursor_visible && row < candidates.len()).then_some(row),
            candidates,
            vertical: self.orientation != HORIZONTAL,
            buttons: (pages >= 2)
                .then_some((self.round || page > 0, self.round || page + 1 < pages)),
        }
    }
}

/// Where the content's top-left goes, in the monitor's coordinates:
/// `ARROW_RISE` under the cursor rectangle, left edges aligned (the
/// boxpointer on St.Side.TOP, alignment 0), flipped above the cursor
/// when it would leave the monitor's bottom and kept on the monitor.
pub fn place(
    cursor: (i32, i32, i32, i32),
    monitor: (i32, i32, i32, i32),
    size: (i32, i32),
) -> (i32, i32) {
    let (cx, cy, _, ch) = cursor;
    let (mx, my, mw, mh) = monitor;
    let (w, h) = size;
    let x = (cx - mx).min(mw - w).max(0);
    let below = cy - my + ch + ARROW_RISE;
    let y = if below + h <= mh {
        below
    } else {
        (cy - my - ARROW_RISE - h).max(0)
    };
    (x, y)
}

fn ibus_installed() -> bool {
    std::env::var_os("IBUS_ADDRESS").is_some()
        || std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("ibus").is_file()))
}

struct Row {
    row: gtk::Box,
    index: gtk::Label,
    label: gtk::Label,
}

#[derive(Default)]
struct State {
    cursor: Option<(i32, i32, i32, i32)>,
    preedit_visible: bool,
    aux_visible: bool,
    table_visible: bool,
    /// The window's top-left in global coordinates, once placed.
    origin: (i32, i32),
    /// The monitor the window is on (its geometry).
    monitor: Option<(i32, i32, i32, i32)>,
}

pub struct CandidatePopup {
    window: gtk::Window,
    content: gtk::Box,
    preedit: gtk::Label,
    aux: gtk::Label,
    area: gtk::Box,
    rows: Vec<Row>,
    buttons: gtk::Box,
    previous: gtk::Button,
    next: gtk::Button,
    state: RefCell<State>,
    conn: RefCell<Option<gio::DBusConnection>>,
    connecting: Cell<bool>,
    log_pending: Cell<bool>,
}

impl CandidatePopup {
    fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("tuna-ibus-candidates");
        window.set_title(Some("Input Method Candidates"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::IBUS_CANDIDATES_NAMESPACE));
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Left, true);
        window.set_exclusive_zone(-1);
        // Never the keyboard: the text field keeps it (and with it the
        // input method) while a candidate is clicked.
        window.set_keyboard_mode(KeyboardMode::None);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.add_css_class("candidate-popup-content");
        let preedit = gtk::Label::new(None);
        preedit.add_css_class("candidate-popup-text");
        preedit.set_xalign(0.0);
        preedit.set_visible(false);
        content.append(&preedit);
        let aux = gtk::Label::new(None);
        aux.add_css_class("candidate-popup-text");
        aux.set_xalign(0.0);
        aux.set_visible(false);
        content.append(&aux);

        let area = gtk::Box::new(gtk::Orientation::Vertical, 0);
        area.add_css_class("candidate-area");
        area.add_css_class("vertical");
        let rows: Vec<Row> = (0..MAX_CANDIDATES_PER_PAGE)
            .map(|_| {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                row.add_css_class("candidate-box");
                let index = gtk::Label::new(None);
                index.add_css_class("candidate-index");
                let label = gtk::Label::new(None);
                label.add_css_class("candidate-label");
                label.set_xalign(0.0);
                row.append(&index);
                row.append(&label);
                row.set_visible(false);
                area.append(&row);
                Row { row, index, label }
            })
            .collect();
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        buttons.add_css_class("candidate-page-button-box");
        let previous = gtk::Button::from_icon_name("go-up-symbolic");
        previous.add_css_class("candidate-page-button");
        previous.add_css_class("candidate-page-button-previous");
        previous.set_hexpand(true);
        let next = gtk::Button::from_icon_name("go-down-symbolic");
        next.add_css_class("candidate-page-button");
        next.add_css_class("candidate-page-button-next");
        next.set_hexpand(true);
        buttons.append(&previous);
        buttons.append(&next);
        buttons.set_visible(false);
        area.append(&buttons);
        area.set_visible(false);
        content.append(&area);
        window.set_child(Some(&content));

        let ui = Rc::new(Self {
            window,
            content,
            preedit,
            aux,
            area,
            rows,
            buttons,
            previous,
            next,
            state: RefCell::default(),
            conn: RefCell::new(None),
            connecting: Cell::new(false),
            log_pending: Cell::new(false),
        });
        ui.wire();
        ui
    }

    /// Clicks, page buttons and the wheel become the panel's signals.
    fn wire(self: &Rc<Self>) {
        for (i, row) in self.rows.iter().enumerate() {
            let click = gtk::GestureClick::new();
            click.set_button(0);
            let weak = Rc::downgrade(self);
            click.connect_released(move |gesture, _, _, _| {
                if let Some(ui) = weak.upgrade() {
                    let button = gesture.current_button();
                    let state = gesture.current_event_state().bits();
                    if debug() {
                        eprintln!("tuna-shell-gtk: ibus candidate {i} clicked (button {button})");
                    }
                    ui.emit(
                        "CandidateClicked",
                        Some((i as u32, button, state).to_variant()),
                    );
                }
            });
            row.row.add_controller(click);
        }
        let weak = Rc::downgrade(self);
        self.previous.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.emit("PageUp", None);
            }
        });
        let weak = Rc::downgrade(self);
        self.next.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.emit("PageDown", None);
            }
        });
        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE,
        );
        let weak = Rc::downgrade(self);
        scroll.connect_scroll(move |_, _, dy| {
            if let Some(ui) = weak.upgrade() {
                if dy < 0.0 {
                    ui.emit("CursorUp", None);
                } else if dy > 0.0 {
                    ui.emit("CursorDown", None);
                }
            }
            glib::Propagation::Stop
        });
        self.area.add_controller(scroll);
    }

    /// A signal on the panel object, as `IBus.PanelService` emits it;
    /// the daemon passes it to the focused context's engine.
    fn emit(&self, signal: &str, args: Option<glib::Variant>) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        if let Err(e) = conn.emit_signal(None, PANEL_PATH, PANEL_NAME, signal, args.as_ref()) {
            eprintln!("tuna-shell-gtk: IBus panel: {signal}: {e}");
        }
    }

    /// One call from the daemon.
    fn method(self: &Rc<Self>, method: &str, params: &glib::Variant) {
        {
            let mut state = self.state.borrow_mut();
            match method {
                "UpdatePreeditText" => {
                    self.preedit
                        .set_label(&ibus_text(&params.child_value(0)).unwrap_or_default());
                    state.preedit_visible = params.child_value(2).get::<bool>().unwrap_or(false);
                }
                "ShowPreeditText" => state.preedit_visible = true,
                "HidePreeditText" => state.preedit_visible = false,
                "UpdateAuxiliaryText" => {
                    self.aux
                        .set_label(&ibus_text(&params.child_value(0)).unwrap_or_default());
                    state.aux_visible = params.child_value(1).get::<bool>().unwrap_or(false);
                }
                "ShowAuxiliaryText" => state.aux_visible = true,
                "HideAuxiliaryText" => state.aux_visible = false,
                "UpdateLookupTable" => {
                    if let Some(table) = lookup_table(&params.child_value(0)) {
                        self.set_table(&table.page());
                    }
                    state.table_visible = params.child_value(1).get::<bool>().unwrap_or(false);
                }
                "ShowLookupTable" => state.table_visible = true,
                "HideLookupTable" => state.table_visible = false,
                "SetCursorLocation" => {
                    state.cursor = params.get::<(i32, i32, i32, i32)>();
                }
                // GNOME closes the popup when the context loses focus.
                "FocusOut" => {
                    state.preedit_visible = false;
                    state.aux_visible = false;
                    state.table_visible = false;
                }
                _ => return,
            }
        }
        self.update_visibility();
    }

    /// CandidateArea.setCandidates, setOrientation and updateButtons.
    fn set_table(&self, page: &Page) {
        for (i, row) in self.rows.iter().enumerate() {
            let shown = i < page.candidates.len();
            row.row.set_visible(shown);
            if shown {
                row.index.set_label(&page.indexes[i]);
                row.label.set_label(&page.candidates[i]);
            }
            if page.cursor == Some(i) {
                row.row.set_state_flags(gtk::StateFlags::SELECTED, false);
            } else {
                row.row.unset_state_flags(gtk::StateFlags::SELECTED);
            }
        }
        if page.vertical {
            self.area.set_orientation(gtk::Orientation::Vertical);
            self.area.remove_css_class("horizontal");
            self.area.add_css_class("vertical");
            self.previous.set_icon_name("go-up-symbolic");
            self.next.set_icon_name("go-down-symbolic");
        } else {
            self.area.set_orientation(gtk::Orientation::Horizontal);
            self.area.remove_css_class("vertical");
            self.area.add_css_class("horizontal");
            self.previous.set_icon_name("go-previous-symbolic");
            self.next.set_icon_name("go-next-symbolic");
        }
        self.buttons.set_visible(page.buttons.is_some());
        if let Some((previous, next)) = page.buttons {
            self.previous.set_sensitive(previous);
            self.next.set_sensitive(next);
        }
    }

    /// _updateVisibility: shown while any of its parts is, under the
    /// cursor.
    fn update_visibility(self: &Rc<Self>) {
        let (visible, cursor) = {
            let state = self.state.borrow();
            self.preedit.set_visible(state.preedit_visible);
            self.aux.set_visible(state.aux_visible);
            self.area.set_visible(state.table_visible);
            (
                state.preedit_visible || state.aux_visible || state.table_visible,
                state.cursor.unwrap_or_default(),
            )
        };
        if !visible {
            self.window.set_visible(false);
            return;
        }
        self.reposition(cursor);
        // Shrink to the new content: a layer surface keeps its last
        // size unless asked for its natural one.
        self.window.set_default_size(1, 1);
        self.window.present();
        if debug() && !self.log_pending.replace(true) {
            let weak = Rc::downgrade(self);
            self.window.add_tick_callback(move |window, _| {
                let Some(ui) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if ui.log_rows(window) {
                    ui.log_pending.set(false);
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
        }
    }

    /// Put the window on the monitor under the cursor, its content at
    /// [`place`].
    fn reposition(&self, cursor: (i32, i32, i32, i32)) {
        let display = WidgetExt::display(&self.window);
        let monitors: Vec<gdk::Monitor> = (0..display.monitors().n_items())
            .filter_map(|i| display.monitors().item(i)?.downcast::<gdk::Monitor>().ok())
            .collect();
        let contains = |m: &&gdk::Monitor| {
            let g = m.geometry();
            cursor.0 >= g.x()
                && cursor.0 < g.x() + g.width()
                && cursor.1 >= g.y()
                && cursor.1 < g.y() + g.height()
        };
        let monitor = monitors.iter().find(contains).or(monitors.first());
        let geometry = monitor
            .map(|m| {
                let g = m.geometry();
                (g.x(), g.y(), g.width(), g.height())
            })
            .unwrap_or((0, 0, 1280, 800));
        let (_, natural) = self.content.preferred_size();
        // The natural size includes the shadow's margin.
        let size = (natural.width() - 2 * SHADOW, natural.height() - 2 * SHADOW);
        let (x, y) = place(cursor, geometry, size);
        let (left, top) = ((x - SHADOW).max(0), (y - SHADOW).max(0));
        let mut state = self.state.borrow_mut();
        if state.monitor != Some(geometry) {
            if let Some(monitor) = monitor {
                self.window.set_monitor(Some(monitor));
            }
            state.monitor = Some(geometry);
        }
        self.window.set_margin(Edge::Left, left);
        self.window.set_margin(Edge::Top, top);
        state.origin = (geometry.0 + left, geometry.1 + top);
    }

    /// Log each shown candidate's rectangle in global coordinates (for
    /// the proof's click), once laid out: whether it was.
    fn log_rows(&self, window: &gtk::Window) -> bool {
        if !window.is_mapped() {
            return !window.is_visible();
        }
        let (ox, oy) = self.state.borrow().origin;
        let (sx, sy) = window.surface_transform();
        let mut lines = Vec::new();
        let vertical = self.area.orientation() == gtk::Orientation::Vertical;
        let mut previous_end = None;
        for (i, row) in self.rows.iter().enumerate() {
            if !row.row.is_visible() || !self.area.is_visible() {
                continue;
            }
            let Some(b) = row.row.compute_bounds(window) else {
                return false;
            };
            let (minimum, _) = row.row.preferred_size();
            // Tick callbacks run before layout. Positive old allocations
            // can put every newly shown row at the same origin; do not
            // publish those as coordinates for pixel checks or clicks.
            if b.width() < minimum.width() as f32 || b.height() < minimum.height() as f32 {
                return false;
            }
            let (start, length) = if vertical {
                (b.y(), b.height())
            } else {
                (b.x(), b.width())
            };
            if previous_end.is_some_and(|end| start < end) {
                return false;
            }
            previous_end = Some(start + length);
            lines.push(format!(
                "tuna-shell-gtk: ibus candidate {i} {} at {} {} {} {}",
                row.label.label(),
                ox + (sx + f64::from(b.x())).round() as i32,
                oy + (sy + f64::from(b.y())).round() as i32,
                b.width().round() as i32,
                b.height().round() as i32,
            ));
        }
        for line in lines {
            eprintln!("{line}");
        }
        true
    }

    /// Hide everything (the bus went away).
    fn reset(&self) {
        *self.state.borrow_mut() = State::default();
        self.window.set_visible(false);
    }

    /// Connect to IBus's bus when not connected; notice when it closes.
    fn poll(self: &Rc<Self>) {
        let current = self.conn.borrow().clone();
        if let Some(conn) = current {
            if !conn.is_closed() {
                return;
            }
            eprintln!("tuna-shell-gtk: IBus panel: the IBus bus closed");
            self.conn.replace(None);
            self.reset();
        }
        if self.connecting.get() {
            return;
        }
        let Some(address) = ibus_address() else {
            return;
        };
        self.connecting.set(true);
        let weak = Rc::downgrade(self);
        gio::DBusConnection::for_address(
            &address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            gio::Cancellable::NONE,
            move |result| {
                let Some(ui) = weak.upgrade() else { return };
                ui.connecting.set(false);
                // A stale address (the daemon gone): try again later.
                if let Ok(conn) = result {
                    ui.serve(conn);
                }
            },
        );
    }

    /// Serve org.freedesktop.IBus.Panel on IBus's bus and take its name.
    fn serve(self: &Rc<Self>, conn: gio::DBusConnection) {
        let info = match gio::DBusNodeInfo::for_xml(XML) {
            Ok(node) => node.lookup_interface(PANEL_NAME),
            Err(e) => {
                eprintln!("tuna-shell-gtk: IBus panel interface: {e}");
                return;
            }
        };
        let Some(info) = info else { return };
        let weak = Rc::downgrade(self);
        let registered = conn
            .register_object(PANEL_PATH, &info)
            .method_call(move |_, _, _, _, method, params, invocation| {
                if method == "ProcessKeyEvent" {
                    // The emoji extension's keys; GNOME's panel takes none.
                    invocation.return_value(Some(&(false,).to_variant()));
                } else {
                    invocation.return_value(None);
                }
                if let Some(ui) = weak.upgrade() {
                    ui.method(method, &params);
                }
            })
            .build();
        if let Err(e) = registered {
            eprintln!("tuna-shell-gtk: IBus panel object: {e}");
            return;
        }
        conn.call(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&(PANEL_NAME, NAME_FLAGS).to_variant()),
            glib::VariantTy::new("(u)").ok(),
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            |reply| match reply.map(|r| r.child_value(0).get::<u32>()) {
                // DBUS_REQUEST_NAME_REPLY_PRIMARY_OWNER / ALREADY_OWNER.
                Ok(Some(1 | 4)) => {
                    eprintln!("tuna-shell-gtk: IBus panel: owns {PANEL_NAME} on the IBus bus");
                }
                Ok(code) => {
                    eprintln!("tuna-shell-gtk: IBus panel: {PANEL_NAME} not owned ({code:?})")
                }
                Err(e) => eprintln!("tuna-shell-gtk: IBus panel: RequestName: {e}"),
            },
        );
        self.conn.replace(Some(conn));
    }
}

/// Become IBus's panel when IBus is installed: look for its bus every
/// few seconds (tuna-ibus-bridge starts the daemon), and again after
/// it goes away. The poll keeps the popup for the shell's life.
pub fn start(app: &gtk::Application) {
    if !ibus_installed() {
        return;
    }
    let ui = CandidatePopup::new(app);
    ui.poll();
    glib::timeout_add_local(RETRY, move || {
        ui.poll();
        glib::ControlFlow::Continue
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_host::ibus::text_variant as text;

    fn texts(items: &[&str]) -> glib::Variant {
        glib::Variant::array_from_iter_with_type(
            glib::VariantTy::VARIANT,
            items.iter().copied().map(text),
        )
    }

    /// A table as libibus serializes one.
    fn serialized(
        page_size: u32,
        cursor: u32,
        orientation: i32,
        items: &[&str],
        labels: &[&str],
    ) -> glib::Variant {
        glib::Variant::from_variant(&glib::Variant::tuple_from_iter([
            "IBusLookupTable".to_variant(),
            glib::VariantDict::new(None).end(),
            page_size.to_variant(),
            cursor.to_variant(),
            true.to_variant(),
            false.to_variant(),
            orientation.to_variant(),
            texts(items),
            texts(labels),
        ]))
    }

    #[test]
    fn a_serialized_lookup_table_is_read_like_libibus() {
        let v = serialized(5, 1, 2, &["你", "尼", "泥"], &[]);
        assert_eq!(
            v.as_variant().unwrap().type_().as_str(),
            "(sa{sv}uubbiavav)"
        );
        let table = lookup_table(&v).unwrap();
        assert_eq!(table.page_size, 5);
        assert_eq!(table.cursor_pos, 1);
        assert!(table.cursor_visible);
        assert!(!table.round);
        assert_eq!(table.orientation, 2);
        assert_eq!(table.candidates, ["你", "尼", "泥"]);
        assert!(table.labels.is_empty());
        // Not a table.
        assert_eq!(lookup_table(&text("ni")), None);
    }

    #[test]
    fn raw_and_nested_lookup_variants_decode_without_native_type_assertions() {
        let boxed = serialized(5, 1, 2, &["你", "尼"], &[]);
        let expected = lookup_table(&boxed);
        assert_eq!(lookup_table(&boxed.as_variant().unwrap()), expected);
        assert_eq!(lookup_table(&glib::Variant::from_variant(&boxed)), expected);
        for value in ["not a container".to_variant(), 42u32.to_variant()] {
            assert_eq!(lookup_table(&value), None);
        }
    }

    #[test]
    fn the_page_holds_the_cursor_with_gnome_index_labels() {
        let table = lookup_table(&serialized(5, 1, 2, &["你", "尼", "泥"], &[])).unwrap();
        let page = table.page();
        assert_eq!(page.candidates, ["你", "尼", "泥"]);
        assert_eq!(page.indexes, ["1", "2", "3"]);
        assert_eq!(page.cursor, Some(1));
        // SYSTEM draws vertically, as in GNOME.
        assert!(page.vertical);
        assert_eq!(page.buttons, None);
    }

    #[test]
    fn later_pages_and_labels_and_horizontal_tables() {
        let items = ["a", "b", "c", "d", "e", "f", "g"];
        let table = LookupTable {
            page_size: 3,
            cursor_pos: 4,
            cursor_visible: true,
            round: false,
            orientation: HORIZONTAL,
            candidates: items.iter().map(|s| s.to_string()).collect(),
            labels: vec!["x".into(), String::new()],
        };
        let page = table.page();
        assert_eq!(page.candidates, ["d", "e", "f"]);
        assert_eq!(page.indexes, ["x", "2", "3"]);
        assert_eq!(page.cursor, Some(1));
        assert!(!page.vertical);
        assert_eq!(page.buttons, Some((true, true)));
        // The last page: no next unless the table wraps around.
        let last = LookupTable {
            cursor_pos: 6,
            ..table.clone()
        }
        .page();
        assert_eq!(last.candidates, ["g"]);
        assert_eq!(last.buttons, Some((true, false)));
        let round = LookupTable {
            cursor_pos: 0,
            round: true,
            ..table.clone()
        }
        .page();
        assert_eq!(round.buttons, Some((true, true)));
        // A hidden cursor highlights nothing.
        let hidden = LookupTable {
            cursor_visible: false,
            ..table
        }
        .page();
        assert_eq!(hidden.cursor, None);
    }

    #[test]
    fn the_popup_sits_under_the_cursor_and_stays_on_the_monitor() {
        let monitor = (0, 0, 1280, 800);
        // 6px under a 20px-tall cursor, left edges aligned.
        assert_eq!(place((100, 200, 2, 20), monitor, (120, 110)), (100, 226));
        // Near the right edge: pulled back onto the monitor.
        assert_eq!(place((1250, 200, 2, 20), monitor, (120, 110)), (1160, 226));
        // Near the bottom: flipped above the cursor.
        assert_eq!(place((100, 750, 2, 20), monitor, (120, 110)), (100, 634));
        // A second monitor's coordinates are its own.
        assert_eq!(
            place((1380, 200, 2, 20), (1280, 0, 1280, 800), (120, 110)),
            (100, 226)
        );
    }
}
