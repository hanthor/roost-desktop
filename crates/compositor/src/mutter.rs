//! GNOME's Mutter D-Bus interfaces for screen sharing (#61).
//!
//! xdg-desktop-portal-gnome shares screens by asking Mutter, over D-Bus:
//! `org.gnome.Mutter.DisplayConfig` lists the monitors for its picker,
//! and `org.gnome.Mutter.ScreenCast` hands back a PipeWire node per
//! stream. Roost serves both, so browsers and video calls share screens
//! through the stock GNOME portal. Adapted from niri's
//! `src/dbus/mutter_screen_cast.rs` and `mutter_display_config.rs`
//! (GPL-3.0-or-later, like Roost): monitor, area and window streams. Window
//! ids are the ones `org.gnome.Shell.Introspect` lists (`introspect.rs`).
//!
//! The D-Bus side runs on its own thread; casts start and stop on the
//! compositor's event loop (see `screencast.rs`). When a real GNOME
//! session owns these names (the nested preview), the service stays off.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::zvariant::{DeserializeDict, OwnedObjectPath, OwnedValue, SerializeDict, Type, Value};
use zbus::{fdo, interface, ObjectServer};

pub(crate) mod remote_desktop;
mod remote_eis;
#[cfg(test)]
mod service_channel_tests;
pub use remote_desktop::Input as RemoteInput;

/// One lit output as the D-Bus side describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSnapshot {
    pub connector: String,
    pub make: String,
    pub model: String,
    /// Actual EDID serial text/number, empty when EDID is unavailable.
    pub serial: String,
    /// Mode in physical pixels, refresh in mHz.
    pub width: i32,
    pub height: i32,
    pub refresh_mhz: i32,
    /// Logical position and scale.
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub primary: bool,
}

/// The runtime keeps this current; D-Bus calls read it.
pub type Outputs = Arc<Mutex<Vec<OutputSnapshot>>>;

/// Serial and change notification are published under the SAME snapshot lock.
/// Capture consumers keep the existing Outputs type; display readers never
/// observe a new serial paired with an older (or merely requested) inventory.
#[derive(Clone)]
pub struct DisplayPublication {
    outputs: Outputs,
    serial: Arc<AtomicU32>,
    changed: Arc<AtomicBool>,
}
impl DisplayPublication {
    pub fn new(outputs: Outputs) -> Self {
        Self {
            outputs,
            serial: Arc::new(AtomicU32::new(1)),
            changed: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn current(&self) -> fdo::Result<(u32, Vec<OutputSnapshot>)> {
        let outputs = self
            .outputs
            .lock()
            .map_err(|_| fdo::Error::Failed("display snapshot unavailable".into()))?;
        let serial = self.serial.load(Ordering::SeqCst);
        if serial == 0 {
            return Err(fdo::Error::Failed("display serial exhausted".into()));
        }
        Ok((serial, outputs.clone()))
    }
    pub fn publish(&self, snapshot: Vec<OutputSnapshot>) -> Result<bool, &'static str> {
        let mut outputs = self
            .outputs
            .lock()
            .map_err(|_| "display snapshot unavailable")?;
        if *outputs == snapshot {
            return Ok(false);
        }
        *outputs = snapshot;
        let previous = self.serial.load(Ordering::SeqCst);
        let next = previous.checked_add(1).filter(|_| previous != 0);
        self.serial.store(next.unwrap_or(0), Ordering::SeqCst);
        self.changed.store(true, Ordering::SeqCst);
        next.map(|_| true).ok_or("display serial exhausted")
    }
    pub fn requested_current(&self, serial: u32) -> bool {
        self.current().is_ok_and(|(current, _)| current == serial)
    }
    pub fn completion_admitted(
        &self,
        serial: u32,
        deadline: std::time::Instant,
        now: std::time::Instant,
        reply: &async_channel::Sender<Result<(), &'static str>>,
    ) -> bool {
        !reply.is_closed() && now <= deadline && self.requested_current(serial)
    }
    fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::SeqCst)
    }
}

/// What one stream shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CastTarget {
    /// A whole monitor, by connector name.
    Monitor(String),
    /// One window, by its Introspect id.
    Window(u64),
    /// Part of a monitor (`RecordArea`, what GNOME's screen recorder
    /// asks for): the monitor's connector and the area in its physical
    /// pixels, `(x, y, width, height)`.
    Area(String, (i32, i32, i32, i32)),
}

/// Where a `RecordArea` rectangle (global logical pixels) lands: the
/// output holding its top-left corner, and the area in that output's
/// physical pixels, clipped to it. `None` when no output holds it or
/// nothing of it is left.
pub fn area_on_output(
    outputs: &[OutputSnapshot],
    (x, y, width, height): (i32, i32, i32, i32),
) -> Option<(OutputSnapshot, (i32, i32, i32, i32))> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let output = outputs.iter().find(|o| {
        let w = (f64::from(o.width) / o.scale).round() as i32;
        let h = (f64::from(o.height) / o.scale).round() as i32;
        x >= o.x && y >= o.y && x < o.x + w && y < o.y + h
    })?;
    let px = |v: i32| (f64::from(v) * output.scale).round() as i32;
    let (left, top) = (px(x - output.x), px(y - output.y));
    let right = px(x - output.x + width).min(output.width);
    let bottom = px(y - output.y + height).min(output.height);
    (right > left && bottom > top)
        .then(|| (output.clone(), (left, top, right - left, bottom - top)))
}

