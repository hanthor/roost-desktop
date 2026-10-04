//! roost-ibus-bridge: Roost's IBus client, the part GNOME Shell plays
//! for text input (ibusManager.js, inputMethod.js).
//!
//! It joins the session as the input method (input-method-v2) on the
//! private socket the compositor hands it (`WAYLAND_SOCKET`). While an
//! app has a text field focused it grabs the keyboard and sends each
//! key to an IBus input context (`ProcessKeyEvent`); IBus's preedit and
//! commits go to the app, and keys IBus does not take go back through
//! the virtual keyboard only this client is allowed. Without IBus it
//! starts `ibus-daemon --panel disable`, as GNOME Shell does. The
//! compositor starts ibus-x11 separately once its own XWayland is ready,
//! so XIM never connects to the inherited host DISPLAY. One
//! GLib main loop drives both the Wayland socket and IBus's bus.
//!
//! IBus also hears what GNOME Shell tells it about the field: the text
//! around the cursor (`SetSurroundingText`, and the engine's
//! `DeleteSurroundingText` back), its content type, and where the text
//! cursor is on screen (`SetCursorLocation`, so a candidate window
//! lands at the caret). input-method-v2 gives an input method the
//! cursor only through an input popup surface, in the text field's
//! surface coordinates; the bridge holds one such popup (never drawn)
//! and the compositor follows each rectangle it reports to the bridge,
//! and only to it, with the same rectangle in global coordinates. The
//! last rectangle before a `done` is therefore the global one.
//!
//! The candidate window is not the bridge's: as in GNOME, the shell is
//! IBus's panel. `--panel disable` only stops the daemon spawning its
//! own panel (ibus-ui-gtk3); the daemon routes panel calls to whoever
//! owns `org.freedesktop.IBus.Panel` on its bus, which the GTK shell
//! takes (crates/shell-gtk/src/ibus_panel.rs). Because the bridge's
//! capabilities leave out lookup tables and auxiliary text, the
//! engine's candidates go there, with the cursor location it sends.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::os::fd::AsFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gio::glib;
use gio::prelude::*;
use wayland_client::protocol::{wl_compositor, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
    zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use xkbcommon::xkb;

const IBUS: &str = "org.freedesktop.IBus";
const IBUS_PATH: &str = "/org/freedesktop/IBus";
const CONTEXT: &str = "org.freedesktop.IBus.InputContext";
/// IBus capabilities: preedit text, focus and surrounding text
/// (IBUS_CAP_PREEDIT_TEXT, IBUS_CAP_FOCUS, IBUS_CAP_SURROUNDING_TEXT),
/// as GNOME Shell's input method sets.
const CAPABILITIES: u32 = 1 | 8 | 32;
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
    /// Delete `nchars` characters starting `offset` characters from
    /// the cursor.
    DeleteSurrounding {
        offset: i32,
        nchars: u32,
    },
}

/// Text around the cursor as input-method-v2 gives it: offsets in bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Surrounding {
    text: String,
    cursor: u32,
    anchor: u32,
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
    let im = manager.get_input_method(&seat, &qh, ());
    // The popup that hears where the text cursor is.
    if let Some(compositor) = &bridge.compositor {
        let surface = compositor.create_surface(&qh, ());
        let popup = im.get_input_popup_surface(&surface, &qh, ());
        bridge._popup = Some((surface, popup));
    }
    bridge.im = Some(im);
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
            rustix::event::poll(
                &mut fds,
                Some(&rustix::event::Timespec {
                    tv_sec: 0,
                    tv_nsec: 50_000_000,
                }),
            )?;
            if fds[0].revents().contains(rustix::event::PollFlags::IN) {
                let _ = guard.read();
            }
        }
        queue.dispatch_pending(&mut bridge)?;
        // Popup rectangles also arrive independently of input-method done:
        // moving a parent must update IBus without another text edit.
        bridge.sync_cursor();
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
        "DeleteSurroundingText" => Some(FromIbus::DeleteSurrounding {
            offset: params.child_value(0).get::<i32>()?,
            nchars: params.child_value(1).get::<u32>()?,
        }),
        _ => None,
    }
}

/// The string inside a serialized IBusText (`v` holding `(sa{sv}sv)`).
fn ibus_text(value: &glib::Variant) -> Option<String> {
    // as_variant calls g_variant_get_variant directly; its Option result does
    // not protect ordinary serialized tuples from GLib's type assertion.
    let mut inner = value.clone();
    while inner.is_type(glib::VariantTy::VARIANT) {
        inner = inner.as_variant()?;
    }
    inner.try_child_value(2)?.str().map(str::to_owned)
}

