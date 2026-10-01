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

mod logic;
mod notify;
mod overview;
mod services;

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
    button
}

fn render_pills(shell: &mut Shell) {
    let wanted = match shell.control.as_ref() {
        Some(control) => {
            let model = control.model();
            logic::workspace_pills(model.workspaces(), model.active_workspace())
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
        }
        pill.set_valign(gtk::Align::Center);
        shell.pills.append(&pill);
    }
    shell.pill_state = wanted;
}

fn calendar_popover(notes: &gtk::Box) -> (gtk::Popover, gtk::Label, gtk::Label) {
    let popover = gtk::Popover::new();
    popover.add_css_class("roost-shell-popover");
    popover.add_css_class("roost-cal");
    popover.set_has_arrow(false);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 24);

    // Right: date heading plus month grid.
    let cal = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let weekday = gtk::Label::new(None);
    weekday.add_css_class("weekday");
    weekday.set_halign(gtk::Align::Start);
    let full_date = gtk::Label::new(None);
    full_date.add_css_class("full-date");
    full_date.set_halign(gtk::Align::Start);
    let calendar = gtk::Calendar::new();
    cal.append(&weekday);
    cal.append(&full_date);
    cal.append(&calendar);

    row.append(notes);
    row.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    row.append(&cal);
    popover.set_child(Some(&row));
    (popover, weekday, full_date)
}

fn qs_tile(icon: &str, title: &str, subtitle: Option<&str>) -> services::Tile {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let image = gtk::Image::from_icon_name(icon);
    image.set_valign(gtk::Align::Center);
    content.append(&image);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_valign(gtk::Align::Center);
    let t = gtk::Label::new(Some(title));
    t.set_halign(gtk::Align::Start);
    text.append(&t);
    let s = gtk::Label::new(subtitle);
    s.set_halign(gtk::Align::Start);
    s.add_css_class("caption");
    s.set_visible(subtitle.is_some());
    text.append(&s);
    content.append(&text);
    let toggle = gtk::ToggleButton::builder().child(&content).build();
    toggle.add_css_class("qs-toggle");
    toggle.set_hexpand(true);
    toggle.update_property(&[gtk::accessible::Property::Label(title)]);
    services::Tile::new(toggle, s)
}

