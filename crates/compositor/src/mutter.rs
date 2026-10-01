//! GNOME's Mutter D-Bus interfaces for screen sharing (#61).
//!
//! xdg-desktop-portal-gnome shares screens by asking Mutter, over D-Bus:
//! `org.gnome.Mutter.DisplayConfig` lists the monitors for its picker,
//! and `org.gnome.Mutter.ScreenCast` hands back a PipeWire node per
//! stream. Roost serves both, so browsers and video calls share screens
//! through the stock GNOME portal. Adapted from niri's
//! `src/dbus/mutter_screen_cast.rs` and `mutter_display_config.rs`
//! (GPL-3.0-or-later, like Roost): monitor streams only for now.
//!
//! The D-Bus side runs on its own thread; casts start and stop on the
//! compositor's event loop (see `screencast.rs`). When a real GNOME
//! session owns these names (the nested preview), the service stays off.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::zvariant::{DeserializeDict, OwnedObjectPath, OwnedValue, SerializeDict, Type, Value};
use zbus::{fdo, interface, ObjectServer};

/// One lit output as the D-Bus side describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSnapshot {
    pub connector: String,
    pub make: String,
    pub model: String,
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

/// Requests from D-Bus to the event loop.
pub enum ToLoop {
    StartCast {
        session_id: u64,
        connector: String,
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
    },
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
    outputs: Outputs,
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
        let outputs = self
            .outputs
            .lock()
            .map_err(|_| fdo::Error::Failed("poisoned".into()))?;
        let mut monitors = Vec::new();
        let mut logical = Vec::new();
        for o in outputs.iter() {
            let names = (
                o.connector.clone(),
                o.make.clone(),
                o.model.clone(),
                // Mutter's serial; the connector keeps session restore stable.
                o.connector.clone(),
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
                    id: format!("{}x{}@{refresh:.3}", o.width, o.height),
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
        Ok((0, monitors, logical, properties))
    }

    /// GNOME Settings' Displays panel: 0 verifies, 1 applies, 2 applies
    /// and keeps (niri's checks: no mirroring, known connectors only).
    async fn apply_monitors_config(
        &self,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
        _serial: u32,
        method: u32,
        logical_monitor_configs: Vec<LogicalMonitorConfiguration>,
        _properties: HashMap<String, OwnedValue>,
    ) -> fdo::Result<()> {
        let configs = {
            let outputs = self
                .outputs
                .lock()
                .map_err(|_| fdo::Error::Failed("poisoned".into()))?;
            validate(&outputs, &logical_monitor_configs)?
        };
        if method == 0 {
            return Ok(());
        }
        self.to_loop
            .send(ToLoop::ApplyMonitors {
                configs,
                persistent: method == 2,
            })
            .map_err(|_| fdo::Error::Failed("compositor gone".into()))?;
        let _ = DisplayConfig::monitors_changed(&ctxt).await;
        Ok(())
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

/// Check a requested arrangement against the lit outputs.
fn validate(
    outputs: &[OutputSnapshot],
    requested: &[LogicalMonitorConfiguration],
) -> fdo::Result<Vec<crate::monitors::MonitorConfig>> {
    let mut configs = Vec::new();
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
        for (connector, _mode, _props) in &logical.monitors {
            if !outputs.iter().any(|o| &o.connector == connector) {
                return Err(fdo::Error::Failed(format!(
                    "connector '{connector}' not found"
                )));
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
    Ok(configs)
}

// --- ScreenCast ----------------------------------------------------------

#[derive(Clone)]
struct ScreenCast {
    outputs: Outputs,
    to_loop: calloop::channel::Sender<ToLoop>,
    next_id: Arc<AtomicU64>,
}

/// A session's streams with their D-Bus interface handles.
type SessionStreams = Arc<Mutex<Vec<(Stream, InterfaceRef<Stream>)>>>;

#[derive(Clone)]
struct Session {
    id: u64,
    outputs: Outputs,
    to_loop: calloop::channel::Sender<ToLoop>,
    next_id: Arc<AtomicU64>,
    streams: SessionStreams,
    stopped: Arc<AtomicBool>,
}

#[derive(Clone)]
struct Stream {
    output: OutputSnapshot,
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
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        _properties: HashMap<String, OwnedValue>,
    ) -> fdo::Result<OwnedObjectPath> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let path = format!("/org/gnome/Mutter/ScreenCast/Session/u{id}");
        let path =
            OwnedObjectPath::try_from(path).map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let session = Session {
            id,
            outputs: self.outputs.clone(),
            to_loop: self.to_loop.clone(),
            next_id: self.next_id.clone(),
            streams: Arc::new(Mutex::new(Vec::new())),
            stopped: Arc::new(AtomicBool::new(false)),
        };
        server.at(&path, session).await?;
        Ok(path)
    }

    #[zbus(property)]
    fn version(&self) -> i32 {
        4
    }
}

#[interface(name = "org.gnome.Mutter.ScreenCast.Session")]
impl Session {
    async fn start(&self) {
        let streams = self.streams.lock().map(|s| s.clone()).unwrap_or_default();
        for (stream, iface) in streams {
            let _ = self.to_loop.send(ToLoop::StartCast {
                session_id: self.id,
                connector: stream.output.connector.clone(),
                signal: iface.signal_emitter().to_owned(),
            });
        }
    }

    async fn stop(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
    ) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
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
    }

    async fn record_monitor(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        connector: &str,
        _properties: RecordMonitorProperties,
    ) -> fdo::Result<OwnedObjectPath> {
        let output = self
            .outputs
            .lock()
            .ok()
            .and_then(|o| o.iter().find(|o| o.connector == connector).cloned())
            .ok_or_else(|| fdo::Error::Failed("no such monitor".into()))?;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let path = format!("/org/gnome/Mutter/ScreenCast/Stream/u{id}");
        let path =
            OwnedObjectPath::try_from(path).map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let stream = Stream { output };
        server.at(&path, stream.clone()).await?;
        let iface = server.interface::<_, Stream>(&path).await?;
        if let Ok(mut streams) = self.streams.lock() {
            streams.push((stream, iface));
        }
        Ok(path)
    }

    #[zbus(signal)]
    async fn closed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[interface(name = "org.gnome.Mutter.ScreenCast.Stream")]
impl Stream {
    #[zbus(signal)]
    pub async fn pipe_wire_stream_added(ctxt: &SignalEmitter<'_>, node_id: u32)
        -> zbus::Result<()>;

    #[zbus(property)]
    fn parameters(&self) -> StreamParameters {
        let o = &self.output;
        StreamParameters {
            position: (o.x, o.y),
            size: (
                (f64::from(o.width) / o.scale).round() as i32,
                (f64::from(o.height) / o.scale).round() as i32,
            ),
        }
    }
}

/// Announce a stream's PipeWire node (from the event loop).
pub fn stream_added(signal: &SignalEmitter<'static>, node_id: u32) {
    let _ = zbus::block_on(Stream::pipe_wire_stream_added(signal, node_id));
}

/// Tell a session's client it ended (from the event loop).
pub fn session_closed(signal: &SignalEmitter<'static>) {
    let _ = zbus::block_on(Session::closed(signal));
}

/// Serve DisplayConfig and ScreenCast on the session bus from a thread.
/// Returns the channel the event loop reads cast requests from.
pub fn start(outputs: Outputs) -> calloop::channel::Channel<ToLoop> {
    let (to_loop, from_dbus) = calloop::channel::channel();
    let _ = std::thread::Builder::new()
        .name("roost-mutter-dbus".into())
        .spawn(move || {
            let display_config = DisplayConfig {
                outputs: outputs.clone(),
                to_loop: to_loop.clone(),
            };
            let screencast = ScreenCast {
                outputs: outputs.clone(),
                to_loop,
                next_id: Arc::new(AtomicU64::new(1)),
            };
            let conn = match zbus::blocking::connection::Builder::session()
                .and_then(|b| b.serve_at("/org/gnome/Mutter/ScreenCast", screencast))
                .and_then(|b| b.serve_at("/org/gnome/Mutter/DisplayConfig", display_config))
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
            loop {
                std::thread::park();
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
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            x: 0,
            y: 0,
            scale: 1.0,
            primary: true,
        }
    }

    fn logical(connector: &str, scale: f64) -> LogicalMonitorConfiguration {
        LogicalMonitorConfiguration {
            x: 0,
            y: 0,
            scale,
            transform: 0,
            is_primary: true,
            monitors: vec![(connector.into(), "1920x1080@60.000".into(), HashMap::new())],
        }
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
    fn builtin_panels_follow_mutter() {
        assert!(is_laptop_panel("eDP-1"));
        assert!(is_laptop_panel("LVDS-1"));
        assert!(!is_laptop_panel("HDMI-A-1"));
        assert!(!is_laptop_panel("roost-0"));
    }
}