/// A serialized IBusText with no attributes, as libibus sends one:
/// `v` holding `("IBusText", a{sv} {}, s text, v ("IBusAttrList",
/// a{sv} {}, av []))`.
fn ibus_text_variant(text: &str) -> glib::Variant {
    let no_props = || glib::VariantDict::new(None).end();
    let attrs = glib::Variant::tuple_from_iter([
        "IBusAttrList".to_variant(),
        no_props(),
        glib::Variant::array_from_iter_with_type(
            glib::VariantTy::VARIANT,
            std::iter::empty::<glib::Variant>(),
        ),
    ]);
    glib::Variant::from_variant(&glib::Variant::tuple_from_iter([
        "IBusText".to_variant(),
        no_props(),
        text.to_variant(),
        glib::Variant::from_variant(&attrs),
    ]))
}

/// The largest char boundary at or before `byte` (clamped to the text).
fn floor_boundary(text: &str, byte: usize) -> usize {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

/// Characters before byte offset `byte` (IBus counts characters, the
/// protocol bytes).
fn char_offset(text: &str, byte: u32) -> u32 {
    text[..floor_boundary(text, byte as usize)].chars().count() as u32
}

/// Byte offset of character `chars` (the text's length past its end).
fn byte_offset(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(i, _)| i)
}

/// IBus's `DeleteSurroundingText(offset, nchars)`, in characters from
/// the cursor, as input-method-v2's `(before_length, after_length)` in
/// bytes. `None` when the range does not touch the cursor: the
/// protocol can only delete around it.
fn delete_lengths(surrounding: &Surrounding, offset: i32, nchars: u32) -> Option<(u32, u32)> {
    let text = surrounding.text.as_str();
    let cursor = floor_boundary(text, surrounding.cursor as usize);
    let cursor_chars = i64::from(char_offset(text, cursor as u32));
    let start_chars = (cursor_chars + i64::from(offset)).max(0);
    let end_chars = start_chars + i64::from(nchars);
    if start_chars > cursor_chars || end_chars < cursor_chars {
        return None;
    }
    let start = byte_offset(text, start_chars as usize);
    let end = byte_offset(text, end_chars as usize);
    Some(((cursor - start) as u32, (end - cursor) as u32))
}

/// text-input-v3's content type as IBus's (`SetContentType(purpose,
/// hints)`, GTK's input purposes and hints, as GNOME Shell passes).
fn ibus_content_type(hint: u32, purpose: u32) -> (u32, u32) {
    // Purposes normal..pin share numbers; date, time and datetime have
    // no IBus purpose; terminal is IBus's 10.
    let purpose = match purpose {
        0..=9 => purpose,
        13 => 10,
        _ => 0,
    };
    let hints = [
        (0x1, 4),     // completion -> WORD_COMPLETION
        (0x2, 1),     // spellcheck -> SPELLCHECK
        (0x4, 64),    // auto_capitalization -> UPPERCASE_SENTENCES
        (0x8, 8),     // lowercase -> LOWERCASE
        (0x10, 16),   // uppercase -> UPPERCASE_CHARS
        (0x20, 32),   // titlecase -> UPPERCASE_WORDS
        (0x80, 2048), // sensitive_data -> PRIVATE
    ]
    .into_iter()
    .filter(|(bit, _)| hint & bit != 0)
    .fold(0, |bits, (_, ibus)| bits | ibus);
    (purpose, hints)
}