fn qs_round(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("qs-round");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

fn quick_settings_popover(
    shell: &Rc<RefCell<Shell>>,
    notify: &Rc<notify::NotifyUi>,
) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.add_css_class("roost-shell-popover");
    popover.add_css_class("roost-qs");
    popover.set_has_arrow(false);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 12);

    // Top row: screenshot and settings left, lock and power right.
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let screenshot = qs_round("camera-photo-symbolic", "Take Screenshot");
    let settings_btn = qs_round("emblem-system-symbolic", "Settings");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let lock = qs_round("system-lock-screen-symbolic", "Lock");
    let power = qs_round("system-shutdown-symbolic", "Power Off");
    top.append(&screenshot);
    top.append(&settings_btn);
    top.append(&spacer);
    top.append(&lock);
    top.append(&power);
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

    // Brightness: hidden without a backlight.
    let brightness_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    brightness_row.append(&gtk::Image::from_icon_name("display-brightness-symbolic"));
    let brightness = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    brightness.set_hexpand(true);
    brightness.update_property(&[gtk::accessible::Property::Label("Brightness")]);
    brightness_row.append(&brightness);

    // Toggle grid, two columns, GNOME 51 order. Tiles whose service is
    // absent hide, and the flow closes the gap.
    let grid = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(2)
        .row_spacing(12)
        .column_spacing(12)
        .build();
    let wifi = qs_tile("network-wireless-symbolic", "Wi-Fi", None);
    let wired = qs_tile("network-wired-symbolic", "Wired", None);
    let bluetooth = qs_tile("bluetooth-active-symbolic", "Bluetooth", None);
    let power_mode = qs_tile(
        "power-profile-balanced-symbolic",
        "Power Mode",
        Some("Balanced"),
    );
    let night = qs_tile("night-light-symbolic", "Night Light", None);
    let dark = qs_tile("dark-mode-symbolic", "Dark Style", None);
    let dnd_tile = qs_tile("notifications-disabled-symbolic", "Do Not Disturb", None);
    for tile in [
        &wifi,
        &wired,
        &bluetooth,
        &power_mode,
        &night,
        &dark,
        &dnd_tile,
    ] {
        grid.append(&tile.button);
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
    services::attach(&Rc::new(services::Widgets {
        wifi,
        wired,
        bluetooth,
        power_mode,
        volume: slider,
        mute,
        brightness_row: brightness_row.clone(),
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
        // Keep the tile in step when DND changes elsewhere.
        let (dnd, notify) = (dnd.clone(), notify.clone());
        glib::timeout_add_local(Duration::from_millis(250), move || {
            if dnd.is_active() != notify.dnd() {
                dnd.set_active(notify.dnd());
            }
            glib::ControlFlow::Continue
        });
    }

    col.append(&top);
    col.append(&volume_row);
    col.append(&brightness_row);
    col.append(&grid);
    popover.set_child(Some(&col));
    popover
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

    let pills = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    pills.set_valign(gtk::Align::Center);
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
    let (cal_popover, weekday, full_date) = calendar_popover(notify.pane());
    let clock = panel_menu_button(&clock_label, "Date and Time", &cal_popover);

    let indicators = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for icon in [
        "network-wired-symbolic",
        "audio-volume-high-symbolic",
        "system-shutdown-symbolic",
    ] {
        indicators.append(&gtk::Image::from_icon_name(icon));
    }
    let qs = quick_settings_popover(&shell, &notify);
    let system = panel_menu_button(&indicators, "System", &qs);

    let bar = gtk::CenterBox::new();
    bar.set_start_widget(Some(&activities));
    bar.set_center_widget(Some(&clock));
    bar.set_end_widget(Some(&system));
    window.set_child(Some(&bar));

    // Clock: GNOME's panel text, refreshed every second.
    let interface = settings(INTERFACE_SCHEMA);
    let tick = move || {
        let now = jiff::Zoned::now().datetime();
        let format = interface
            .as_ref()
            .map(|s| ClockFormat::from_setting(&s.string("clock-format")))
            .unwrap_or(ClockFormat::TwentyFourHour);
        let text = logic::clock_text(&now, format);
        clock_label.set_label(&text);
        clock.update_property(&[gtk::accessible::Property::Description(&text)]);
        let (day, date) = logic::calendar_heading(&now);
        weekday.set_label(&day);
        full_date.set_label(&date);
        glib::ControlFlow::Continue
    };
    tick();
    glib::timeout_add_seconds_local(1, tick);

    // Overview search, dash, and app grid (#54, #57).
    let favorites = {
        let pinned = roost_shell_host::favorites::Favorites::system()
            .ids()
            .to_vec();
        if pinned.is_empty() {
            logic::DEFAULT_FAVORITES
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        } else {
            pinned
        }
    };
    let overview_ui = overview::OverviewUi::new(
        app.upcast_ref(),
        Rc::new(roost_shell_host::apps::AppProvider::system()),
        favorites,
        Rc::new(ShellActions(shell.clone())),
    );

    // Compositor state: drain the control socket every frame.
    {
        let shell = shell.clone();
        let overview_ui = overview_ui.clone();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            let mut shell = shell.borrow_mut();
            if let Some(control) = shell.control.as_mut() {
                loop {
                    match control.poll() {
                        Ok(Handled::Gap { .. }) => {
                            let _ = control.request_snapshot();
                        }
                        Ok(_) => {}
                        Err(e) if is_would_block(&e) => break,
                        Err(e) => {
                            eprintln!("roost-shell-gtk: control error: {e}");
                            break;
                        }
                    }
                }
            }
            render_pills(&mut shell);
            let open = shell
                .control
                .as_ref()
                .is_some_and(|c| c.model().is_overview_open());
            drop(shell);
            overview::OverviewUi::set_open(&overview_ui, open);
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