/// A window as the D-Bus side describes it (Introspect, RecordWindow).
#[derive(Debug, Clone, PartialEq)]
pub struct WindowSnapshot {
    pub id: u64,
    pub title: String,
    /// Wayland app id or X11 class, as the client set it.
    pub app_id: Option<String>,
    /// Desktop application identity inherited from the ultimate transient
    /// parent, independently of this window's raw app ID / X11 class.
    pub application_id: Option<String>,
    /// Optional sandbox identity from the original live process, not app-id/class.
    pub sandboxed_app_id: Option<String>,
    /// Logical size of the window's visible geometry.
    pub width: i32,
    pub height: i32,
    pub focused: bool,
    /// On an inactive workspace.
    pub hidden: bool,
    /// X11 (through Xwayland) rather than native Wayland.
    pub x11: bool,
    /// Has no live transient parent; used for GNOME running-app eligibility.
    pub standalone: bool,
    /// Eligible for GNOME GetWindows enumeration, independently of app eligibility.
    pub introspect_eligible: bool,
}

/// The runtime keeps this current; Introspect and RecordWindow read it.
pub type Windows = Arc<Mutex<Vec<WindowSnapshot>>>;

/// Requests from D-Bus to the event loop.
pub enum ToLoop {
    RemoteInput {
        session_id: u64,
        grant: Arc<AtomicBool>,
        pending: Arc<std::sync::atomic::AtomicU32>,
        input: RemoteInput,
    },
    RemoteStop {
        session_id: u64,
    },
    RemoteKeymap {
        reply: std::sync::mpsc::Sender<String>,
    },
    StartCast {
        session_id: u64,
        grant: Arc<AtomicBool>,
        target: CastTarget,
        signal: SignalEmitter<'static>,
    },
    StopCast {
        session_id: u64,
    },
    /// GNOME Settings' Displays panel applied an arrangement; persist it
    /// when asked (method 2).
    ApplyMonitors {
        configs: Vec<crate::monitors::MonitorConfig>,
        persistent: bool,
        serial: u32,
        deadline: std::time::Instant,
        reply: async_channel::Sender<Result<(), &'static str>>,
    },
}

/// GNOME's backend requests this ordinary Wayland connection before owning
/// its portal bus name. Capture admission remains a separate interface.
struct ServiceChannel {
    display: smithay::reexports::wayland_server::DisplayHandle,
    authority: crate::capture_security::Authority,
    clients: HashMap<String, Vec<ServiceConnection>>,
}

struct ServiceConnection {
    alive: Arc<AtomicBool>,
    typed: bool,
}

fn connection_window_tag(options: &HashMap<String, OwnedValue>) -> fdo::Result<Option<String>> {
    // Mutter ignores unknown options and window-tag values of the wrong type.
    let tag = options
        .get("window-tag")
        .and_then(|value| <&str>::try_from(value).ok());
    if tag.is_some_and(|tag| tag.len() > 1024) {
        return Err(fdo::Error::InvalidArgs(
            "window tag exceeds 1024 bytes".into(),
        ));
    }
    Ok(tag.map(str::to_owned))
}

impl ServiceChannel {
    fn open_connection(
        &mut self,
        owner: String,
        typed: bool,
        window_tag: Option<String>,
        credentials: Option<Arc<fdo::ConnectionCredentials>>,
    ) -> fdo::Result<zbus::zvariant::OwnedFd> {
        self.clients.retain(|_, connections| {
            connections.retain(|connection| connection.alive.load(Ordering::SeqCst));
            !connections.is_empty()
        });
        let count: usize = self.clients.values().map(Vec::len).sum();
        let duplicate_provider = typed
            && self
                .clients
                .get(&owner)
                .is_some_and(|connections| connections.iter().any(|connection| connection.typed));
        if count >= 32 || duplicate_provider {
            return Err(fdo::Error::LimitsExceeded(
                "service connection limit".into(),
            ));
        }
        let (server, client) = std::os::unix::net::UnixStream::pair()
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let alive = Arc::new(AtomicBool::new(true));
        let mut client_data = crate::ClientState::service_connection(alive.clone(), window_tag);
        client_data.sandboxed_app_id = credentials
            .clone()
            .map(crate::sandbox_identity::discover_async)
            .unwrap_or_default();
        client_data.original_credentials = credentials;
        client_data.x11_interop = typed;
        self.display
            .insert_client(server, Arc::new(client_data))
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        self.clients
            .entry(owner)
            .or_default()
            .push(ServiceConnection { alive, typed });
        Ok(std::os::fd::OwnedFd::from(client).into())
    }
}

