//! roost-shell-gtk: the Roost shell drawn with GTK4 and libadwaita
//! (ADR 0006, #53).
//!
//! The first surface is the GNOME 51 top panel as a layer-shell strip:
//! the workspace pill on the left (Activities), the date and time in the
//! center opening the calendar and notification list, and the system
//! indicators on the right opening the quick settings grid. Everything
//! the user sees is a real GTK widget, so text uses the system interface
//! font and every control is reachable through AT-SPI for screen
//! readers. Compositor state arrives over the same versioned control
//! socket the current shell uses (`ROOST_CONTROL_SOCKET`).
//!
//! The compositor runs one supervised shell; select this one with
//! `ROOST_SHELL_BIN=roost-shell-gtk` while it grows to parity.

mod bt_menu;
mod calendar;
mod events;
mod folder_dialog;
mod folders;
mod group_animation;
mod ibus_panel;
mod keybindings;
mod live_apps;
mod lock;
mod logic;
mod network_agent;
mod notify;
mod osd;
mod overview;
mod polkit;
mod power;
mod preview_chrome;
mod providers;
mod screencast;
mod screensaver;
mod screenshot_ui;
mod services;
mod shell_dbus;
mod shortcut_consent;
mod switcher;
mod tray;
mod wifi;
mod window_menu;
mod wired;
mod ws_popup;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use libadwaita as adw;
use roost_shell_host::control::{ControlClient, ControlError, Handled};

use logic::ClockFormat;

const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
const NOTIFICATIONS_SCHEMA: &str = "org.gnome.desktop.notifications";
const COLOR_SCHEMA: &str = "org.gnome.settings-daemon.plugins.color";
const SESSION_SCHEMA: &str = "org.gnome.desktop.session";
const SHELL_SCHEMA: &str = "org.gnome.shell";
const SCREENSAVER_SCHEMA: &str = "org.gnome.desktop.screensaver";

fn release_version() -> &'static str {
    match option_env!("ROOST_VERSION") {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => env!("CARGO_PKG_VERSION"),
    }
}

/// GSettings for `schema` when installed (Marlin ships the GNOME desktop
/// schemas); `None` keeps the shell running without them.
fn settings(schema: &str) -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup(schema, true)?;
    Some(gio::Settings::new(schema))
}

