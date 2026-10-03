//! roost-ibus-bridge: Roost's IBus client, the part GNOME Shell plays
//! for text input (ibusManager.js, inputMethod.js).
//!
//! It joins the session as the input method (input-method-v2) on the
//! private socket the compositor hands it (`WAYLAND_SOCKET`). While an
//! app has a text field focused it grabs the keyboard and sends each
//! key to an IBus input context (`ProcessKeyEvent`); IBus's preedit and
//! commits go to the app, and keys IBus does not take go back through
//! the virtual keyboard only this client is allowed. Without IBus it
//! starts `ibus-daemon --panel disable`, as GNOME Shell does (less its
//! `--xim`: XIM serves X11 apps, but the daemon would reach whatever
//! `DISPLAY` it inherits, the host's in a nested session). One
//! GLib main loop drives both the Wayland socket and IBus's bus.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::os::fd::AsFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gio::glib;
use gio::prelude::*;
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use xkbcommon::xkb;

const IBUS: &str = "org.freedesktop.IBus";
const IBUS_PATH: &str = "/org/freedesktop/IBus";
const CONTEXT: &str = "org.freedesktop.IBus.InputContext";
/// IBus capabilities: preedit text and focus (IBUS_CAP_PREEDIT_TEXT,
/// IBUS_CAP_FOCUS), as GNOME Shell's input method sets.
const CAPABILITIES: u32 = 1 | 8;
/// IBUS_RELEASE_MASK.
const RELEASE: u32 = 1 << 30;
/// How long a key may wait on IBus before it goes to the app anyway.
const CALL_TIMEOUT_MS: i32 = 2_000;

/// What IBus tells the bridge.
enum FromIbus {
    Commit(String),
    Preedit {
        text: String,
        cursor: u32,
        visible: bool,
    },
    HidePreedit,
    Forward {
        keycode: u32,
        state: u32,
    },
}

/// Release version stamped at build time, as in every Roost binary.
fn release_version() -> &'static str {
    match option_env!("ROOST_VERSION") {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => env!("CARGO_PKG_VERSION"),
    }
}