#[interface(name = "org.gnome.Mutter.ServiceChannel")]
impl ServiceChannel {
    async fn open_wayland_service_connection(
        &mut self,
        service_client_type: u32,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<zbus::zvariant::OwnedFd> {
        let role = crate::capture_security::ServiceClient::from_wire(service_client_type)?;
        let caller = self
            .authority
            .admit_service_connection(conn, &header, role)
            .await?;
        self.open_connection(caller.owner, true, None, Some(caller.credentials))
    }
    async fn open_wayland_connection(
        &mut self,
        options: HashMap<String, OwnedValue>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<zbus::zvariant::OwnedFd> {
        let caller = self.authority.admit_connection(conn, &header).await?;
        let tag = connection_window_tag(&options)?;
        self.open_connection(caller.owner, false, tag, Some(caller.credentials))
    }
}

// --- DisplayConfig -----------------------------------------------------

#[derive(Serialize, Type)]
struct Monitor {
    names: (String, String, String, String),
    modes: Vec<Mode>,
    properties: HashMap<String, OwnedValue>,
}

#[derive(Serialize, Type)]
struct Mode {
    id: String,
    width: i32,
    height: i32,
    refresh_rate: f64,
    preferred_scale: f64,
    supported_scales: Vec<f64>,
    properties: HashMap<String, OwnedValue>,
}

#[derive(Serialize, Type)]
struct LogicalMonitor {
    x: i32,
    y: i32,
    scale: f64,
    transform: u32,
    is_primary: bool,
    monitors: Vec<(String, String, String, String)>,
    properties: HashMap<String, OwnedValue>,
}

/// GNOME's built-in panel test (Mutter's), for "Built-in display".
fn is_laptop_panel(connector: &str) -> bool {
    ["eDP-", "LVDS-", "DSI-"]
        .iter()
        .any(|p| connector.starts_with(p))
}

#[derive(Deserialize, Type)]
struct LogicalMonitorConfiguration {
    x: i32,
    y: i32,
    scale: f64,
    transform: u32,
    is_primary: bool,
    monitors: Vec<(String, String, HashMap<String, OwnedValue>)>,
}

struct DisplayConfig {
    publication: DisplayPublication,
    to_loop: calloop::channel::Sender<ToLoop>,
}

#[interface(name = "org.gnome.Mutter.DisplayConfig")]
impl DisplayConfig {
    #[allow(clippy::type_complexity)]
    fn get_current_state(
        &self,
    ) -> fdo::Result<(
        u32,
        Vec<Monitor>,
        Vec<LogicalMonitor>,
        HashMap<String, OwnedValue>,
    )> {
        let (serial, outputs) = self.publication.current()?;
        let mut monitors = Vec::new();
        let mut logical = Vec::new();
        for o in outputs.iter() {
            let names = (
                o.connector.clone(),
                o.make.clone(),
                o.model.clone(),
                o.serial.clone(),
            );
            let builtin = is_laptop_panel(&o.connector);
            let display_name = if builtin {
                "Built-in display".to_owned()
            } else {
                o.model.clone()
            };
            let refresh = f64::from(o.refresh_mhz) / 1000.0;
            monitors.push(Monitor {
                names: names.clone(),
                modes: vec![Mode {
                    id: mode_id(o),
                    width: o.width,
                    height: o.height,
                    refresh_rate: refresh,
                    preferred_scale: o.scale,
                    supported_scales: vec![1.0, 1.25, 1.5, 1.75, 2.0],
                    properties: HashMap::from([
                        ("is-current".to_owned(), OwnedValue::from(true)),
                        ("is-preferred".to_owned(), OwnedValue::from(true)),
                    ]),
                }],
                properties: HashMap::from([
                    (
                        "display-name".to_owned(),
                        OwnedValue::try_from(Value::from(display_name)).expect("string value"),
                    ),
                    ("is-builtin".to_owned(), OwnedValue::from(builtin)),
                ]),
            });
            logical.push(LogicalMonitor {
                x: o.x,
                y: o.y,
                scale: o.scale,
                transform: 0,
                is_primary: o.primary,
                monitors: vec![names],
                properties: HashMap::new(),
            });
        }
        let properties = HashMap::from([(
            "layout-mode".to_owned(),
            // 1: logical layout (GNOME's default).
            OwnedValue::from(1u32),
        )]);
        Ok((serial, monitors, logical, properties))
    }

    /// GNOME Settings' Displays panel: 0 verifies, 1 applies, 2 applies
    /// and keeps (niri's checks: no mirroring, known connectors only).
    async fn apply_monitors_config(
        &self,
        serial: u32,
        method: u32,
        logical_monitor_configs: Vec<LogicalMonitorConfiguration>,
        _properties: HashMap<String, OwnedValue>,
    ) -> fdo::Result<()> {
        if method > 2 {
            return Err(fdo::Error::InvalidArgs(
                "invalid configuration method".into(),
            ));
        }
        let (current, outputs) = self.publication.current()?;
        if serial != current {
            return Err(fdo::Error::AccessDenied(
                "The requested configuration is based on stale information".into(),
            ));
        }
        let configs = validate(&outputs, &logical_monitor_configs)?;
        if method == 0 {
            return Ok(());
        }
        let (reply, receive) = async_channel::bounded(1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        self.to_loop
            .send(ToLoop::ApplyMonitors {
                configs,
                persistent: method == 2,
                serial,
                deadline,
                reply,
            })
            .map_err(|_| fdo::Error::Failed("compositor gone".into()))?;
        futures_lite::future::race(
            async {
                receive
                    .recv()
                    .await
                    .map_err(|_| "compositor completion unavailable")?
            },
            async {
                async_io::Timer::at(deadline).await;
                Err("compositor configuration deadline expired")
            },
        )
        .await
        .map_err(|message| fdo::Error::Failed(message.into()))
    }

    #[zbus(signal)]
    async fn monitors_changed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property)]
    fn apply_monitors_config_allowed(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn power_save_mode(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn panel_orientation_managed(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn night_light_supported(&self) -> bool {
        false
    }
}

/// The mode identifier advertised to GNOME Settings for this lit output.
fn mode_id(output: &OutputSnapshot) -> String {
    let refresh = f64::from(output.refresh_mhz) / 1000.0;
    format!("{}x{}@{refresh:.3}", output.width, output.height)
}

/// Check a requested arrangement against the lit outputs.
fn validate(
    outputs: &[OutputSnapshot],
    requested: &[LogicalMonitorConfiguration],
) -> fdo::Result<Vec<crate::monitors::MonitorConfig>> {
    let mut configs = Vec::new();
    if requested.len() > 64 {
        return Err(fdo::Error::InvalidArgs("too many logical monitors".into()));
    }
    for logical in requested {
        if logical.monitors.len() > 1 {
            return Err(fdo::Error::Failed("mirroring is not supported yet".into()));
        }
        if logical.transform != 0 {
            return Err(fdo::Error::Failed("rotation is not supported yet".into()));
        }
        if !(1.0..=4.0).contains(&logical.scale) {
            return Err(fdo::Error::Failed(format!(
                "scale {} out of range",
                logical.scale
            )));
        }
        for (connector, requested_mode, _props) in &logical.monitors {
            let output = outputs
                .iter()
                .find(|o| &o.connector == connector)
                .ok_or_else(|| fdo::Error::Failed(format!("connector '{connector}' not found")))?;
            // Mode changes are not implemented. Only accept the exact mode
            // we advertised, rather than silently ignoring the requested one.
            if requested_mode != &mode_id(output) {
                return Err(fdo::Error::Failed(format!(
                    "mode '{requested_mode}' is not supported on '{connector}'"
                )));
            }
            if configs
                .iter()
                .any(|config: &crate::monitors::MonitorConfig| &config.connector == connector)
            {
                return Err(fdo::Error::Failed(format!(
                    "connector '{connector}' configured more than once"
                )));
            }
            let width = (f64::from(output.width) / logical.scale).round() as i32;
            let height = (f64::from(output.height) / logical.scale).round() as i32;
            if width <= 0
                || height <= 0
                || logical.x.checked_add(width).is_none()
                || logical.y.checked_add(height).is_none()
            {
                return Err(fdo::Error::InvalidArgs("monitor geometry overflow".into()));
            }
            configs.push(crate::monitors::MonitorConfig {
                connector: connector.clone(),
                scale: logical.scale,
                x: logical.x,
                y: logical.y,
                primary: logical.is_primary,
            });
        }
    }
    if configs.is_empty() {
        return Err(fdo::Error::Failed(
            "at least one output must stay on".into(),
        ));
    }
    // The runtime rearranges lit outputs but cannot disable one yet.
    // GNOME expresses disabling by omitting the output from this list.
    if outputs.iter().any(|output| {
        !configs
            .iter()
            .any(|config| config.connector == output.connector)
    }) {
        return Err(fdo::Error::Failed(
            "disabling outputs is not supported yet".into(),
        ));
    }
    crate::monitors::requested_primary(&configs)
        .map_err(|message| fdo::Error::InvalidArgs(message.into()))?;
    Ok(configs)
}

// --- ScreenCast ----------------------------------------------------------

type Sessions = Arc<Mutex<Vec<(Session, OwnedObjectPath)>>>;

#[derive(Clone)]
struct ScreenCast {
    remote: remote_desktop::RemoteSessions,
    authority: crate::capture_security::Authority,
    sessions: Sessions,
    outputs: Outputs,
    windows: Windows,
    to_loop: calloop::channel::Sender<ToLoop>,
    next_id: Arc<AtomicU64>,
}

/// A session's streams with their D-Bus interface handles.
type SessionStreams = Arc<Mutex<Vec<(Stream, InterfaceRef<Stream>)>>>;

#[derive(Clone)]
struct Session {
    owner: String,
    authority: crate::capture_security::Authority,
    id: u64,
    outputs: Outputs,
    windows: Windows,
    to_loop: calloop::channel::Sender<ToLoop>,
    next_id: Arc<AtomicU64>,
    streams: SessionStreams,
    stopped: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
}

#[derive(Clone)]
enum Stream {
    Monitor(OutputSnapshot),
    Window(WindowSnapshot),
    /// The output, the requested logical area, and its physical pixels.
    Area(OutputSnapshot, (i32, i32, i32, i32), (i32, i32, i32, i32)),
}

impl Stream {
    fn target(&self) -> CastTarget {
        match self {
            Stream::Monitor(output) => CastTarget::Monitor(output.connector.clone()),
            Stream::Window(window) => CastTarget::Window(window.id),
            Stream::Area(output, _, physical) => {
                CastTarget::Area(output.connector.clone(), *physical)
            }
        }
    }
}

#[derive(Debug, DeserializeDict, Type)]
#[zvariant(signature = "dict")]
struct RecordWindowProperties {
    #[zvariant(rename = "window-id")]
    window_id: Option<u64>,
    #[zvariant(rename = "cursor-mode")]
    _cursor_mode: Option<u32>,
    #[zvariant(rename = "is-recording")]
    _is_recording: Option<bool>,
}

#[derive(Debug, DeserializeDict, Type)]
#[zvariant(signature = "dict")]
struct RecordMonitorProperties {
    #[zvariant(rename = "cursor-mode")]
    _cursor_mode: Option<u32>,
    #[zvariant(rename = "is-recording")]
    _is_recording: Option<bool>,
}

#[derive(Debug, SerializeDict, Type, Value)]
#[zvariant(signature = "dict")]
struct StreamParameters {
    /// Position in logical coordinates.
    position: (i32, i32),
    /// Size in logical coordinates.
    size: (i32, i32),
}

#[interface(name = "org.gnome.Mutter.ScreenCast")]
impl ScreenCast {
    async fn create_session(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        properties: HashMap<String, OwnedValue>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<OwnedObjectPath> {
        let owner = self.authority.admit(conn, &header).await?;
        if self
            .sessions
            .lock()
            .map_or(true, |sessions| sessions.len() >= 256)
        {
            return Err(fdo::Error::LimitsExceeded(
                "too many capture sessions".into(),
            ));
        }
        let linked = if let Some(value) = properties.get("remote-desktop-session-id") {
            let remote_id = <&str>::try_from(value)
                .map_err(|_| fdo::Error::InvalidArgs("invalid remote session id".into()))?;
            let remote = self
                .remote
                .lock()
                .map_err(|_| fdo::Error::Failed("remote registry unavailable".into()))?;
            let grant = remote
                .get(remote_id)
                .ok_or_else(|| crate::capture_security::denied("unknown remote session"))?;
            if grant.owner != owner
                || grant.stopped.load(Ordering::SeqCst)
                || grant.started.load(Ordering::SeqCst)
            {
                return Err(crate::capture_security::denied(
                    "remote grant cannot accept this stream",
                ));
            }
            Some(grant.stopped.clone())
        } else {
            None
        };
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let path = format!("/org/gnome/Mutter/ScreenCast/Session/u{id}");
        let path =
            OwnedObjectPath::try_from(path).map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let session = Session {
            owner,
            authority: self.authority.clone(),
            id,
            outputs: self.outputs.clone(),
            windows: self.windows.clone(),
            to_loop: self.to_loop.clone(),
            next_id: self.next_id.clone(),
            streams: Arc::new(Mutex::new(Vec::new())),
            stopped: linked.unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
            started: Arc::new(AtomicBool::new(false)),
        };
        server.at(&path, session.clone()).await?;
        self.sessions
            .lock()
            .map_err(|_| fdo::Error::Failed("session registry unavailable".into()))?
            .push((session, path.clone()));
        Ok(path)
    }

    #[zbus(property)]
    fn version(&self) -> i32 {
        4
    }
}

impl Session {
    fn start_streams(&self) -> fdo::Result<()> {
        self.authority.unlocked()?;
        if self.stopped.load(Ordering::SeqCst) {
            return Err(crate::capture_security::denied("session revoked"));
        }
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(fdo::Error::Failed("capture session already started".into()));
        }
        let streams = self.streams.lock().map(|s| s.clone()).unwrap_or_default();
        for (stream, iface) in streams {
            let _ = self.to_loop.send(ToLoop::StartCast {
                session_id: self.id,
                grant: self.stopped.clone(),
                target: stream.target(),
                signal: iface.signal_emitter().to_owned(),
            });
        }
        Ok(())
    }
}

#[interface(name = "org.gnome.Mutter.ScreenCast.Session")]
impl Session {
    async fn start(&self, #[zbus(header)] header: zbus::message::Header<'_>) -> fdo::Result<()> {
        self.check_owner(&header)?;
        self.start_streams()
    }

    async fn stop(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.check_owner(&header)?;
        if self.stopped.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let _ = Session::closed(&ctxt).await;
        let _ = self.to_loop.send(ToLoop::StopCast {
            session_id: self.id,
        });
        let streams = self
            .streams
            .lock()
            .map(|mut s| std::mem::take(&mut *s))
            .unwrap_or_default();
        for (_, iface) in streams {
            let _ = server
                .remove::<Stream, _>(iface.signal_emitter().path())
                .await;
        }
        let _ = server.remove::<Session, _>(ctxt.path()).await;
        Ok(())
    }

    async fn record_monitor(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(header)] header: zbus::message::Header<'_>,
        connector: &str,
        _properties: RecordMonitorProperties,
    ) -> fdo::Result<OwnedObjectPath> {
        self.check_owner(&header)?;
        self.authority.unlocked()?;
        let output = self
            .outputs
            .lock()
            .ok()
            .and_then(|o| o.iter().find(|o| o.connector == connector).cloned())
            .ok_or_else(|| fdo::Error::Failed("no such monitor".into()))?;
        self.add_stream(server, Stream::Monitor(output)).await
    }

    /// Part of the screen, in global logical pixels: what GNOME's screen
    /// recorder (screencastService.js) asks for, the whole monitor
    /// included.
    #[allow(clippy::too_many_arguments)] // fixed Mutter D-Bus signature plus authenticated caller
    async fn record_area(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        _properties: RecordMonitorProperties,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<OwnedObjectPath> {
        self.check_owner(&header)?;
        self.authority.unlocked()?;
        let outputs = self.outputs.lock().map(|o| o.clone()).unwrap_or_default();
        let (output, physical) = area_on_output(&outputs, (x, y, width, height))
            .ok_or_else(|| fdo::Error::Failed("the area is on no monitor".into()))?;
        self.add_stream(
            server,
            Stream::Area(output, (x, y, width, height), physical),
        )
        .await
    }

    /// GNOME's window share: the portal's picker lists windows from
    /// `org.gnome.Shell.Introspect.GetWindows` and passes the chosen id.
    async fn record_window(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        properties: RecordWindowProperties,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<OwnedObjectPath> {
        self.check_owner(&header)?;
        self.authority.unlocked()?;
        let id = properties
            .window_id
            .ok_or_else(|| fdo::Error::InvalidArgs("window-id is required".into()))?;
        let window = self
            .windows
            .lock()
            .ok()
            .and_then(|w| w.iter().find(|w| w.id == id).cloned())
            .ok_or_else(|| fdo::Error::Failed("no such window".into()))?;
        self.add_stream(server, Stream::Window(window)).await
    }

    #[zbus(signal)]
    async fn closed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;
}

impl Session {
    fn check_owner(&self, header: &zbus::message::Header<'_>) -> fdo::Result<()> {
        if crate::capture_security::owns_session(
            &self.owner,
            header.sender().map(|sender| sender.as_str()),
            self.stopped.load(Ordering::SeqCst),
        ) {
            Ok(())
        } else {
            Err(crate::capture_security::denied(
                "session belongs to another caller or was revoked",
            ))
        }
    }
    async fn add_stream(
        &self,
        server: &ObjectServer,
        stream: Stream,
    ) -> fdo::Result<OwnedObjectPath> {
        if self.started.load(Ordering::SeqCst) {
            return Err(fdo::Error::Failed("capture session already started".into()));
        }
        if self
            .streams
            .lock()
            .map_or(true, |streams| streams.len() >= 64)
        {
            return Err(fdo::Error::LimitsExceeded(
                "too many capture streams".into(),
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let path = format!("/org/gnome/Mutter/ScreenCast/Stream/u{id}");
        let path =
            OwnedObjectPath::try_from(path).map_err(|e| fdo::Error::Failed(e.to_string()))?;
        server.at(&path, stream.clone()).await?;
        let iface = server.interface::<_, Stream>(&path).await?;
        if let Ok(mut streams) = self.streams.lock() {
            streams.push((stream, iface));
        }
        Ok(path)
    }
}

#[interface(name = "org.gnome.Mutter.ScreenCast.Stream")]
impl Stream {
    #[zbus(signal)]
    pub async fn pipe_wire_stream_added(ctxt: &SignalEmitter<'_>, node_id: u32)
        -> zbus::Result<()>;

    #[zbus(property)]
    fn parameters(&self) -> StreamParameters {
        match self {
            Stream::Monitor(o) => StreamParameters {
                position: (o.x, o.y),
                size: (
                    (f64::from(o.width) / o.scale).round() as i32,
                    (f64::from(o.height) / o.scale).round() as i32,
                ),
            },
            // A window has no place on a monitor layout.
            Stream::Window(w) => StreamParameters {
                position: (0, 0),
                size: (w.width.max(1), w.height.max(1)),
            },
            Stream::Area(_, (x, y, w, h), _) => StreamParameters {
                position: (*x, *y),
                size: (*w, *h),
            },
        }
    }
}

/// Announce a stream's PipeWire node (from the event loop).
pub fn stream_added(signal: &SignalEmitter<'static>, node_id: u32) {
    let _ = zbus::block_on(Stream::pipe_wire_stream_added(signal, node_id));
}

/// Tell a session's client it ended (from the event loop). `stream` is
/// any of its streams' emitters; the signal goes out on the session.
pub fn session_closed(stream: &SignalEmitter<'static>, session_id: u64) {
    let path = format!("/org/gnome/Mutter/ScreenCast/Session/u{session_id}");
    if let Ok(session) = SignalEmitter::new(stream.connection(), path) {
        let _ = zbus::block_on(Session::closed(&session));
    }
}

/// Serve DisplayConfig and ScreenCast on the session bus from a thread.
/// Returns the channel the event loop reads cast requests from.
pub fn start(
    outputs: Outputs,
    publication: DisplayPublication,
    windows: Windows,
    authority: crate::capture_security::Authority,
    display: smithay::reexports::wayland_server::DisplayHandle,
) -> calloop::channel::Channel<ToLoop> {
    let (to_loop, from_dbus) = calloop::channel::channel();
    let _ = std::thread::Builder::new()
        .name("roost-mutter-dbus".into())
        .spawn(move || {
            let display_config = DisplayConfig {
                publication: publication.clone(),
                to_loop: to_loop.clone(),
            };
            let sessions: Sessions = Default::default();
            let remote: remote_desktop::RemoteSessions = Default::default();
            let next_id = Arc::new(AtomicU64::new(1));
            let screencast = ScreenCast {
                remote: remote.clone(),
                sessions: sessions.clone(),
                authority: authority.clone(),
                outputs: outputs.clone(),
                windows,
                to_loop: to_loop.clone(),
                next_id: next_id.clone(),
            };
            let remote_desktop = remote_desktop::RemoteDesktop {
                authority: authority.clone(),
                remote: remote.clone(),
                captures: sessions.clone(),
                outputs: outputs.clone(),
                to_loop: to_loop.clone(),
                next_id,
            };
            let service_channel = ServiceChannel {
                display,
                authority: authority.clone(),
                clients: HashMap::new(),
            };
            let conn = match zbus::blocking::connection::Builder::session()
                .and_then(|b| b.serve_at("/org/gnome/Mutter/ScreenCast", screencast))
                .and_then(|b| b.serve_at("/org/gnome/Mutter/DisplayConfig", display_config))
                .and_then(|b| b.serve_at("/org/gnome/Mutter/RemoteDesktop", remote_desktop))
                .and_then(|b| b.serve_at("/org/gnome/Mutter/ServiceChannel", service_channel))
                .and_then(|b| b.build())
            {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("roost-compositor: mutter D-Bus: no session bus: {e}");
                    return;
                }
            };
            let flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
            for name in [
                "org.gnome.Mutter.DisplayConfig",
                "org.gnome.Mutter.ScreenCast",
                "org.gnome.Mutter.RemoteDesktop",
                "org.gnome.Mutter.ServiceChannel",
            ] {
                match conn.request_name_with_flags(name, flags) {
                    Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                        eprintln!("roost-compositor: serving {name}");
                    }
                    _ => {
                        eprintln!("roost-compositor: {name} is taken; screen sharing off");
                        return;
                    }
                }
            }
            // Revoke on lock, unique-name disconnect or loss of the trusted
            // service identity. 100ms admission scan plus one compositor tick
            // bounds stream teardown; frames additionally fail closed on lock.
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if publication.take_changed() {
                    if let Ok(signal) =
                        SignalEmitter::new(conn.inner(), "/org/gnome/Mutter/DisplayConfig")
                    {
                        if zbus::block_on(DisplayConfig::monitors_changed(&signal)).is_err() {
                            publication.changed.store(true, Ordering::SeqCst);
                        }
                    }
                }
                let remote_snapshot = remote.lock().map(|s| s.clone()).unwrap_or_default();
                for (remote_id, grant) in remote_snapshot {
                    let alive = zbus::block_on(authority.owner_alive(conn.inner(), &grant.owner));
                    if alive && !grant.stopped.load(Ordering::SeqCst) {
                        continue;
                    }
                    grant.stopped.store(true, Ordering::SeqCst);
                    let _ = to_loop.send(ToLoop::RemoteStop {
                        session_id: grant.id,
                    });
                    let path = format!("/org/gnome/Mutter/RemoteDesktop/Session/u{}", grant.id);
                    if let Ok(signal) = SignalEmitter::new(conn.inner(), path.clone()) {
                        let _ = zbus::block_on(remote_desktop::RemoteSession::closed(&signal));
                    }
                    let _ = conn
                        .object_server()
                        .remove::<remote_desktop::RemoteSession, _>(path.as_str());
                    if let Ok(mut remote) = remote.lock() {
                        remote.remove(&remote_id);
                    }
                }
                let snapshot = sessions.lock().map(|s| s.clone()).unwrap_or_default();
                for (session, path) in snapshot {
                    let alive = zbus::block_on(authority.owner_alive(conn.inner(), &session.owner));
                    if alive && !session.stopped.load(Ordering::SeqCst) {
                        continue;
                    }
                    session.stopped.store(true, Ordering::SeqCst);
                    let _ = to_loop.send(ToLoop::StopCast {
                        session_id: session.id,
                    });
                    if let Ok(signal) = SignalEmitter::new(conn.inner(), path.clone()) {
                        let _ = zbus::block_on(Session::closed(&signal));
                    }
                    let streams = session
                        .streams
                        .lock()
                        .map(|mut s| std::mem::take(&mut *s))
                        .unwrap_or_default();
                    for (_, iface) in streams {
                        let _ = conn
                            .object_server()
                            .remove::<Stream, _>(iface.signal_emitter().path());
                    }
                    let _ = conn.object_server().remove::<Session, _>(&path);
                    if let Ok(mut sessions) = sessions.lock() {
                        sessions.retain(|(s, _)| s.id != session.id);
                    }
                }
            }
        });
    from_dbus
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(connector: &str) -> OutputSnapshot {
        OutputSnapshot {
            connector: connector.into(),
            make: String::new(),
            model: String::new(),
            serial: String::new(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            x: 0,
            y: 0,
            scale: 1.0,
            primary: true,
        }
    }

    #[test]
    fn current_state_exports_actual_serial_and_never_substitutes_connector() {
        let mut output = snapshot("eDP-1");
        output.make = "DEL".into();
        output.model = "Panel".into();
        output.serial = "S-123".into();
        let outputs = Arc::new(Mutex::new(vec![output]));
        let (to_loop, _receiver) = calloop::channel::channel();
        let config = DisplayConfig {
            publication: DisplayPublication::new(outputs.clone()),
            to_loop,
        };
        let (_, monitors, _, _) = config.get_current_state().unwrap();
        assert_eq!(
            monitors[0].names,
            ("eDP-1".into(), "DEL".into(), "Panel".into(), "S-123".into())
        );
        outputs.lock().unwrap()[0].serial.clear();
        let (_, monitors, _, _) = config.get_current_state().unwrap();
        assert_eq!(monitors[0].names.3, "");
    }

    fn logical(connector: &str, scale: f64) -> LogicalMonitorConfiguration {
        LogicalMonitorConfiguration {
            x: 0,
            y: 0,
            scale,
            transform: 0,
            is_primary: connector == "eDP-1",
            monitors: vec![(connector.into(), "1920x1080@60.000".into(), HashMap::new())],
        }
    }

    #[test]
    fn display_settings_do_not_acknowledge_ignored_modes_or_disabled_outputs() {
        let outputs = [snapshot("eDP-1"), snapshot("HDMI-A-1")];
        let valid = || vec![logical("eDP-1", 1.5), logical("HDMI-A-1", 1.0)];
        assert_eq!(validate(&outputs, &valid()).unwrap().len(), 2);
        for mode in ["", "1920x1080@59.940", "1280x720@60.000"] {
            let mut requested = valid();
            requested[0].monitors[0].1 = mode.into();
            assert!(
                validate(&outputs, &requested).is_err(),
                "unsupported mode {mode:?}"
            );
        }
        assert!(
            validate(&outputs, &valid()[..1]).is_err(),
            "omitting an output requests disabling it"
        );
        let mut duplicate = valid();
        duplicate.push(logical("eDP-1", 2.0));
        assert!(validate(&outputs, &duplicate).is_err(), "duplicate output");

        let mut fractional_refresh = snapshot("eDP-1");
        fractional_refresh.refresh_mhz = 59_940;
        let mut request = logical("eDP-1", 1.0);
        request.monitors[0].1 = "1920x1080@59.940".into();
        assert!(
            validate(&[fractional_refresh], &[request]).is_ok(),
            "advertised fractional refresh remains valid"
        );
    }

    #[test]
    fn display_settings_requests_are_checked() {
        let outputs = [snapshot("eDP-1")];
        let ok = validate(&outputs, &[logical("eDP-1", 1.5)]).unwrap();
        assert_eq!((ok[0].connector.as_str(), ok[0].scale), ("eDP-1", 1.5));
        assert!(
            validate(&outputs, &[logical("DP-9", 1.0)]).is_err(),
            "unknown connector"
        );
        assert!(
            validate(&outputs, &[logical("eDP-1", 0.5)]).is_err(),
            "scale range"
        );
        assert!(validate(&outputs, &[]).is_err(), "all off");
        let mut mirrored = logical("eDP-1", 1.0);
        mirrored
            .monitors
            .push(("HDMI-A-1".into(), String::new(), HashMap::new()));
        assert!(validate(&outputs, &[mirrored]).is_err(), "mirroring");
    }

    #[test]
    fn record_areas_land_on_their_monitor() {
        let mut right = snapshot("HDMI-A-1");
        right.x = 1920;
        right.scale = 2.0;
        let outputs = [snapshot("eDP-1"), right];
        let (output, area) = area_on_output(&outputs, (480, 300, 320, 200)).unwrap();
        assert_eq!(
            (output.connector.as_str(), area),
            ("eDP-1", (480, 300, 320, 200))
        );
        // The second monitor at scale 2 (960x540 logical): physical pixels.
        let (output, area) = area_on_output(&outputs, (1920 + 10, 20, 100, 50)).unwrap();
        assert_eq!(
            (output.connector.as_str(), area),
            ("HDMI-A-1", (20, 40, 200, 100))
        );
        // Clipped to the monitor it starts on.
        let (_, area) = area_on_output(&outputs, (1800, 1000, 400, 400)).unwrap();
        assert_eq!(area, (1800, 1000, 120, 80));
        assert!(area_on_output(&outputs, (-50, 0, 10, 10)).is_none());
        assert!(area_on_output(&outputs, (0, 0, 0, 10)).is_none());
    }

    #[test]
    fn builtin_panels_follow_mutter() {
        assert!(is_laptop_panel("eDP-1"));
        assert!(is_laptop_panel("LVDS-1"));
        assert!(!is_laptop_panel("HDMI-A-1"));
        assert!(!is_laptop_panel("roost-0"));
    }
}

#[cfg(test)]
mod display_publication_tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn snapshot() -> OutputSnapshot {
        OutputSnapshot {
            connector: "DP-1".into(),
            make: "DEL".into(),
            model: "Panel".into(),
            serial: "S-1".into(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60000,
            x: 0,
            y: 0,
            scale: 1.0,
            primary: true,
        }
    }
    #[test]
    fn serial_snapshot_changed_and_unchanged_publications_are_coherent() {
        let outputs: Outputs = Default::default();
        let publication = DisplayPublication::new(outputs.clone());
        assert_eq!(publication.current().unwrap(), (1, vec![]));
        assert!(!publication.take_changed());
        let output = snapshot();
        assert!(publication.publish(vec![output.clone()]).unwrap());
        assert_eq!(publication.current().unwrap(), (2, vec![output.clone()]));
        assert!(!publication.requested_current(1));
        assert!(publication.requested_current(2));
        assert_eq!(*outputs.lock().unwrap(), vec![output.clone()]);
        assert!(publication.take_changed());
        assert!(!publication.publish(vec![output]).unwrap());
        assert!(!publication.take_changed());
        assert_eq!(publication.current().unwrap().0, 2);
        // A real failed/unavailable backend publishes empty, never old active
        // outputs under a success serial, and still notifies actual change.
        assert!(publication.publish(vec![]).unwrap());
        assert_eq!(publication.current().unwrap(), (3, vec![]));
        assert!(publication.take_changed());
    }
    #[test]
    fn serial_never_wraps_and_actual_failure_snapshot_remains_retained() {
        let outputs: Outputs = Default::default();
        let publication = DisplayPublication::new(outputs.clone());
        publication.serial.store(u32::MAX, Ordering::SeqCst);
        assert!(publication.publish(vec![snapshot()]).is_err());
        assert_eq!(outputs.lock().unwrap().len(), 1);
        assert!(publication.current().is_err());
        assert!(!publication.requested_current(0));
        assert!(!publication.requested_current(u32::MAX));
        assert!(publication.take_changed());
        assert!(publication.publish(vec![]).is_err());
        assert!(outputs.lock().unwrap().is_empty());
    }
    #[test]
    fn original_completion_gate_refuses_stale_expired_and_gone_receiver() {
        let publication = DisplayPublication::new(Default::default());
        let now = Instant::now();
        let deadline = now + Duration::from_secs(2);
        let (reply, receive) = async_channel::bounded(1);
        assert!(publication.completion_admitted(1, deadline, deadline, &reply));
        assert!(!publication.completion_admitted(
            1,
            deadline,
            deadline + Duration::from_nanos(1),
            &reply
        ));
        publication.publish(vec![snapshot()]).unwrap();
        assert!(!publication.completion_admitted(1, deadline, now, &reply));
        assert!(publication.completion_admitted(2, deadline, now, &reply));
        drop(receive);
        assert!(!publication.completion_admitted(2, deadline, now, &reply));
    }
    #[test]
    fn real_snapshot_poison_never_qualifies_original_serial_or_change() {
        let outputs: Outputs = Default::default();
        let original = outputs.clone();
        let _ = std::thread::spawn(move || {
            let _guard = original.lock().unwrap();
            panic!("controlled poison");
        })
        .join();
        let publication = DisplayPublication::new(outputs);
        assert!(publication.current().is_err());
        assert!(publication.publish(vec![snapshot()]).is_err());
        assert!(!publication.requested_current(1));
    }
}