fn is_would_block(e: &ControlError) -> bool {
    matches!(e, ControlError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
}

/// Connect, greet, and wait (bounded) for the first snapshot.
fn attach_control() -> Option<ControlClient> {
    let path = std::env::var_os("ROOST_CONTROL_SOCKET")?;
    let mut control = ControlClient::connect(std::path::Path::new(&path)).ok()?;
    control.send_hello().ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut greeted = false;
    while Instant::now() < deadline {
        let step = if greeted {
            control
                .poll()
                .map(|h| matches!(h, Handled::Snapshot { .. }))
        } else {
            control.await_hello().map(|()| {
                greeted = true;
                false
            })
        };
        match step {
            Ok(true) => return Some(control),
            Ok(false) => {}
            Err(e) if is_would_block(&e) => std::thread::sleep(Duration::from_millis(2)),
            Err(e) => {
                eprintln!("roost-shell-gtk: control handshake failed: {e}");
                return None;
            }
        }
    }
    eprintln!("roost-shell-gtk: control handshake timed out");
    None
}

struct Shell {
    control: Option<ControlClient>,
    pills: gtk::Box,
    pill_state: Vec<bool>,
}

/// Overview actions routed through the control socket.
struct ShellActions(Rc<RefCell<Shell>>);

impl overview::OverviewActions for ShellActions {
    fn close_overview(&self) {
        if let Some(control) = self.0.borrow_mut().control.as_mut() {
            if control.model().is_overview_open() {
                let _ = control.toggle_overview();
            }
        }
    }

    fn activate_window(&self, id: u64) {
        if let Some(control) = self.0.borrow_mut().control.as_mut() {
            let _ = control.activate_window(id);
        }
    }

    fn running(&self) -> Vec<(u64, Option<String>)> {
        self.0
            .borrow()
            .control
            .as_ref()
            .map(|c| {
                c.model()
                    .windows()
                    .iter()
                    .map(|w| (w.id, w.app_id.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn set_app_grid(&self, active: bool) {
        if let Some(control) = self.0.borrow_mut().control.as_mut() {
            let _ = control.set_overview_app_grid(active);
        }
    }

    fn set_search(&self, active: bool) {
        if let Some(control) = self.0.borrow_mut().control.as_mut() {
            let _ = control.set_overview_search(active);
        }
    }

    fn open_overview(&self) {
        if let Some(control) = self.0.borrow_mut().control.as_mut() {
            if !control.model().is_overview_open() {
                let _ = control.toggle_overview();
            }
        }
    }
}

/// org.gnome.Shell's methods, routed to the OSD and the overview.
struct GnomeShellDbus {
    osd: Rc<osd::OsdUi>,
    overview: Rc<RefCell<overview::OverviewUi>>,
    shell: Rc<RefCell<Shell>>,
}

impl shell_dbus::ShellActions for GnomeShellDbus {
    fn show_osd(&self, request: &osd::OsdRequest) {
        self.osd.show(request);
    }

    fn focus_search(&self) {
        overview::OverviewUi::focus_search(&self.overview);
    }

    fn show_applications(&self) {
        overview::OverviewUi::show_apps(&self.overview);
    }

    fn overview_active(&self) -> bool {
        self.shell
            .borrow()
            .control
            .as_ref()
            .is_some_and(|c| c.model().is_overview_open())
    }

    fn set_accelerators(&self, accelerators: Vec<roost_shell_control::Accelerator>) {
        if let Some(control) = self.shell.borrow_mut().control.as_mut() {
            let _ = control.set_accelerators(accelerators);
        }
    }
}

fn panel_button(child: &impl IsA<gtk::Widget>, label: &str) -> gtk::Button {
    let button = gtk::Button::builder().child(child).build();
    button.add_css_class("panel-button");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

fn panel_menu_button(
    child: &impl IsA<gtk::Widget>,
    label: &str,
    popover: &gtk::Popover,
) -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .child(child)
        .popover(popover)
        .always_show_arrow(false)
        .build();
    button.add_css_class("panel-button");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    // GNOME focuses a panel menu's first item only when the keyboard
    // opened it: a click shows no focus ring (keyboard opens set it
    // back, see the keybinding toggle).
    popover.connect_show(|popover| {
        if let Some(window) = popover.root().and_downcast::<gtk::Window>() {
            window.set_focus_visible(false);
        }
    });
    button
}

fn render_pills(shell: &mut Shell) {
    let wanted = match shell.control.as_ref() {
        Some(control) => {
            let model = control.model();
            let occupied: Vec<u32> = model.windows().iter().map(|w| w.workspace).collect();
            logic::workspace_pills(model.workspaces(), model.active_workspace(), &occupied)
        }
        None => vec![true],
    };
    if wanted == shell.pill_state {
        return;
    }
    while let Some(child) = shell.pills.first_child() {
        shell.pills.remove(&child);
    }
    for active in &wanted {
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pill.add_css_class("ws-pill");
        if *active {
            pill.add_css_class("active");
            pill.set_size_request(logic::active_pill_width(wanted.len()), -1);
        }
        pill.set_valign(gtk::Align::Center);
        shell.pills.append(&pill);
    }
    shell.pill_state = wanted;
}

/// GNOME 51's date menu: the notification list beside the calendar
/// column, back on today each time it opens (dateMenu.js).
fn calendar_popover(notes: &gtk::Box) -> (gtk::Popover, Rc<calendar::CalendarUi>) {
    let popover = gtk::Popover::new();
    popover.add_css_class("roost-shell-popover");
    popover.add_css_class("roost-cal");
    popover.set_has_arrow(false);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("calendar-area");
    let cal = calendar::CalendarUi::new(jiff::Zoned::now().date());
    row.append(notes);
    row.append(&cal.column);
    popover.set_child(Some(&row));
    {
        let cal = cal.clone();
        popover.connect_show(move |_| cal.reset());
    }
    {
        let popover = popover.downgrade();
        cal.connect_open(move || {
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
        });
    }
    (popover, cal)
}

/// One GNOME 51 quick toggle (`.quick-toggle`): icon, bold title and an
/// optional subtitle in a 176x48 pill.
fn qs_tile(icon: &str, title: &str, subtitle: Option<&str>) -> services::Tile {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    let image = gtk::Image::from_icon_name(icon);
    image.set_valign(gtk::Align::Center);
    image.add_css_class("qs-icon");
    content.append(&image);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_valign(gtk::Align::Center);
    // Fixed-width toggles (GNOME's 12em): labels ellipsize instead of
    // widening the panel.
    let t = gtk::Label::new(Some(title));
    t.set_xalign(0.0);
    t.set_max_width_chars(1);
    t.set_hexpand(true);
    t.set_ellipsize(gtk::pango::EllipsizeMode::End);
    t.add_css_class("qs-title");
    text.append(&t);
    let s = gtk::Label::new(subtitle);
    s.set_xalign(0.0);
    s.set_max_width_chars(1);
    s.set_hexpand(true);
    s.set_ellipsize(gtk::pango::EllipsizeMode::End);
    s.add_css_class("qs-subtitle");
    s.set_visible(subtitle.is_some());
    text.append(&s);
    content.append(&text);
    let toggle = gtk::ToggleButton::builder().child(&content).build();
    toggle.add_css_class("qs-toggle");
    toggle.update_property(&[gtk::accessible::Property::Label(title)]);
    services::Tile::new(toggle, s, image)
}

/// A GNOME quick toggle with a menu (`.quick-toggle-has-menu`): the
/// toggle, a 1px separator, and an arrow that opens `menu` in place.
fn qs_menu_tile(icon: &str, title: &str, subtitle: Option<&str>, menu: &QsMenu) -> services::Tile {
    let mut tile = qs_tile(icon, title, subtitle);
    tile.button.add_css_class("has-menu");
    let outer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    outer.add_css_class("qs-toggle-has-menu");
    outer.append(&tile.button);
    tile.button.set_hexpand(true);
    let separator = gtk::Separator::new(gtk::Orientation::Vertical);
    separator.add_css_class("qs-separator");
    outer.append(&separator);
    let arrow = gtk::ToggleButton::new();
    arrow.set_icon_name("go-next-symbolic");
    arrow.add_css_class("qs-menu-button");
    arrow.update_property(&[gtk::accessible::Property::Label(&format!("{title} Menu"))]);
    outer.append(&arrow);
    {
        let revealer = menu.revealer.clone();
        arrow.connect_toggled(move |a| revealer.set_visible(a.is_active()));
    }
    {
        let arrow = arrow.clone();
        menu.revealer.connect_visible_notify(move |r| {
            if arrow.is_active() != r.is_visible() {
                arrow.set_active(r.is_visible());
            }
        });
    }
    // The separator and arrow follow the toggle's checked state.
    {
        let (outer, button) = (outer.clone(), tile.button.clone());
        let sync = move || {
            if button.is_active() {
                outer.add_css_class("checked");
            } else {
                outer.remove_css_class("checked");
            }
        };
        sync();
        tile.button.connect_toggled(move |_| sync());
    }
    tile.outer = outer.upcast();
    tile
}

/// A quick toggle's menu (`.quick-toggle-menu`): a header with a round
/// icon and title, then a section of items. Shown in place, spanning
/// the grid under its toggle's row.
struct QsMenu {
    revealer: gtk::Box,
    header_icon: gtk::Image,
    section: gtk::Box,
}

impl QsMenu {
    fn new(icon: &str, title: &str) -> Self {
        let revealer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        revealer.add_css_class("qs-menu");
        revealer.set_visible(false);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header.add_css_class("qs-menu-header");
        let header_icon = gtk::Image::from_icon_name(icon);
        header_icon.add_css_class("qs-menu-icon");
        header_icon.set_pixel_size(24);
        header.append(&header_icon);
        let label = gtk::Label::new(Some(title));
        label.add_css_class("qs-menu-title");
        header.append(&label);
        revealer.append(&header);
        let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
        revealer.append(&section);
        Self {
            revealer,
            header_icon,
            section,
        }
    }

    /// One item: icon, label and a check ornament (hidden until set).
    fn item(&self, icon: Option<&str>, label: &str) -> (gtk::Button, gtk::Image) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        if let Some(icon) = icon {
            row.append(&gtk::Image::from_icon_name(icon));
        }
        row.append(&gtk::Label::new(Some(label)));
        let ornament = gtk::Image::from_icon_name("ornament-check-symbolic");
        ornament.set_visible(false);
        row.append(&ornament);
        let button = gtk::Button::builder().child(&row).build();
        button.add_css_class("qs-menu-item");
        button.update_property(&[gtk::accessible::Property::Label(label)]);
        self.section.append(&button);
        (button, ornament)
    }

    /// A box for items rebuilt at run time (the Wi-Fi networks).
    fn list(&self) -> gtk::Box {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        self.section.append(&list);
        list
    }

    fn separator(&self) {
        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        separator.add_css_class("qs-menu-separator");
        self.section.append(&separator);
    }
}

/// GNOME's quick-settings grid (`QuickSettingsLayout`): two 176px
/// columns, 12px apart; hidden toggles leave no gap; an open toggle
/// menu takes a full-width row under its toggle's row.
struct QsGrid {
    grid: gtk::Grid,
    /// Each toggle (whose visibility decides placement), the wrapper
    /// the grid holds, and its menu.
    items: RefCell<Vec<(gtk::Widget, gtk::Box, Option<gtk::Box>)>>,
}

impl QsGrid {
    fn new() -> Rc<Self> {
        let grid = gtk::Grid::builder()
            .row_spacing(12)
            .column_spacing(12)
            .column_homogeneous(true)
            .build();
        grid.add_css_class("qs-grid");
        Rc::new(Self {
            grid,
            items: RefCell::new(Vec::new()),
        })
    }

    fn add(self: &Rc<Self>, tile: &services::Tile, menu: Option<&QsMenu>) {
        let menu = menu.map(|m| m.revealer.clone());
        // GtkGrid lines toggles up by baseline, and a toggle with a
        // subtitle has a different one: the row grows 2px past GNOME's
        // 48. A vertical box reports no baseline.
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrapper.append(&tile.outer);
        self.items
            .borrow_mut()
            .push((tile.outer.clone(), wrapper, menu.clone()));
        // A strong reference: the grid lives as long as its tiles do
        // (the quick-settings popover, for the whole session).
        let this = self.clone();
        let relayout = move || this.relayout();
        let r = relayout.clone();
        tile.outer.connect_visible_notify(move |_| r());
        if let Some(menu) = menu {
            menu.connect_visible_notify(move |_| relayout());
        }
        self.relayout();
    }

    fn relayout(&self) {
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        let items = self.items.borrow();
        let visible: Vec<_> = items.iter().filter(|(w, _, _)| w.is_visible()).collect();
        let mut row = 0;
        for pair in visible.chunks(2) {
            for (col, (_, wrapper, _)) in pair.iter().enumerate() {
                self.grid.attach(wrapper, col as i32, row, 1, 1);
            }
            row += 1;
            for (_, _, menu) in pair {
                if let Some(menu) = menu {
                    self.grid.attach(menu, 0, row, 2, 1);
                    row += 1;
                }
            }
        }
    }
}

fn qs_round(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("qs-round");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

/// The panel's status icons that quick-settings services drive.
struct PanelIcons {
    mic: gtk::Image,
    network: gtk::Image,
    dnd: gtk::Image,
    volume: gtk::Image,
    power_profile: gtk::Image,
    battery: gtk::Box,
    battery_icon: gtk::Image,
    battery_percentage: gtk::Label,
}

fn quick_settings_popover(
    shell: &Rc<RefCell<Shell>>,
    notify: &Rc<notify::NotifyUi>,
    power_ui: &Rc<power::PowerUi>,
    icons: PanelIcons,
) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.add_css_class("roost-shell-popover");
    popover.add_css_class("roost-qs");
    popover.set_has_arrow(false);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 12);

    // Top row: screenshot and settings left, lock and power right.
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let screenshot = qs_round("screenshooter-symbolic", "Take Screenshot");
    // GNOME shows the Settings app's own icon, in its symbolic form.
    let settings_btn = qs_round("emblem-system-symbolic", "Settings");
    settings_btn.set_child(Some(&gtk::Image::from_gicon(&gio::ThemedIcon::from_names(
        &["org.gnome.Settings-symbolic", "emblem-system-symbolic"],
    ))));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let lock = qs_round("system-lock-screen-symbolic", "Lock");
    let power = qs_round("system-shutdown-symbolic", "Power Off Menu");
    top.append(&screenshot);
    top.append(&settings_btn);
    top.append(&spacer);
    top.append(&lock);
    top.append(&power);
    {
        // GNOME's Screenshot button, through org.gnome.Shell.Screenshot
        // (#61): close the menu first so it is not in the shot.
        let (popover, notify) = (popover.clone(), notify.clone());
        screenshot.connect_clicked(move |_| {
            popover.popdown();
            take_screenshot(false, notify.clone());
        });
    }
    settings_btn.connect_clicked(|_| {
        let _ = gio::AppInfo::launch_default_for_uri("settings://", None::<&gio::AppLaunchContext>)
            .or_else(|_| {
                std::process::Command::new("gnome-control-center")
                    .spawn()
                    .map(|_| ())
                    .map_err(|e| glib::Error::new(gio::IOErrorEnum::Failed, &e.to_string()))
            });
    });
    {
        let shell = shell.clone();
        let popover = popover.clone();
        lock.connect_clicked(move |_| {
            popover.popdown();
            if let Some(control) = shell.borrow_mut().control.as_mut() {
                if let Err(e) = control.lock() {
                    eprintln!("roost-shell-gtk: lock failed: {e}");
                }
            }
        });
    }

    // Power: GNOME's shutdown menu (status/system.js ShutdownItem), a
    // quick toggle menu opening in place under the top row.
    let power_menu_box = QsMenu::new("system-shutdown-symbolic", "Power Off");
    for action in power_ui.actions() {
        let label = match action {
            power::Action::Suspend => "Suspend",
            power::Action::Restart => "Restart…",
            power::Action::PowerOff => "Power Off…",
            power::Action::LogOut => "Log Out…",
        };
        // GNOME's menu always separates the session actions, even when
        // no power action sits above them.
        if *action == power::Action::LogOut {
            power_menu_box.separator();
        }
        let (item, _) = power_menu_box.item(None, label);
        let (power_ui, popover, action) = (power_ui.clone(), popover.clone(), *action);
        item.connect_clicked(move |_| {
            popover.popdown();
            power_ui.activate(action);
        });
    }
    let power_menu = power_menu_box.revealer.clone();
    {
        let menu = power_menu.clone();
        power.connect_clicked(move |_| menu.set_visible(!menu.is_visible()));
    }
    {
        let popover = popover.clone();
        power_menu.connect_visible_notify(move |menu| {
            if menu.is_visible() {
                popover.add_css_class("dimmed");
            } else {
                popover.remove_css_class("dimmed");
            }
        });
    }
    {
        let menu = power_menu.clone();
        popover.connect_closed(move |_| menu.set_visible(false));
    }

    // Volume: mute button plus slider (PipeWire via wpctl).
    let volume_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let mute = gtk::Button::from_icon_name("audio-volume-high-symbolic");
    mute.add_css_class("flat");
    mute.update_property(&[gtk::accessible::Property::Label("Mute")]);
    volume_row.append(&mute);
    let slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    slider.set_value(100.0);
    slider.set_hexpand(true);
    slider.update_property(&[gtk::accessible::Property::Label("Volume")]);
    volume_row.append(&slider);
    // GNOME's sound output menu (volume.js): an arrow beside the slider,
    // offered with more than one output.
    let sound_menu_ui = QsMenu::new("audio-headphones-symbolic", "Sound Output");
    let sound_list = sound_menu_ui.list();
    sound_menu_ui.separator();
    let (sound_settings, _) = sound_menu_ui.item(None, "Sound Settings");
    sound_settings.connect_clicked(|_| {
        let _ = std::process::Command::new("gnome-control-center")
            .arg("sound")
            .spawn();
    });
    let sound_arrow = gtk::ToggleButton::new();
    sound_arrow.set_icon_name("go-next-symbolic");
    sound_arrow.add_css_class("qs-slider-menu-button");
    sound_arrow.update_property(&[gtk::accessible::Property::Label("Open sound output menu")]);
    sound_arrow.set_visible(false);
    {
        let revealer = sound_menu_ui.revealer.clone();
        sound_arrow.connect_toggled(move |a| revealer.set_visible(a.is_active()));
    }
    {
        let arrow = sound_arrow.clone();
        sound_menu_ui.revealer.connect_visible_notify(move |r| {
            if arrow.is_active() != r.is_visible() {
                arrow.set_active(r.is_visible());
            }
        });
    }
    volume_row.append(&sound_arrow);

    // GNOME's microphone slider (volume.js `InputStreamSlider`): shown
    // only while an app records; the icon button mutes.
    let mic_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let mic_mute = gtk::Button::from_icon_name("microphone-sensitivity-medium-symbolic");
    mic_mute.add_css_class("flat");
    mic_mute.update_property(&[gtk::accessible::Property::Label("Mute")]);
    mic_row.append(&mic_mute);
    let mic = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    mic.set_hexpand(true);
    mic.update_property(&[gtk::accessible::Property::Label("Microphone")]);
    mic_row.append(&mic);
    mic_row.set_visible(false);

    // Brightness: hidden without a backlight.
    let brightness_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    brightness_row.append(&gtk::Image::from_icon_name("display-brightness-symbolic"));
    let brightness = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    brightness.set_hexpand(true);
    brightness.update_property(&[gtk::accessible::Property::Label("Brightness")]);
    brightness_row.append(&brightness);

    // Toggle grid, two columns, GNOME 51 order (panel.js
    // `_setupIndicators`). Tiles whose service is absent hide, and the
    // grid closes the gap.
    let grid = QsGrid::new();
    // GNOME's Wi-Fi menu: the networks, then All Networks (Settings).
    let wifi_menu_ui = QsMenu::new("network-wireless-symbolic", "Wi\u{2013}Fi");
    let wifi_menu = wifi::WifiMenu::new(wifi_menu_ui.list());
    wifi_menu_ui.separator();
    let (all_networks, _) = wifi_menu_ui.item(None, "All Networks");
    all_networks.connect_clicked(|_| {
        let _ = std::process::Command::new("gnome-control-center")
            .arg("wifi")
            .spawn();
    });
    {
        let wifi_menu = wifi_menu.clone();
        wifi_menu_ui
            .revealer
            .connect_visible_notify(move |r| wifi_menu.set_open(r.is_visible()));
    }
    let wifi = qs_menu_tile("network-wireless-symbolic", "Wi-Fi", None, &wifi_menu_ui);
    wifi_menu.set_tile(wifi.clone());
    // GNOME's wired menu: the devices' profiles, then Wired Settings.
    let wired_menu_ui = QsMenu::new("network-wired-symbolic", "Wired Connections");
    let wired_menu = wired::WiredMenu::new(wired_menu_ui.list());
    wired_menu_ui.separator();
    let (wired_settings, _) = wired_menu_ui.item(None, "Wired Settings");
    wired_settings.connect_clicked(|_| {
        let _ = std::process::Command::new("gnome-control-center")
            .arg("network")
            .spawn();
    });
    let wired = qs_menu_tile("network-wired-symbolic", "Wired", None, &wired_menu_ui);
    wired_menu.set_tile(wired.clone());
    // GNOME's Bluetooth menu: devices, a placeholder, Bluetooth Settings.
    let bt_menu_ui = QsMenu::new("bluetooth-active-symbolic", "Bluetooth");
    let bt_list = bt_menu_ui.list();
    let bt_placeholder = gtk::Label::new(None);
    bt_placeholder.add_css_class("bt-menu-placeholder");
    bt_placeholder.set_wrap(true);
    bt_placeholder.set_justify(gtk::Justification::Center);
    bt_menu_ui.section.append(&bt_placeholder);
    let bt_menu = bt_menu::BtMenu::new(bt_list, bt_placeholder);
    bt_menu_ui.separator();
    let (bt_settings, _) = bt_menu_ui.item(None, "Bluetooth Settings");
    bt_settings.connect_clicked(|_| {
        let _ = std::process::Command::new("gnome-control-center")
            .arg("bluetooth")
            .spawn();
    });
    let bluetooth = qs_menu_tile("bluetooth-active-symbolic", "Bluetooth", None, &bt_menu_ui);
    bt_menu.set_tile(bluetooth.clone());
    let power_menu_ui = QsMenu::new("power-profile-balanced-symbolic", "Power Mode");
    let power_items: Vec<(&'static str, gtk::Button, gtk::Image)> = [
        ("performance", "Performance"),
        ("balanced", "Balanced"),
        ("power-saver", "Power Saver"),
    ]
    .into_iter()
    .map(|(name, label)| {
        let (button, ornament) = power_menu_ui.item(Some(logic::power_mode_icon(name)), label);
        (name, button, ornament)
    })
    .collect();
    power_menu_ui.separator();
    let (power_settings, _) = power_menu_ui.item(None, "Power Settings");
    power_settings.connect_clicked(|_| {
        let _ = std::process::Command::new("gnome-control-center")
            .arg("power")
            .spawn();
    });
    let power_mode = qs_menu_tile(
        "power-profile-balanced-symbolic",
        "Power Mode",
        Some("Balanced"),
        &power_menu_ui,
    );
    let night = qs_tile("night-light-symbolic", "Night Light", None);
    let dark = qs_tile("dark-mode-symbolic", "Dark Style", None);
    let dnd_tile = qs_tile("notifications-disabled-symbolic", "Do Not Disturb", None);
    grid.add(&wifi, Some(&wifi_menu_ui));
    grid.add(&wired, Some(&wired_menu_ui));
    grid.add(&bluetooth, Some(&bt_menu_ui));
    grid.add(&power_mode, Some(&power_menu_ui));
    for tile in [&night, &dark, &dnd_tile] {
        grid.add(tile, None);
    }
    let (wifi_outer, wired_outer, bluetooth_outer, power_outer, night_outer, dark_outer, dnd_outer) = (
        wifi.outer.clone(),
        wired.outer.clone(),
        bluetooth.outer.clone(),
        power_mode.outer.clone(),
        night.outer.clone(),
        dark.outer.clone(),
        dnd_tile.outer.clone(),
    );
    // While a toggle menu is open GNOME dims everything else in the
    // panel (brightness -0.4); selecting an item closes the panel, as
    // PopupMenu activation does.
    {
        let popover = popover.clone();
        power_menu_ui.revealer.connect_visible_notify(move |menu| {
            if menu.is_visible() {
                popover.add_css_class("dimmed");
            } else {
                popover.remove_css_class("dimmed");
            }
        });
    }
    for (_, button, _) in &power_items {
        let popover = popover.clone();
        button.connect_clicked(move |_| popover.popdown());
    }
    {
        let popover = popover.clone();
        power_settings.connect_clicked(move |_| popover.popdown());
    }
    {
        let menu = power_menu_ui.revealer.clone();
        popover.connect_closed(move |_| menu.set_visible(false));
    }
    // The Wi-Fi and Bluetooth menus dim the panel and reset on close the
    // same way; their rows close the panel themselves.
    for menu in [
        &wifi_menu_ui.revealer,
        &wired_menu_ui.revealer,
        &bt_menu_ui.revealer,
        &sound_menu_ui.revealer,
    ] {
        {
            let popover = popover.clone();
            menu.connect_visible_notify(move |menu| {
                if menu.is_visible() {
                    popover.add_css_class("dimmed");
                } else {
                    popover.remove_css_class("dimmed");
                }
            });
        }
        let menu = menu.clone();
        popover.connect_closed(move |_| menu.set_visible(false));
    }
    for settings in [
        &all_networks,
        &wired_settings,
        &bt_settings,
        &sound_settings,
    ] {
        let popover = popover.clone();
        settings.connect_clicked(move |_| popover.popdown());
    }
    let dnd = dnd_tile.button.clone();

    if let Some(iface) = settings(INTERFACE_SCHEMA) {
        dark.button
            .set_active(logic::is_dark(&iface.string("color-scheme")));
        dark.button.connect_toggled(move |t| {
            let _ = iface.set_string("color-scheme", logic::color_scheme_for(t.is_active()));
        });
    }
    match settings(COLOR_SCHEMA) {
        Some(color) => {
            night
                .button
                .set_active(color.boolean("night-light-enabled"));
            night.button.connect_toggled(move |t| {
                let _ = color.set_boolean("night-light-enabled", t.is_active());
            });
        }
        None => night.present(false),
    }
    let battery_summary = gtk::Label::new(None);
    battery_summary.set_xalign(0.0);
    battery_summary.set_visible(false);
    services::attach(&Rc::new(services::Widgets {
        wifi,
        wifi_menu,
        wired_menu,
        bt_menu,
        wired,
        bluetooth,
        power_mode,
        power_header: power_menu_ui.header_icon.clone(),
        power_items,
        panel_mic: icons.mic,
        mic,
        mic_mute,
        mic_row: mic_row.clone(),
        panel_network: icons.network,
        panel_volume: icons.volume,
        panel_power_profile: icons.power_profile,
        panel_battery: icons.battery,
        battery_icon: icons.battery_icon,
        battery_percentage: icons.battery_percentage,
        battery_summary: battery_summary.clone(),
        volume: slider,
        mute,
        brightness_row: brightness_row.clone(),
        sound_list,
        sound_arrow,
        brightness,
    }));
    // Do Not Disturb drives the shell's notification store (banners
    // held back) and mirrors GNOME's show-banners key.
    let notes_settings = settings(NOTIFICATIONS_SCHEMA);
    if let Some(notes) = notes_settings.as_ref() {
        notify.set_dnd(!notes.boolean("show-banners"));
    }
    dnd.set_active(notify.dnd());
    {
        let notify = notify.clone();
        dnd.connect_toggled(move |t| {
            notify.set_dnd(t.is_active());
            if let Some(notes) = notes_settings.as_ref() {
                let _ = notes.set_boolean("show-banners", !t.is_active());
            }
        });
    }
    {
        // Keep the tile, and the panel's DND icon, in step when DND
        // changes elsewhere.
        let (dnd, notify, dnd_icon) = (dnd.clone(), notify.clone(), icons.dnd.clone());
        glib::timeout_add_local(Duration::from_millis(250), move || {
            if dnd.is_active() != notify.dnd() {
                dnd.set_active(notify.dnd());
            }
            dnd_icon.set_visible(notify.dnd());
            glib::ControlFlow::Continue
        });
    }

    col.append(&top);
    col.append(&battery_summary);
    col.append(&power_menu);
    col.append(&volume_row);
    col.append(&sound_menu_ui.revealer);
    col.append(&mic_row);
    col.append(&brightness_row);
    col.append(&grid.grid);
    // Everything but an open toggle menu dims with the panel.
    for widget in [
        top.upcast_ref::<gtk::Widget>(),
        volume_row.upcast_ref(),
        mic_row.upcast_ref(),
        brightness_row.upcast_ref(),
    ] {
        widget.add_css_class("qs-dimmable");
    }
    for tile in [
        &wifi_outer,
        &wired_outer,
        &bluetooth_outer,
        &power_outer,
        &night_outer,
        &dark_outer,
        &dnd_outer,
    ] {
        tile.add_css_class("qs-dimmable");
    }
    popover.set_child(Some(&col));
    popover
}

/// Save a screenshot of the screen (or the focused window) through
/// org.gnome.Shell.Screenshot and say so in a notification, as GNOME's
/// screenshot button and keys do. Waits a moment so a closing menu is
/// not in the shot.
fn take_screenshot(window: bool, notify: Rc<notify::NotifyUi>) {
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        gio::bus_get(
            gio::BusType::Session,
            None::<&gio::Cancellable>,
            move |conn| {
                let Ok(conn) = conn else {
                    return;
                };
                let (method, args) = if window {
                    ("ScreenshotWindow", (true, false, true, "").to_variant())
                } else {
                    ("Screenshot", (false, true, "").to_variant())
                };
                conn.call(
                    Some("org.gnome.Shell.Screenshot"),
                    "/org/gnome/Shell/Screenshot",
                    "org.gnome.Shell.Screenshot",
                    method,
                    Some(&args),
                    glib::VariantTy::new("(bs)").ok(),
                    gio::DBusCallFlags::NONE,
                    10_000,
                    None::<&gio::Cancellable>,
                    move |reply| match reply {
                        Ok(reply) => {
                            let path = reply
                                .try_child_value(1)
                                .and_then(|v| v.get::<String>())
                                .unwrap_or_default();
                            let name = std::path::Path::new(&path)
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or(path);
                            notify.post(
                                "Screenshot",
                                "screenshot-recorded-symbolic",
                                "Screenshot captured",
                                &name,
                            );
                        }
                        Err(e) => eprintln!("roost-shell-gtk: screenshot failed: {e}"),
                    },
                );
            },
        );
    });
}

fn build(app: &adw::Application) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        // GNOME Shell's bundled icons (dark-mode, screenshooter...).
        gio::resources_register_include!("icons.gresource").ok();
        gtk::IconTheme::for_display(&display).add_resource_path("/org/roost/Shell/icons");
    }
    // GNOME's icon theme key, applied the way gnome-settings-daemon's
    // xsettings plugin applies it to GTK.
    if let (Some(gtk_settings), Some(iface)) =
        (gtk::Settings::default(), settings(INTERFACE_SCHEMA))
    {
        gtk_settings.set_gtk_icon_theme_name(Some(&iface.string("icon-theme")));
        iface.connect_changed(Some("icon-theme"), move |i, _| {
            gtk_settings.set_gtk_icon_theme_name(Some(&i.string("icon-theme")));
        });
    }
    // Text renders with GNOME's font hinting and antialiasing keys
    // (slight, grayscale by default), mapped the way
    // gnome-settings-daemon's xsettings plugin maps them for GTK.
    if let Some(gtk_settings) = gtk::Settings::default() {
        let apply = {
            let gtk_settings = gtk_settings.clone();
            move |iface: Option<&gio::Settings>| {
                let (hinting, antialias) = iface
                    .map(|i| {
                        (
                            i.string("font-hinting").to_string(),
                            i.string("font-antialiasing").to_string(),
                        )
                    })
                    .unwrap_or_else(|| ("slight".into(), "grayscale".into()));
                let (hint, style, aa, rgba) = logic::font_rendering(&hinting, &antialias);
                gtk_settings.set_property("gtk-xft-hinting", hint);
                gtk_settings.set_property("gtk-xft-hintstyle", style);
                gtk_settings.set_property("gtk-xft-antialias", aa);
                gtk_settings.set_property("gtk-xft-rgba", rgba);
            }
        };
        let iface = settings(INTERFACE_SCHEMA);
        apply(iface.as_ref());
        if let Some(iface) = iface {
            iface.connect_changed(None, move |i, key| {
                if key.starts_with("font-") {
                    apply(Some(i));
                }
            });
        }
    }
    // The shell chrome is always dark, as in GNOME.
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);

    let window = gtk::ApplicationWindow::new(app);
    window.add_css_class("roost-panel");
    window.set_title(Some("Top Bar"));
    window.init_layer_shell();
    window.set_layer(Layer::Top);
    window.set_namespace(Some("roost-panel"));
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.auto_exclusive_zone_enable();
    window.set_keyboard_mode(KeyboardMode::OnDemand);

    let pills = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    pills.set_valign(gtk::Align::Center);
    pills.add_css_class("ws-pills");
    let shell = Rc::new(RefCell::new(Shell {
        control: attach_control(),
        pills: pills.clone(),
        pill_state: Vec::new(),
    }));
    render_pills(&mut shell.borrow_mut());

    // The compositor toggles the overview on presses over the
    // Activities control (ACTIVITIES_WIDTH_PX); sending a toggle here
    // as well would cancel it out.
    let activities = panel_button(&pills, "Activities");

    let notify = notify::NotifyUi::new(app.upcast_ref());
    {
        let notify = notify.clone();
        glib::timeout_add_local(Duration::from_millis(250), move || {
            notify.tick();
            glib::ControlFlow::Continue
        });
    }
    let clock_label = gtk::Label::new(None);
    let (cal_popover, calendar_ui) = calendar_popover(notify.pane());
    // GNOME's calendar events through its calendar server.
    calendar_ui.set_event_source(events::EventSource::new());
    let clock = panel_menu_button(&clock_label, "Date and Time", &cal_popover);
    clock.add_css_class("clock-display");

    // System indicators in GNOME's order (panel.js `_indicators`): each
    // shows only while it means something; the power icon always does.
    let indicators = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    indicators.add_css_class("panel-status-indicators-box");
    let status_icon = |icon: &str, visible: bool| {
        let image = gtk::Image::from_icon_name(icon);
        image.add_css_class("system-status-icon");
        image.set_visible(visible);
        indicators.append(&image);
        image
    };
    // Privacy indicators lead (the microphone in use, orange unless
    // muted).
    let panel_mic = status_icon("microphone-sensitivity-medium-symbolic", false);
    panel_mic.update_property(&[gtk::accessible::Property::Label("Microphone in use")]);
    let panel_network = status_icon("network-wired-symbolic", false);
    let panel_dnd = status_icon("notifications-disabled-symbolic", false);
    let panel_volume = status_icon("audio-volume-high-symbolic", false);
    let panel_power_profile = status_icon("power-profile-balanced-symbolic", false);
    let panel_battery = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let battery_icon = gtk::Image::from_icon_name("battery-level-100-symbolic");
    battery_icon.add_css_class("system-status-icon");
    let battery_percentage = gtk::Label::new(None);
    panel_battery.append(&battery_icon);
    panel_battery.append(&battery_percentage);
    panel_battery.set_visible(false);
    indicators.append(&panel_battery);
    status_icon("system-shutdown-symbolic", true);
    let power_ui = {
        let shell = shell.clone();
        power::PowerUi::new(
            app.upcast_ref(),
            Rc::new(move || {
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    let _ = control.lock();
                }
            }),
        )
    };
    // GNOME's lock screen on ext-session-lock (the compositor decides
    // when, draws the blurred background, and checks the password).
    let lock_ui = {
        let shell = shell.clone();
        let interface = settings(INTERFACE_SCHEMA);
        lock::LockUi::new(
            app.upcast_ref(),
            Rc::new(move |password| {
                shell
                    .borrow_mut()
                    .control
                    .as_mut()
                    .and_then(|c| c.unlock(password).ok())
            }),
            Box::new(move || {
                interface
                    .as_ref()
                    .map(|s| ClockFormat::from_setting(&s.string("clock-format")))
                    .unwrap_or(ClockFormat::TwentyFourHour)
            }),
        )
    };
    let qs = quick_settings_popover(
        &shell,
        &notify,
        &power_ui,
        PanelIcons {
            mic: panel_mic,
            network: panel_network,
            dnd: panel_dnd,
            volume: panel_volume,
            power_profile: panel_power_profile,
            battery: panel_battery,
            battery_icon,
            battery_percentage,
        },
    );
    let system = panel_menu_button(&indicators, "System", &qs);

    // GNOME's screen recorder (org.gnome.Shell.Screencast) and its
    // indicator, first in the panel's right box as in panel.js.
    let recorder = {
        let notify = notify.clone();
        screencast::Recorder::new(Rc::new(move |summary: &str, body: &str| {
            notify.post("Screenshot", "screencast-recorded-symbolic", summary, body);
        }))
    };
    screencast::serve(recorder.clone());

    let bar = gtk::CenterBox::new();
    bar.set_start_widget(Some(&activities));
    bar.set_center_widget(Some(&clock));
    // AppIndicator items sit left of the system indicators.
    let end = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    end.append(&screencast::indicator(&recorder));
    end.append(&tray::tray());
    end.append(&system);
    bar.set_end_widget(Some(&end));
    window.set_child(Some(&bar));

    // Clock: GNOME's panel text, refreshed every second.
    let interface = settings(INTERFACE_SCHEMA);
    let clock_button = clock.clone();
    let tick = move || {
        let now = jiff::Zoned::now().datetime();
        let format = interface
            .as_ref()
            .map(|s| ClockFormat::from_setting(&s.string("clock-format")))
            .unwrap_or(ClockFormat::TwentyFourHour);
        // GNOME's clock keys; without the schema, weekday and date.
        let parts = interface
            .as_ref()
            .map(|s| logic::ClockParts {
                weekday: s.boolean("clock-show-weekday"),
                date: s.boolean("clock-show-date"),
                seconds: s.boolean("clock-show-seconds"),
            })
            .unwrap_or_default();
        let text = logic::clock_text(&now, format, parts);
        clock_label.set_label(&text);
        clock.update_property(&[gtk::accessible::Property::Description(&text)]);
        calendar_ui.set_clock_format(format);
        calendar_ui.set_today(now.date());
        glib::ControlFlow::Continue
    };
    tick();
    glib::timeout_add_seconds_local(1, tick);

    // Overview search, dash, and app grid (#54, #57).
    let favorites = {
        let pinned = roost_shell_host::favorites::Favorites::system()
            .ids()
            .to_vec();
        // Roost's own pins first, then the user's GNOME dash
        // (org.gnome.shell favorite-apps, #63), then GNOME's defaults.
        let gnome: Vec<String> = settings(SHELL_SCHEMA)
            .map(|s| {
                s.strv("favorite-apps")
                    .iter()
                    .map(|v| v.to_string())
                    .collect()
            })
            .unwrap_or_default();
        if !pinned.is_empty() {
            pinned
        } else if !gnome.is_empty() {
            gnome
        } else {
            logic::DEFAULT_FAVORITES
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        }
    };
    let apps = live_apps::LiveApps::new();
    let dash_apps = favorites.clone();
    let overview_ui = overview::OverviewUi::new(
        app.upcast_ref(),
        apps.clone(),
        favorites,
        Rc::new(ShellActions(shell.clone())),
    );
    let switcher_ui = switcher::SwitcherUi::new(app.upcast_ref(), apps.clone());
    // GNOME's WindowTracker: a window belongs to the desktop entry its
    // app id names (as the switcher finds its icon), shown in the grid
    // or not; a window no desktop file claims is an app of its own.
    if let Some(control) = shell.borrow_mut().control.as_mut() {
        let apps = apps.clone();
        let on_disk: RefCell<std::collections::HashMap<String, bool>> = RefCell::default();
        control.set_app_resolver(move |app_id| {
            let id = app_id.trim_end_matches(".desktop");
            if let Some(entry) = {
                let apps = apps.get();
                apps.entry(id)
                    .or_else(|| apps.entry(app_id))
                    .map(|e| e.app_id.clone())
            } {
                return Some(entry);
            }
            let found = *on_disk
                .borrow_mut()
                .entry(id.to_owned())
                .or_insert_with(|| roost_shell_host::apps::desktop_file_exists(id));
            found.then(|| id.to_owned())
        });
    }
    // The folder dialog's shade reaches under the top bar, as GNOME's.
    {
        let panel = window.clone();
        overview::OverviewUi::set_folder_shade(
            &overview_ui,
            Rc::new(move |on| {
                if on {
                    panel.add_css_class("shaded");
                } else {
                    panel.remove_css_class("shaded");
                }
            }),
        );
    }
    // org.gnome.Shell for the rest of GNOME: the OSD gnome-settings-daemon
    // shows for volume and brightness keys, search and the app grid.
    let osd_ui = osd::OsdUi::new(app.upcast_ref());
    let screensaver = screensaver::start(
        {
            let shell = shell.clone();
            Rc::new(move || {
                let mut shell = shell.borrow_mut();
                let control = shell.control.as_mut().ok_or("compositor disconnected")?;
                control
                    .lock()
                    .map(|_| ())
                    .map_err(|_| "lock request failed")
            })
        },
        {
            let shell = shell.clone();
            Rc::new(move || shell.borrow().control.as_ref().is_some_and(|c| c.locked()))
        },
    );
    let gnome_shell = shell_dbus::start(Rc::new(GnomeShellDbus {
        osd: osd_ui.clone(),
        overview: overview_ui.clone(),
        shell: shell.clone(),
    }));

    // GNOME's screenshot UI (Print).
    let screenshot_ui = {
        let notify_shot = notify.clone();
        let notify_post = notify.clone();
        screenshot_ui::ScreenshotUi::new(
            app.upcast_ref(),
            Rc::new(move || take_screenshot(true, notify_shot.clone())),
            Rc::new(move |summary: &str, body: &str| {
                notify_post.post("Screenshot", "screenshot-recorded-symbolic", summary, body);
            }),
            recorder.clone(),
        )
    };

    // GNOME Shell is the session's polkit agent.
    polkit::start(app.upcast_ref());
    // GNOME's NetworkManager secret agent (Wi-Fi passwords).
    network_agent::start(app.upcast_ref());

    // GNOME Shell is IBus's panel: it draws the candidate window.
    ibus_panel::start(app.upcast_ref());

    // GNOME's workspace switcher popup.
    let workspace_popup = ws_popup::WorkspacePopup::new(app.upcast_ref());

    // GNOME Shell's own keybindings (org.gnome.shell.keybindings).
    {
        let run: Rc<dyn Fn(keybindings::Action)> = {
            let (shell, overview_ui, clock, system) = (
                shell.clone(),
                overview_ui.clone(),
                clock_button.clone(),
                system.clone(),
            );
            let (apps, notify, osd_ui) = (apps.clone(), notify.clone(), osd_ui.clone());
            let screenshot_ui = screenshot_ui.clone();
            let workspace_popup_keys = workspace_popup.clone();
            Rc::new(move |action| {
                use keybindings::Action;
                let toggle = |button: &gtk::MenuButton| {
                    if button.is_active() {
                        button.popdown();
                    } else {
                        button.popup();
                        // Opened from the keyboard: GNOME shows the
                        // first item's focus.
                        if let Some(window) = button.root().and_downcast::<gtk::Window>() {
                            window.set_focus_visible(true);
                        }
                    }
                };
                let dash_entry = |n: u8| {
                    let id = dash_apps.get(usize::from(n) - 1)?;
                    let id = id.trim_end_matches(".desktop");
                    apps.get().entry(id).cloned()
                };
                match action {
                    Action::LockScreen => {
                        if let Some(control) = shell.borrow_mut().control.as_mut() {
                            if let Err(e) = control.lock() {
                                eprintln!("roost-shell-gtk: lock failed: {e}");
                            }
                        }
                    }
                    Action::ToggleOverview => {
                        if let Some(control) = shell.borrow_mut().control.as_mut() {
                            let _ = control.toggle_overview();
                        }
                    }
                    Action::ToggleApplicationView => {
                        overview::OverviewUi::toggle_apps(&overview_ui);
                    }
                    Action::FocusActiveNotification => notify.focus_active(),
                    Action::ShiftOverviewUp | Action::ShiftOverviewDown => {
                        overview::OverviewUi::shift(
                            &overview_ui,
                            action == Action::ShiftOverviewUp,
                        );
                    }
                    Action::ToggleMessageTray => toggle(&clock),
                    Action::ToggleQuickSettings => toggle(&system),
                    Action::SwitchToApplication(n) => {
                        let Some(entry) = dash_entry(n) else { return };
                        // Its newest window when it runs, else a launch;
                        // the overview steps aside either way.
                        let window = shell.borrow().control.as_ref().and_then(|c| {
                            c.model()
                                .windows()
                                .iter()
                                .rev()
                                .find(|w| w.app_id.as_deref() == Some(entry.app_id.as_str()))
                                .map(|w| w.id)
                        });
                        let mut shell = shell.borrow_mut();
                        if let Some(control) = shell.control.as_mut() {
                            match window {
                                Some(id) => {
                                    let _ = control.activate_window(id);
                                }
                                None => {
                                    let _ = roost_shell_host::apps::launch(&entry);
                                }
                            }
                            if control.model().is_overview_open() {
                                let _ = control.toggle_overview();
                            }
                        }
                    }
                    Action::OpenNewWindow(n) => {
                        if let Some(entry) = dash_entry(n) {
                            let _ = roost_shell_host::apps::launch(&entry);
                        }
                    }
                    Action::ShowScreenshotUi => screenshot_ui.open(),
                    Action::ShowScreenRecordingUi => screenshot_ui.open_recording(),
                    Action::Screenshot => take_screenshot(false, notify.clone()),
                    Action::ScreenshotWindow => take_screenshot(true, notify.clone()),
                    Action::NextInputSource | Action::PreviousInputSource => {
                        if let Some(control) = shell.borrow_mut().control.as_mut() {
                            let _ =
                                control.switch_input_source(action == Action::PreviousInputSource);
                        }
                    }
                    Action::Close => {
                        let mut shell = shell.borrow_mut();
                        if let Some(control) = shell.control.as_mut() {
                            let focused = control
                                .model()
                                .windows()
                                .iter()
                                .find(|w| w.active)
                                .map(|w| w.id);
                            if let Some(id) = focused {
                                let _ = control.close_window(id);
                            }
                        }
                    }
                    Action::WindowMenu
                    | Action::ToggleMaximized
                    | Action::Unmaximize
                    | Action::Maximize
                    | Action::Minimize
                    | Action::TileLeft
                    | Action::TileRight
                    | Action::BeginMove
                    | Action::BeginResize => {
                        use roost_shell_control::WindowAction as W;
                        let what = match action {
                            Action::WindowMenu => W::ShowMenu,
                            Action::ToggleMaximized => W::ToggleMaximize,
                            Action::Unmaximize => W::Unmaximize,
                            Action::Maximize => W::Maximize,
                            Action::Minimize => W::Minimize,
                            Action::TileLeft => W::ToggleTiledLeft,
                            Action::TileRight => W::ToggleTiledRight,
                            Action::BeginMove => W::Move,
                            _ => W::Resize,
                        };
                        let mut shell = shell.borrow_mut();
                        if let Some(control) = shell.control.as_mut() {
                            let focused = control
                                .model()
                                .windows()
                                .iter()
                                .find(|w| w.active)
                                .map(|w| w.id);
                            if let Some(id) = focused {
                                let _ = control.window_action(id, what);
                            }
                        }
                    }
                    Action::Workspace(target) | Action::MoveToWorkspace(target) => {
                        let mut shell = shell.borrow_mut();
                        let Some(control) = shell.control.as_mut() else {
                            return;
                        };
                        let model = control.model();
                        let occupied: Vec<u32> =
                            model.windows().iter().map(|w| w.workspace).collect();
                        let Some(workspace) =
                            target.resolve(model.active_workspace(), model.workspaces(), &occupied)
                        else {
                            return;
                        };
                        let focused = model.windows().iter().find(|w| w.active).map(|w| w.id);
                        if matches!(action, Action::MoveToWorkspace(_)) {
                            // Mutter moves the window and follows it.
                            let Some(id) = focused else { return };
                            let _ = control.window_action(
                                id,
                                roost_shell_control::WindowAction::MoveToWorkspace { workspace },
                            );
                        }
                        let _ = control.focus_workspace(workspace);
                        // GNOME's switcher popup, outside the overview.
                        if !control.model().is_overview_open() {
                            let moved = matches!(action, Action::MoveToWorkspace(_));
                            let occupied = control
                                .model()
                                .windows()
                                .iter()
                                .map(|w| {
                                    if moved && w.active {
                                        workspace
                                    } else {
                                        w.workspace
                                    }
                                })
                                .max();
                            workspace_popup_keys.display(
                                workspace,
                                roost_shell_control::dynamic_workspace_count(occupied, workspace),
                            );
                        }
                    }
                    Action::BrightnessUp
                    | Action::BrightnessDown
                    | Action::BrightnessUpMonitor
                    | Action::BrightnessDownMonitor
                    | Action::BrightnessCycle
                    | Action::BrightnessCycleMonitor => {
                        let monitor = matches!(
                            action,
                            Action::BrightnessUpMonitor
                                | Action::BrightnessDownMonitor
                                | Action::BrightnessCycleMonitor
                        );
                        let output = shell
                            .borrow()
                            .control
                            .as_ref()
                            .and_then(|c| c.pointer_output().map(str::to_owned));
                        // No pointer-output/backlight match means no write to a different screen.
                        if monitor && output.is_none() {
                            return;
                        }
                        let step = match action {
                            Action::BrightnessUp | Action::BrightnessUpMonitor => {
                                services::BrightnessStep::Up
                            }
                            Action::BrightnessDown | Action::BrightnessDownMonitor => {
                                services::BrightnessStep::Down
                            }
                            _ => services::BrightnessStep::Cycle,
                        };
                        if let Some(level) = services::step_brightness_for(
                            step,
                            output.as_deref().filter(|_| monitor),
                        ) {
                            osd_ui.show(&osd::OsdRequest {
                                icon: Some("display-brightness-symbolic".into()),
                                level: Some(level),
                                ..Default::default()
                            });
                        }
                    }
                }
            })
        };
        let settings = keybindings::settings();
        let wm_settings = keybindings::wm_settings();
        let mutter_settings = keybindings::mutter_settings();
        let media_settings = keybindings::media_settings();
        let apply: Rc<dyn Fn()> = {
            let (settings, wm_settings, mutter_settings, media_settings, gnome_shell) = (
                settings.clone(),
                wm_settings.clone(),
                mutter_settings.clone(),
                media_settings.clone(),
                gnome_shell.clone(),
            );
            let shell = shell.clone();
            Rc::new(move || {
                // The switcher's chords go to the compositor, which holds
                // the popup open while their modifiers are held.
                let switcher: Vec<roost_shell_control::SwitcherKey> =
                    keybindings::switcher_keys(wm_settings.as_ref())
                        .iter()
                        .filter_map(|(a, kind)| keybindings::parse_switcher_key(a, *kind))
                        .take(roost_shell_control::MAX_SWITCHER_KEYS)
                        .collect();
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    let _ = control.set_switcher_keys(switcher);
                }
                let list = keybindings::bindings(
                    settings.as_ref(),
                    wm_settings.as_ref(),
                    mutter_settings.as_ref(),
                    media_settings.as_ref(),
                );
                let accels: Vec<(String, u32)> = list
                    .iter()
                    .map(|b| (b.accelerator.clone(), b.modes))
                    .collect();
                eprintln!(
                    "roost-shell-gtk: keybindings: {} grabs, maximize on {:?}",
                    accels.len(),
                    list.iter()
                        .filter(|b| b.action == keybindings::Action::Maximize)
                        .map(|b| b.accelerator.as_str())
                        .collect::<Vec<_>>()
                );
                let actions: Vec<keybindings::Action> = list.iter().map(|b| b.action).collect();
                let run = run.clone();
                gnome_shell.set_internal(
                    &accels,
                    Rc::new(move |i| {
                        if let Some(action) = actions.get(i) {
                            run(*action);
                        }
                    }),
                );
            })
        };
        apply();
        for settings in [settings, wm_settings, mutter_settings, media_settings]
            .into_iter()
            .flatten()
        {
            let apply = apply.clone();
            settings.connect_changed(None, move |_, _| apply());
            std::mem::forget(settings);
        }
    }

    // GNOME's background settings: the picture (the dark variant under
    // the dark style, as GNOME picks it) and primary-color, published to
    // the compositor's wallpaper drop file now and on every change.
    {
        let background = settings("org.gnome.desktop.background");
        let interface = settings(INTERFACE_SCHEMA);
        let screensaver = settings(SCREENSAVER_SCHEMA);
        let publish: Rc<dyn Fn()> = {
            let (background, interface, screensaver) =
                (background.clone(), interface.clone(), screensaver.clone());
            Rc::new(move || {
                let Some(bg) = background.as_ref() else {
                    return;
                };
                let dark = interface
                    .as_ref()
                    .is_some_and(|i| i.string("color-scheme") == "prefer-dark");
                let mut text = logic::wallpaper_drop(
                    &bg.string("picture-uri"),
                    &bg.string("picture-uri-dark"),
                    dark,
                    &bg.string("picture-options"),
                    &bg.string("primary-color"),
                    // GNOME 47's accent; older schemas have none (blue).
                    &interface
                        .as_ref()
                        .filter(|i| {
                            i.settings_schema()
                                .is_some_and(|schema| schema.has_key("accent-color"))
                        })
                        .map(|i| i.string("accent-color").to_string())
                        .unwrap_or_default(),
                );
                let lock_uri = screensaver
                    .as_ref()
                    .filter(|s| {
                        s.settings_schema()
                            .is_some_and(|schema| schema.has_key("picture-uri"))
                    })
                    .map(|s| {
                        let uri = s.string("picture-uri");
                        if !uri.starts_with("file://") {
                            return String::new();
                        }
                        // GIO decodes escaped path components before the
                        // compositor's simple local-file drop reader.
                        gio::File::for_uri(&uri)
                            .path()
                            .filter(|path| path.is_absolute())
                            .map(|path| format!("file://{}", path.display()))
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                text.push_str(&lock_uri);
                text.push('\n');
                let dir = std::env::var_os("XDG_RUNTIME_DIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir);
                let path = dir.join("roost-wallpaper");
                let tmp = dir.join(".roost-wallpaper.tmp");
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            })
        };
        publish();
        for s in [background, interface, screensaver].into_iter().flatten() {
            let publish = publish.clone();
            // `publish` holds both settings objects, so they live as long
            // as this handler does.
            s.connect_changed(None, move |_, _| publish());
        }
    }

    // GNOME's input settings (#60): keymap, repeat, touchpad, mouse and
    // hot corner, sent now and on every change.
    {
        let schemas = [
            "org.gnome.desktop.input-sources",
            "org.gnome.desktop.peripherals.keyboard",
            "org.gnome.desktop.peripherals.touchpad",
            "org.gnome.desktop.peripherals.mouse",
            INTERFACE_SCHEMA,
        ];
        let all: Rc<Vec<Option<gio::Settings>>> =
            Rc::new(schemas.iter().map(|s| settings(s)).collect());
        let send: Rc<dyn Fn()> = {
            let (shell, all) = (shell.clone(), all.clone());
            Rc::new(move || {
                let mut out = roost_shell_control::InputSettings::default();
                if let Some(sources) = all[0].as_ref() {
                    let list: Vec<(String, String)> =
                        sources.value("sources").get().unwrap_or_default();
                    let (layout, variant) = logic::xkb_from_sources(&list);
                    out.xkb_layout = layout;
                    out.xkb_variant = variant;
                    out.xkb_options = sources
                        .strv("xkb-options")
                        .iter()
                        .map(|o| o.to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                }
                if let Some(kbd) = all[1].as_ref() {
                    out.repeat = kbd.boolean("repeat");
                    out.repeat_delay_ms = kbd.uint("delay");
                    out.repeat_interval_ms = kbd.uint("repeat-interval");
                }
                if let Some(pad) = all[2].as_ref() {
                    out.tap_to_click = pad.boolean("tap-to-click");
                    out.touchpad_natural_scroll = pad.boolean("natural-scroll");
                    out.touchpad_speed_milli = (pad.double("speed") * 1000.0).round() as i32;
                    out.disable_while_typing = pad.boolean("disable-while-typing");
                }
                if let Some(mouse) = all[3].as_ref() {
                    out.mouse_natural_scroll = mouse.boolean("natural-scroll");
                    out.mouse_speed_milli = (mouse.double("speed") * 1000.0).round() as i32;
                }
                if let Some(iface) = all[4].as_ref() {
                    out.hot_corners = iface.boolean("enable-hot-corners");
                    if iface
                        .settings_schema()
                        .is_some_and(|schema| schema.has_key("enable-animations"))
                    {
                        out.enable_animations = iface.boolean("enable-animations");
                    }
                    if let Some(gtk_settings) = gtk::Settings::default() {
                        gtk_settings.set_gtk_enable_animations(out.enable_animations);
                    }
                }
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    let _ = control.set_input_settings(out);
                }
            })
        };
        send();
        for settings in all.iter().flatten() {
            let send = send.clone();
            settings.connect_changed(None, move |_, _| send());
        }
        std::mem::forget(all);
    }

    // GNOME's idle and lock settings drive the compositor's idle lock
    // (#63): sent now and on every change.
    {
        let session = settings(SESSION_SCHEMA);
        let screensaver = settings(SCREENSAVER_SCHEMA);
        let interface = settings(INTERFACE_SCHEMA);
        let send: Rc<dyn Fn()> = {
            let (shell, session, screensaver, interface) = (
                shell.clone(),
                session.clone(),
                screensaver.clone(),
                interface.clone(),
            );
            Rc::new(move || {
                let idle = session
                    .as_ref()
                    .map(|s| s.uint("idle-delay"))
                    .unwrap_or(300);
                let (enabled, delay) = screensaver
                    .as_ref()
                    .map(|s| (s.boolean("lock-enabled"), s.uint("lock-delay")))
                    .unwrap_or((true, 0));
                let animations = interface
                    .as_ref()
                    .filter(|s| {
                        s.settings_schema()
                            .is_some_and(|schema| schema.has_key("enable-animations"))
                    })
                    .is_none_or(|s| s.boolean("enable-animations"));
                let ms = logic::idle_lock_ms(idle, enabled, delay, animations);
                if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
                    let dir = std::path::PathBuf::from(dir);
                    let temporary = dir.join(".roost-idle-blank.tmp");
                    let policy = format!(
                        "{}\n{}\n",
                        u64::from(idle) * 1000,
                        if animations { 10_000 } else { 0 }
                    );
                    if std::fs::write(&temporary, policy).is_ok() {
                        let _ = std::fs::rename(temporary, dir.join("roost-idle-blank"));
                    }
                }
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    let _ = control.set_idle_timeout(ms);
                }
            })
        };
        send();
        for settings in [session, screensaver, interface].into_iter().flatten() {
            let send = send.clone();
            settings.connect_changed(None, move |_, _| send());
            // Keep the settings object (and its signal) alive.
            std::mem::forget(settings);
        }
    }

    // GNOME's window menu (header-bar right click).
    let window_menu = {
        let (shell, notify) = (shell.clone(), notify.clone());
        window_menu::WindowMenu::new(
            app.upcast_ref(),
            Rc::new(move |window, item| {
                let mut shell = shell.borrow_mut();
                let Some(control) = shell.control.as_mut() else {
                    return;
                };
                match item {
                    window_menu::Item::Screenshot => {
                        // The menu's window is the focused one.
                        let _ = control.activate_window(window);
                        take_screenshot(true, notify.clone());
                    }
                    window_menu::Item::Action(action) => {
                        let _ = control.window_action(window, action);
                    }
                    window_menu::Item::Close => {
                        let _ = control.close_window(window);
                    }
                }
            }),
        )
    };

    let shortcut_consent = {
        let shell = shell.clone();
        shortcut_consent::Consent::new(
            app.upcast_ref(),
            Rc::new(move |request, allow| {
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    let _ = control.shortcut_consent(request, allow);
                }
            }),
        )
    };
    // Compositor state: drain the control socket every frame.
    {
        let shell = shell.clone();
        let notify = notify.clone();
        let overview_ui = overview_ui.clone();
        let switcher_ui = switcher_ui.clone();
        let panel_window = window.clone();
        let chrome = {
            let shell = shell.clone();
            preview_chrome::PreviewChrome::new(
                app.upcast_ref(),
                Rc::new(move |id| {
                    if let Some(control) = shell.borrow_mut().control.as_mut() {
                        let _ = control.close_window(id);
                    }
                }),
            )
        };
        let chrome_apps = apps.clone();
        let shell_rc = shell.clone();
        let activities_button = activities.clone();
        let last_frames: RefCell<Vec<roost_shell_control::SwitcherThumbnail>> = RefCell::default();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            let mut shell = shell.borrow_mut();
            let mut results = Vec::new();
            let mut accelerators = Vec::new();
            let mut menus = Vec::new();
            let mut consent = None;
            let mut popups = Vec::new();
            if let Some(control) = shell.control.as_mut() {
                loop {
                    match control.poll() {
                        Ok(Handled::Gap { .. }) => {
                            let _ = control.request_snapshot();
                        }
                        Ok(Handled::WindowMenu(request)) => menus.push(request),
                        Ok(Handled::ShortcutConsent(request)) => consent = Some(request),
                        Ok(Handled::WorkspacePopup { index, count }) => {
                            popups.push((index, count));
                        }
                        Ok(Handled::Accelerator { action, time, mode }) => {
                            accelerators.push((action, time, mode));
                        }
                        Ok(Handled::CommandResult { id, status }) => results.push((
                            id,
                            matches!(status, roost_shell_control::CommandStatus::Applied),
                        )),
                        Ok(_) => {}
                        Err(e) if is_would_block(&e) => break,
                        Err(e) => {
                            eprintln!("roost-shell-gtk: control error: {e}");
                            break;
                        }
                    }
                }
            }
            let locked = shell.control.as_ref().is_some_and(|c| c.locked());
            render_pills(&mut shell);
            if let Some(control) = shell.control.as_ref() {
                switcher_ui.sync(control.model());
            }
            // The switcher's window thumbnails: the compositor draws the
            // windows into the frames, sent whenever they move.
            let frames = switcher_ui.thumbnail_frames();
            if *last_frames.borrow() != frames {
                if let Some(control) = shell.control.as_mut() {
                    if control.set_switcher_thumbnails(frames.clone()).is_ok() {
                        *last_frames.borrow_mut() = frames;
                    }
                }
            }
            let open = shell
                .control
                .as_ref()
                .is_some_and(|c| c.model().is_overview_open());
            drop(shell);
            if locked {
                notify.release_focus();
            }
            lock_ui.sync(locked);
            screensaver.sync(locked);
            if locked {
                shortcut_consent.dismiss();
            } else if let Some(request) = consent {
                shortcut_consent.sync(request);
            }
            for (action, time, mode) in accelerators {
                gnome_shell.accelerator_activated(action, time, mode);
            }
            for request in menus {
                window_menu.open(&request);
            }
            for (index, count) in popups {
                workspace_popup.display(index, count);
            }
            for (id, applied) in results {
                lock_ui.command_result(id, applied);
            }
            // The bar goes see-through and Activities shows as checked
            // while the overview is open, as GNOME draws them.
            if open {
                panel_window.add_css_class("overview");
                activities_button.add_css_class("checked");
            } else {
                panel_window.remove_css_class("overview");
                activities_button.remove_css_class("checked");
            }
            overview::OverviewUi::set_open(&overview_ui, open);
            // GNOME's preview chrome over the compositor's previews.
            let shell = shell_rc.borrow();
            if let Some(control) = shell.control.as_ref() {
                let (previews, hovered) = control.overview_previews();
                let model = control.model();
                let apps = chrome_apps.get();
                let lookup = |id: u64| {
                    let window = model.windows().iter().find(|w| w.id == id)?;
                    let icon = window
                        .app_id
                        .as_deref()
                        .and_then(|app| providers::provider_app(&apps, app))
                        .and_then(|entry| entry.icon.clone())
                        .map(|icon| -> gio::Icon {
                            if icon.starts_with('/') {
                                gio::FileIcon::new(&gio::File::for_path(&icon)).upcast()
                            } else {
                                gio::ThemedIcon::new(&icon).upcast()
                            }
                        })
                        .unwrap_or_else(|| {
                            gio::ThemedIcon::new("application-x-executable").upcast()
                        });
                    Some(preview_chrome::PreviewWindow {
                        title: window.title.clone(),
                        icon,
                    })
                };
                chrome.update(previews, hovered, &lookup);
            }
            glib::ControlFlow::Continue
        });
    }

    window.present();
}

fn main() -> glib::ExitCode {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("roost-shell-gtk {}", release_version());
        return glib::ExitCode::SUCCESS;
    }
    glib::set_prgname(Some("roost-shell-gtk"));
    let app = adw::Application::builder()
        .application_id("org.roost.Shell")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(build);
    app.run_with_args::<&str>(&[])
}