/// The bridge's Wayland side.
#[derive(Default)]
struct Bridge {
    seat: Option<wl_seat::WlSeat>,
    im_manager: Option<ZwpInputMethodManagerV2>,
    vk_manager: Option<ZwpVirtualKeyboardManagerV1>,
    compositor: Option<wl_compositor::WlCompositor>,
    im: Option<ZwpInputMethodV2>,
    /// The input popup (never given a buffer) that carries the text
    /// cursor's rectangle, and its surface.
    _popup: Option<(wl_surface::WlSurface, ZwpInputPopupSurfaceV2)>,
    vk: Option<ZwpVirtualKeyboardV1>,
    grab: Option<ZwpInputMethodKeyboardGrabV2>,
    context: Option<IbusContext>,
    incoming: Rc<RefCell<VecDeque<FromIbus>>>,
    /// `done` events so far: the serial a commit names.
    serial: u32,
    /// Activation pending the next `done`, and the applied one.
    pending_active: Option<bool>,
    active: bool,
    /// Surrounding text and content type pending the next `done`, and
    /// the applied surrounding text.
    pending_surrounding: Option<Surrounding>,
    surrounding: Option<Surrounding>,
    pending_content_type: Option<(u32, u32)>,
    /// The latest cursor rectangle (global, see the module doc), and
    /// the one IBus last heard.
    cursor_rect: Option<(i32, i32, i32, i32)>,
    sent_cursor_rect: Option<(i32, i32, i32, i32)>,
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
        let handled = self.active
            && self
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
                    let byte = byte_offset(&text, cursor as usize) as i32;
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
            FromIbus::DeleteSurrounding { offset, nchars } => {
                let lengths = self
                    .surrounding
                    .as_ref()
                    .and_then(|s| delete_lengths(s, offset, nchars));
                match lengths {
                    Some((before, after)) => {
                        im.delete_surrounding_text(before, after);
                        im.commit(self.serial);
                    }
                    None => eprintln!(
                        "roost-ibus-bridge: cannot delete {nchars} characters at {offset} from the cursor"
                    ),
                }
            }
        }
    }

    /// A `done`: apply activation, then tell IBus about the field (its
    /// content type, surrounding text and cursor), as GNOME Shell does.
    fn done(&mut self, qh: &QueueHandle<Self>) {
        self.serial = self.serial.wrapping_add(1);
        if let Some(active) = self.pending_active.take() {
            self.activate(active, qh);
        }
        if !self.active {
            return;
        }
        let Some(context) = &self.context else {
            return;
        };
        if let Some((purpose, hints)) = self.pending_content_type.take() {
            context.send("SetContentType", Some((purpose, hints).to_variant()));
        }
        if let Some(surrounding) = self.pending_surrounding.take() {
            if self.surrounding.as_ref() != Some(&surrounding) {
                let text = &surrounding.text;
                let args = glib::Variant::tuple_from_iter([
                    ibus_text_variant(text),
                    char_offset(text, surrounding.cursor).to_variant(),
                    char_offset(text, surrounding.anchor).to_variant(),
                ]);
                context.send("SetSurroundingText", Some(args));
                self.surrounding = Some(surrounding);
            }
        }
        self.sync_cursor();
    }

    /// Forward independent popup placement updates after the event batch
    /// has applied any pending focus transition.
    fn sync_cursor(&mut self) {
        if !self.active {
            return;
        }
        let Some(context) = &self.context else {
            return;
        };
        if let Some(rect) = self
            .cursor_rect
            .filter(|r| Some(*r) != self.sent_cursor_rect)
        {
            context.send("SetCursorLocation", Some(rect.to_variant()));
            if std::env::var_os("ROOST_IBUS_DEBUG").is_some() {
                eprintln!("roost-ibus-bridge: cursor at {rect:?}");
            }
            self.sent_cursor_rect = Some(rect);
        }
    }

    /// Focus the IBus context while retaining one stable keyboard grab.
    /// A field-to-field Tab transition must not destroy/recreate the grab:
    /// physical keys can arrive between those requests. Inactive contexts
    /// forward keys directly instead of sending them through IBus.
    fn activate(&mut self, active: bool, qh: &QueueHandle<Self>) {
        if active == self.active {
            return;
        }
        self.active = active;
        // A new field: what IBus heard about the last one is stale.
        self.surrounding = None;
        self.sent_cursor_rect = None;
        eprintln!(
            "roost-ibus-bridge: text field {}",
            if active { "focused" } else { "left" }
        );
        if let Some(context) = &self.context {
            context.send(if active { "FocusIn" } else { "FocusOut" }, None);
        }
        // Every focused field goes through IBus, including keyboard layouts.
        // Retain the supervised connection's grab across field changes and
        // periods without a field; key() bypasses IBus while inactive.
        // Connection teardown releases the grab if the bridge exits.
        if active && self.grab.is_none() {
            self.grab = self.im.as_ref().map(|im| im.grab_keyboard(qh, ()));
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
                "wl_compositor" if state.compositor.is_none() => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
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
            zwp_input_method_v2::Event::Activate => {
                // Activation resets the field's state (the protocol's
                // rule): only what follows before `done` applies.
                state.pending_active = Some(true);
                state.pending_surrounding = None;
                state.pending_content_type = Some((0, 0));
            }
            zwp_input_method_v2::Event::Deactivate => state.pending_active = Some(false),
            zwp_input_method_v2::Event::SurroundingText {
                text,
                cursor,
                anchor,
            } => {
                state.pending_surrounding = Some(Surrounding {
                    text,
                    cursor,
                    anchor,
                });
            }
            zwp_input_method_v2::Event::ContentType { hint, purpose } => {
                let hint = match hint {
                    WEnum::Value(hint) => hint.bits(),
                    WEnum::Unknown(bits) => bits,
                };
                let purpose = match purpose {
                    WEnum::Value(purpose) => purpose as u32,
                    WEnum::Unknown(value) => value,
                };
                state.pending_content_type = Some(ibus_content_type(hint, purpose));
            }
            zwp_input_method_v2::Event::Done => state.done(qh),
            zwp_input_method_v2::Event::Unavailable => {
                eprintln!("roost-ibus-bridge: another input method is active");
                state.gone = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for Bridge {
    fn event(
        state: &mut Self,
        _: &ZwpInputPopupSurfaceV2,
        event: zwp_input_popup_surface_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_popup_surface_v2::Event::TextInputRectangle {
            x,
            y,
            width,
            height,
        } = event
        {
            state.cursor_rect = Some((x, y, width, height));
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
wayland_client::delegate_noop!(Bridge: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(Bridge: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(Bridge: ignore ZwpInputMethodManagerV2);
wayland_client::delegate_noop!(Bridge: ignore ZwpVirtualKeyboardManagerV1);
wayland_client::delegate_noop!(Bridge: ignore ZwpVirtualKeyboardV1);

#[cfg(test)]
mod tests {
    use super::*;

    fn around(text: &str, cursor: u32) -> Surrounding {
        Surrounding {
            text: text.to_owned(),
            cursor,
            anchor: cursor,
        }
    }

    #[test]
    fn surrounding_offsets_count_characters_not_bytes() {
        // 你 and 好 are three bytes each.
        assert_eq!(char_offset("你好", 6), 2);
        assert_eq!(char_offset("你好", 3), 1);
        assert_eq!(char_offset("a你b", 4), 2);
        // Inside a character, or past the end: clamped.
        assert_eq!(char_offset("你好", 4), 1);
        assert_eq!(char_offset("你好", 99), 2);
        assert_eq!(byte_offset("你好", 1), 3);
        assert_eq!(byte_offset("你好", 2), 6);
        assert_eq!(byte_offset("你好", 5), 6);
    }

    #[test]
    fn ibus_deletions_become_byte_lengths_around_the_cursor() {
        // One character before the cursor: 好, three bytes.
        assert_eq!(delete_lengths(&around("你好", 6), -1, 1), Some((3, 0)));
        // Across the cursor: 你 before, b after.
        assert_eq!(delete_lengths(&around("a你b", 4), -1, 2), Some((3, 1)));
        // After the cursor only.
        assert_eq!(delete_lengths(&around("a你b", 1), 0, 1), Some((0, 3)));
        // Clamped to the text.
        assert_eq!(delete_lengths(&around("你", 3), -5, 9), Some((3, 0)));
        // Not touching the cursor: the protocol cannot say it.
        assert_eq!(delete_lengths(&around("abcd", 2), 1, 1), None);
        assert_eq!(delete_lengths(&around("abcd", 2), -2, 1), None);
    }

    #[test]
    fn surrounding_text_is_packed_as_an_ibus_text() {
        let packed = ibus_text_variant("你好");
        assert_eq!(packed.type_().as_str(), "v");
        let inner = packed.as_variant().unwrap();
        assert_eq!(inner.type_().as_str(), "(sa{sv}sv)");
        assert_eq!(inner.child_value(0).str(), Some("IBusText"));
        let attrs = inner.child_value(3).as_variant().unwrap();
        assert_eq!(attrs.type_().as_str(), "(sa{sv}av)");
        assert_eq!(attrs.child_value(0).str(), Some("IBusAttrList"));
        assert_eq!(attrs.child_value(2).n_children(), 0);
        // What the bridge reads back from IBus's own signals.
        assert_eq!(ibus_text(&packed).as_deref(), Some("你好"));
        let args = glib::Variant::tuple_from_iter([packed, 2u32.to_variant(), 2u32.to_variant()]);
        assert_eq!(args.type_().as_str(), "(vuu)");
    }

    #[test]
    fn raw_and_nested_text_variants_decode_without_native_type_assertions() {
        let boxed = ibus_text_variant("你好");
        assert_eq!(
            ibus_text(&boxed.as_variant().unwrap()).as_deref(),
            Some("你好")
        );
        assert_eq!(
            ibus_text(&glib::Variant::from_variant(&boxed)).as_deref(),
            Some("你好")
        );
        assert_eq!(ibus_text(&"not a container".to_variant()), None);
        assert_eq!(ibus_text(&42u32.to_variant()), None);
    }

    #[test]
    fn content_types_map_to_ibus_purposes_and_hints() {
        // Normal text with spellcheck and completion.
        assert_eq!(ibus_content_type(0x1 | 0x2, 0), (0, 4 | 1));
        // A password field: purpose 8, sensitive data -> PRIVATE.
        assert_eq!(ibus_content_type(0x80 | 0x40, 8), (8, 2048));
        // Terminal, and date (no IBus purpose).
        assert_eq!(ibus_content_type(0, 13), (10, 0));
        assert_eq!(ibus_content_type(0, 10), (0, 0));
    }
}