fn main() {
    if std::env::args_os()
        .skip(1)
        .any(|arg| arg == "--version" || arg == "-V")
    {
        println!("roost-ibus-bridge {}", release_version());
        return;
    }
    if let Err(e) = run() {
        eprintln!("roost-ibus-bridge: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let bus = ibus_connection()?;
    let reply = bus.call_sync(
        Some(IBUS),
        IBUS_PATH,
        IBUS,
        "CreateInputContext",
        Some(&("Roost",).to_variant()),
        glib::VariantTy::new("(o)").ok(),
        gio::DBusCallFlags::NONE,
        CALL_TIMEOUT_MS,
        None::<&gio::Cancellable>,
    )?;
    let path = reply
        .child_value(0)
        .str()
        .ok_or("CreateInputContext returned no path")?
        .to_owned();
    let context = IbusContext {
        bus: bus.clone(),
        path: path.clone(),
    };
    context.send("SetCapabilities", Some((CAPABILITIES,).to_variant()));
    eprintln!("roost-ibus-bridge: input context {path}");

    // IBus sends the context's signals to this connection only; they
    // queue here and are applied on the Wayland side, in order.
    let incoming: Rc<RefCell<VecDeque<FromIbus>>> = Rc::default();
    {
        let incoming = incoming.clone();
        let subscription = bus.subscribe_to_signal(
            None,
            Some(CONTEXT),
            None,
            Some(&path),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                if let Some(event) = ibus_event(signal.signal_name, signal.parameters) {
                    incoming.borrow_mut().push_back(event);
                }
            },
        );
        // Kept for the life of the bridge.
        std::mem::forget(subscription);
    }

    let conn = Connection::connect_to_env()?;
    let mut queue: EventQueue<Bridge> = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut bridge = Bridge {
        context: Some(context),
        incoming,
        ..Bridge::default()
    };
    queue.roundtrip(&mut bridge)?;
    let (Some(seat), Some(manager)) = (bridge.seat.clone(), bridge.im_manager.clone()) else {
        return Err("the compositor offers no seat or input-method-v2".into());
    };
    bridge.im = Some(manager.get_input_method(&seat, &qh, ()));
    if let Some(vk_manager) = &bridge.vk_manager {
        bridge.vk = Some(vk_manager.create_virtual_keyboard(&seat, &qh, ()));
    }
    queue.roundtrip(&mut bridge)?;
    eprintln!("roost-ibus-bridge: ready");

    // The Wayland socket drives the loop; GLib (IBus's signals) runs
    // between reads, at least every 50 ms.
    loop {
        queue.flush()?;
        if let Some(guard) = queue.prepare_read() {
            let wayland_fd = guard.connection_fd();
            let mut fds = [rustix::event::PollFd::new(
                &wayland_fd,
                rustix::event::PollFlags::IN,
            )];
            rustix::event::poll(&mut fds, 50)?;
            if fds[0].revents().contains(rustix::event::PollFlags::IN) {
                let _ = guard.read();
            }
        }
        queue.dispatch_pending(&mut bridge)?;
        bridge.apply_incoming();
        if bridge.gone {
            return Ok(());
        }
    }
}

/// The IBus bus, starting the daemon when none runs (GNOME Shell's
/// `ibus-daemon --panel disable`).
fn ibus_connection() -> Result<gio::DBusConnection, Box<dyn std::error::Error>> {
    let connect = |address: &str| {
        gio::DBusConnection::for_address_sync(
            address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            None::<&gio::Cancellable>,
        )
    };
    if let Some(address) = ibus_address() {
        if let Ok(bus) = connect(&address) {
            return Ok(bus);
        }
    }
    std::process::Command::new("ibus-daemon")
        .args(["--panel", "disable", "--daemonize"])
        .status()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(address) = ibus_address() {
            if let Ok(bus) = connect(&address) {
                return Ok(bus);
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("ibus-daemon did not come up".into())
}

/// `IBUS_ADDRESS`, else what `ibus address` reports for this display.
fn ibus_address() -> Option<String> {
    if let Some(address) = std::env::var("IBUS_ADDRESS").ok().filter(|a| !a.is_empty()) {
        return Some(address);
    }
    let out = std::process::Command::new("ibus")
        .arg("address")
        .output()
        .ok()?;
    let address = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!address.is_empty() && address != "(null)").then_some(address)
}

/// The bridge's IBus input context.
struct IbusContext {
    bus: gio::DBusConnection,
    path: String,
}

impl IbusContext {
    /// A call libibus makes without waiting for a reply (FocusIn,
    /// FocusOut, SetCapabilities); the daemon need not send one.
    fn send(&self, method: &str, args: Option<glib::Variant>) {
        let message =
            gio::DBusMessage::new_method_call(Some(IBUS), &self.path, Some(CONTEXT), method);
        if let Some(args) = args {
            message.set_body(&args);
        }
        message.set_flags(gio::DBusMessageFlags::NO_REPLY_EXPECTED);
        if let Err(e) = self
            .bus
            .send_message(&message, gio::DBusSendMessageFlags::NONE)
        {
            eprintln!("roost-ibus-bridge: {method}: {e}");
        }
    }

    /// Whether IBus's engine composes text: a keyboard layout (`xkb:`)
    /// or none has nothing to add, so keys then go straight to the app.
    fn composing(&self) -> bool {
        let Ok(reply) = self.bus.call_sync(
            Some(IBUS),
            IBUS_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(IBUS, "GlobalEngine").to_variant()),
            glib::VariantTy::new("(v)").ok(),
            gio::DBusCallFlags::NONE,
            CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
        ) else {
            return false;
        };
        // v holding v holding IBusEngineDesc `(sa{sv}ss...)`: the name is
        // its third field.
        let mut desc = reply.child_value(0);
        while let Some(inner) = desc.as_variant() {
            desc = inner;
        }
        let name = desc
            .try_child_value(2)
            .and_then(|n| n.str().map(str::to_owned));
        name.is_some_and(|name| !name.is_empty() && !name.starts_with("xkb:"))
    }

    /// Whether IBus takes this key.
    fn process_key(&self, keyval: u32, keycode: u32, state: u32) -> bool {
        match self.bus.call_sync(
            Some(IBUS),
            &self.path,
            CONTEXT,
            "ProcessKeyEvent",
            Some(&(keyval, keycode, state).to_variant()),
            glib::VariantTy::new("(b)").ok(),
            gio::DBusCallFlags::NONE,
            CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
        ) {
            Ok(reply) => reply.child_value(0).get::<bool>().unwrap_or(false),
            Err(e) => {
                eprintln!("roost-ibus-bridge: ProcessKeyEvent: {e}");
                false
            }
        }
    }
}

/// One input-context signal as the bridge acts on it.
fn ibus_event(member: &str, params: &glib::Variant) -> Option<FromIbus> {
    match member {
        "CommitText" => ibus_text(&params.child_value(0)).map(FromIbus::Commit),
        "UpdatePreeditText" => Some(FromIbus::Preedit {
            text: ibus_text(&params.child_value(0))?,
            cursor: params.child_value(1).get::<u32>()?,
            visible: params.child_value(2).get::<bool>()?,
        }),
        "HidePreeditText" => Some(FromIbus::HidePreedit),
        "ForwardKeyEvent" => Some(FromIbus::Forward {
            keycode: params.child_value(1).get::<u32>()?,
            state: params.child_value(2).get::<u32>()?,
        }),
        _ => None,
    }
}

/// The string inside a serialized IBusText (`v` holding `(sa{sv}sv)`).
fn ibus_text(value: &glib::Variant) -> Option<String> {
    let inner = value.as_variant().unwrap_or_else(|| value.clone());
    inner.try_child_value(2)?.str().map(str::to_owned)
}

/// The bridge's Wayland side.
#[derive(Default)]
struct Bridge {
    seat: Option<wl_seat::WlSeat>,
    im_manager: Option<ZwpInputMethodManagerV2>,
    vk_manager: Option<ZwpVirtualKeyboardManagerV1>,
    im: Option<ZwpInputMethodV2>,
    vk: Option<ZwpVirtualKeyboardV1>,
    grab: Option<ZwpInputMethodKeyboardGrabV2>,
    context: Option<IbusContext>,
    incoming: Rc<RefCell<VecDeque<FromIbus>>>,
    /// `done` events so far: the serial a commit names.
    serial: u32,
    /// Activation pending the next `done`, and the applied one.
    pending_active: Option<bool>,
    active: bool,
    xkb: Option<(xkb::Keymap, xkb::State)>,
    vk_keymap: bool,
    gone: bool,
}

impl Bridge {
    /// Deliver what IBus has sent so far (GLib dispatches the signal
    /// callbacks), then apply it.
    fn apply_incoming(&mut self) {
        let main = glib::MainContext::default();
        while main.pending() {
            main.iteration(false);
        }
        let events: Vec<FromIbus> = self.incoming.borrow_mut().drain(..).collect();
        for event in events {
            self.apply(event);
        }
    }

    /// IBus's mask for the current modifiers (X's bits).
    fn ibus_state(&self) -> u32 {
        let Some((_, state)) = &self.xkb else {
            return 0;
        };
        let active = |name: &str| state.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE);
        [
            (xkb::MOD_NAME_SHIFT, 1),
            (xkb::MOD_NAME_CAPS, 2),
            (xkb::MOD_NAME_CTRL, 4),
            (xkb::MOD_NAME_ALT, 8),
            (xkb::MOD_NAME_LOGO, 64),
        ]
        .into_iter()
        .filter(|(name, _)| active(name))
        .fold(0, |bits, (_, bit)| bits | bit)
    }

    /// One grabbed key: IBus first, then the app if IBus passes.
    fn key(&mut self, time: u32, key: u32, pressed: bool) {
        let keyval = self.xkb.as_ref().map_or(0, |(_, state)| {
            state.key_get_one_sym((key + 8).into()).raw()
        });
        let state = self.ibus_state() | if pressed { 0 } else { RELEASE };
        let started = Instant::now();
        let handled = self
            .context
            .as_ref()
            .is_some_and(|context| context.process_key(keyval, key, state));
        if std::env::var_os("ROOST_IBUS_DEBUG").is_some() {
            eprintln!(
                "roost-ibus-bridge: key {key} sym {keyval:#x} pressed {pressed}: IBus took it: {handled} ({:?})",
                started.elapsed()
            );
        }
        if let Some((_, xkb_state)) = &mut self.xkb {
            xkb_state.update_key(
                (key + 8).into(),
                if pressed {
                    xkb::KeyDirection::Down
                } else {
                    xkb::KeyDirection::Up
                },
            );
        }
        // The commit or preedit this key produced comes before later keys.
        self.apply_incoming();
        if !handled {
            self.forward(time, key, pressed);
        }
    }

    fn forward(&self, time: u32, key: u32, pressed: bool) {
        if let (Some(vk), true) = (&self.vk, self.vk_keymap) {
            vk.key(time, key, u32::from(pressed));
        }
    }

    fn apply(&mut self, event: FromIbus) {
        let Some(im) = &self.im else { return };
        if !self.active {
            return;
        }
        match event {
            FromIbus::Commit(text) => {
                im.set_preedit_string(String::new(), 0, 0);
                im.commit_string(text);
                im.commit(self.serial);
            }
            FromIbus::Preedit {
                text,
                cursor,
                visible,
            } => {
                let (text, cursor) = if visible {
                    // IBus counts characters; the protocol counts bytes.
                    let byte = text
                        .char_indices()
                        .nth(cursor as usize)
                        .map_or(text.len(), |(i, _)| i) as i32;
                    (text, byte)
                } else {
                    (String::new(), 0)
                };
                im.set_preedit_string(text, cursor, cursor);
                im.commit(self.serial);
            }
            FromIbus::HidePreedit => {
                im.set_preedit_string(String::new(), 0, 0);
                im.commit(self.serial);
            }
            FromIbus::Forward { keycode, state } => {
                self.forward(0, keycode, state & RELEASE == 0);
            }
        }
    }

    /// A `done`: apply activation (focus in with a keyboard grab, or
    /// focus out and release it).
    fn done(&mut self, qh: &QueueHandle<Self>) {
        self.serial = self.serial.wrapping_add(1);
        let Some(active) = self.pending_active.take() else {
            return;
        };
        if active == self.active {
            return;
        }
        self.active = active;
        eprintln!(
            "roost-ibus-bridge: text field {}",
            if active { "focused" } else { "left" }
        );
        if let Some(context) = &self.context {
            context.send(if active { "FocusIn" } else { "FocusOut" }, None);
        }
        // Keys go through IBus only while its engine composes (pinyin,
        // anthy...); with a plain layout they reach the app untouched.
        let composing = active && self.context.as_ref().is_some_and(IbusContext::composing);
        if composing {
            if self.grab.is_none() {
                self.grab = self.im.as_ref().map(|im| im.grab_keyboard(qh, ()));
            }
        } else if let Some(grab) = self.grab.take() {
            grab.release();
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Bridge {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(7), qh, ()));
                }
                "zwp_input_method_manager_v2" => {
                    state.im_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.vk_manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for Bridge {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => state.pending_active = Some(true),
            zwp_input_method_v2::Event::Deactivate => state.pending_active = Some(false),
            zwp_input_method_v2::Event::Done => state.done(qh),
            zwp_input_method_v2::Event::Unavailable => {
                eprintln!("roost-ibus-bridge: another input method is active");
                state.gone = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for Bridge {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_keyboard_grab_v2::Event::Keymap { format, fd, size } => {
                if let Some(keymap) = load_keymap(&fd, size) {
                    let xkb_state = xkb::State::new(&keymap);
                    state.xkb = Some((keymap, xkb_state));
                }
                if let (Some(vk), WEnum::Value(format)) = (&state.vk, format) {
                    vk.keymap(format as u32, fd.as_fd(), size);
                    state.vk_keymap = true;
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::Key {
                time,
                key,
                state: key_state,
                ..
            } => {
                let pressed = matches!(
                    key_state,
                    WEnum::Value(wayland_client::protocol::wl_keyboard::KeyState::Pressed)
                );
                state.key(time, key, pressed);
            }
            zwp_input_method_keyboard_grab_v2::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some((_, xkb_state)) = &mut state.xkb {
                    xkb_state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
                if let (Some(vk), true) = (&state.vk, state.vk_keymap) {
                    vk.modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
            }
            _ => {}
        }
    }
}

/// The grab's keymap, from its shared fd.
fn load_keymap(fd: &std::os::fd::OwnedFd, size: u32) -> Option<xkb::Keymap> {
    use std::os::unix::fs::FileExt;
    let mut bytes = vec![0u8; size as usize];
    let file = std::fs::File::from(fd.try_clone().ok()?);
    file.read_exact_at(&mut bytes, 0).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_string(
        &context,
        text.trim_end_matches('\0').to_owned(),
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
}

wayland_client::delegate_noop!(Bridge: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(Bridge: ignore ZwpInputMethodManagerV2);
wayland_client::delegate_noop!(Bridge: ignore ZwpVirtualKeyboardManagerV1);
wayland_client::delegate_noop!(Bridge: ignore ZwpVirtualKeyboardV1);
