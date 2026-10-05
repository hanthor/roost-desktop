//! Authenticated GNOME RemoteDesktop sessions. The installed portal owns consent.
use super::*;
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicU32;

pub(crate) type RemoteSessions = Arc<Mutex<HashMap<String, RemoteGrant>>>;
#[derive(Clone)]
pub(crate) struct RemoteGrant {
    pub id: u64,
    pub owner: String,
    pub stopped: Arc<AtomicBool>,
    pub started: Arc<AtomicBool>,
    pub pending: Arc<AtomicU32>,
    eis_opened: Arc<AtomicBool>,
}

/// Input is queued with its revocable grant, never with caller-provided identity.
#[derive(Debug)]
pub enum Input {
    Key { evdev: u32, pressed: bool },
    Relative { dx: f64, dy: f64 },
    Absolute { x: f64, y: f64 },
    Button { button: u32, pressed: bool },
    Axis { dx: f64, dy: f64 },
}

pub(crate) struct RemoteDesktop {
    pub authority: crate::capture_security::Authority,
    pub remote: RemoteSessions,
    pub(super) captures: Sessions,
    pub outputs: Outputs,
    pub to_loop: calloop::channel::Sender<ToLoop>,
    pub next_id: Arc<AtomicU64>,
}
#[derive(Clone)]
pub(crate) struct RemoteSession {
    pub grant: RemoteGrant,
    pub authority: crate::capture_security::Authority,
    pub(super) captures: Sessions,
    pub outputs: Outputs,
    pub to_loop: calloop::channel::Sender<ToLoop>,
}
impl RemoteSession {
    fn admitted(&self, header: &zbus::message::Header<'_>, active: bool) -> fdo::Result<()> {
        if !crate::capture_security::owns_session(
            &self.grant.owner,
            header.sender().map(|s| s.as_str()),
            self.grant.stopped.load(Ordering::SeqCst),
        ) {
            return Err(crate::capture_security::denied(
                "remote session is not owned by caller",
            ));
        }
        self.authority.unlocked()?;
        if active && !self.grant.started.load(Ordering::SeqCst) {
            return Err(crate::capture_security::denied(
                "remote session has not started",
            ));
        }
        Ok(())
    }
    fn send(&self, input: Input) -> fdo::Result<()> {
        queue_input(&self.grant, &self.to_loop, input)
    }
    fn unsupported(&self, header: &zbus::message::Header<'_>) -> fdo::Result<()> {
        self.admitted(header, true)?;
        Err(fdo::Error::NotSupported(
            "this input capability is not advertised".into(),
        ))
    }
    fn absolute(&self, stream_path: &str, x: f64, y: f64) -> fdo::Result<Input> {
        finite_pair(x, y)?;
        let captures = self
            .captures
            .lock()
            .map_err(|_| fdo::Error::Failed("capture registry unavailable".into()))?;
        for (capture, _) in captures.iter() {
            if !Arc::ptr_eq(&capture.stopped, &self.grant.stopped) {
                continue;
            }
            let streams = capture
                .streams
                .lock()
                .map_err(|_| fdo::Error::Failed("stream registry unavailable".into()))?;
            for (stream, iface) in streams.iter() {
                if iface.signal_emitter().path().as_str() != stream_path {
                    continue;
                }
                let parameters = stream.parameters();
                if x < 0.0
                    || y < 0.0
                    || x >= f64::from(parameters.size.0)
                    || y >= f64::from(parameters.size.1)
                {
                    return Err(fdo::Error::InvalidArgs(
                        "position outside granted stream".into(),
                    ));
                }
                return Ok(Input::Absolute {
                    x: x + f64::from(parameters.position.0),
                    y: y + f64::from(parameters.position.1),
                });
            }
        }
        Err(crate::capture_security::denied(
            "stream is not associated with this remote grant",
        ))
    }
}

pub(crate) fn queue_input(
    grant: &RemoteGrant,
    to_loop: &calloop::channel::Sender<ToLoop>,
    input: Input,
) -> fdo::Result<()> {
    if grant.stopped.load(Ordering::SeqCst) || !grant.started.load(Ordering::SeqCst) {
        return Err(crate::capture_security::denied("remote grant is inactive"));
    }
    let mut pending = grant.pending.load(Ordering::SeqCst);
    loop {
        let next = pending
            .checked_add(1)
            .filter(|next| *next <= 256)
            .ok_or_else(|| fdo::Error::LimitsExceeded("remote input queue full".into()))?;
        match grant
            .pending
            .compare_exchange_weak(pending, next, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => break,
            Err(current) => pending = current,
        }
    }
    if to_loop
        .send(ToLoop::RemoteInput {
            session_id: grant.id,
            grant: grant.stopped.clone(),
            pending: grant.pending.clone(),
            input,
        })
        .is_err()
    {
        grant.pending.fetch_sub(1, Ordering::SeqCst);
        return Err(fdo::Error::Failed("compositor gone".into()));
    }
    Ok(())
}

fn device_types(value: Option<&OwnedValue>) -> fdo::Result<u32> {
    let types = value
        .map(u32::try_from)
        .transpose()
        .map_err(|_| fdo::Error::InvalidArgs("device-types must be unsigned".into()))?
        .unwrap_or(3);
    if types == 0 || types & !3 != 0 {
        return Err(fdo::Error::InvalidArgs(
            "only keyboard and pointer are supported".into(),
        ));
    }
    Ok(types)
}

pub(crate) fn finite_pair(x: f64, y: f64) -> fdo::Result<()> {
    if !x.is_finite() || !y.is_finite() || x.abs() > 1_000_000.0 || y.abs() > 1_000_000.0 {
        return Err(fdo::Error::InvalidArgs(
            "input coordinates must be finite and bounded".into(),
        ));
    }
    Ok(())
}
pub(crate) fn valid_key(key: u32) -> fdo::Result<()> {
    if key > 0x2ff {
        return Err(fdo::Error::InvalidArgs("invalid evdev keycode".into()));
    }
    Ok(())
}
pub(crate) fn valid_button(button: i32) -> fdo::Result<u32> {
    if !(0x110..=0x11f).contains(&button) {
        return Err(fdo::Error::InvalidArgs("unsupported pointer button".into()));
    }
    Ok(button as u32)
}

#[interface(name = "org.gnome.Mutter.RemoteDesktop")]
impl RemoteDesktop {
    async fn create_session(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<OwnedObjectPath> {
        let owner = self.authority.admit(conn, &header).await?;
        if self
            .remote
            .lock()
            .map_or(true, |sessions| sessions.len() >= 64)
        {
            return Err(fdo::Error::LimitsExceeded(
                "too many remote sessions".into(),
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let grant = RemoteGrant {
            id,
            owner,
            stopped: Arc::new(AtomicBool::new(false)),
            started: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(AtomicU32::new(0)),
            eis_opened: Arc::new(AtomicBool::new(false)),
        };
        let session_id = format!("roost-remote-{id}");
        let path =
            OwnedObjectPath::try_from(format!("/org/gnome/Mutter/RemoteDesktop/Session/u{id}"))
                .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        server
            .at(
                &path,
                RemoteSession {
                    grant: grant.clone(),
                    authority: self.authority.clone(),
                    captures: self.captures.clone(),
                    outputs: self.outputs.clone(),
                    to_loop: self.to_loop.clone(),
                },
            )
            .await?;
        self.remote
            .lock()
            .map_err(|_| fdo::Error::Failed("remote registry unavailable".into()))?
            .insert(session_id, grant);
        Ok(path)
    }
    #[zbus(property)]
    fn supported_device_types(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn version(&self) -> i32 {
        2
    }
}

#[interface(name = "org.gnome.Mutter.RemoteDesktop.Session")]
impl RemoteSession {
    #[zbus(property)]
    fn session_id(&self) -> String {
        format!("roost-remote-{}", self.grant.id)
    }
    async fn start(&self, #[zbus(header)] header: zbus::message::Header<'_>) -> fdo::Result<()> {
        self.admitted(&header, false)?;
        if self.grant.started.swap(true, Ordering::SeqCst) {
            return Err(fdo::Error::Failed("remote session already started".into()));
        }
        let captures = self
            .captures
            .lock()
            .map_err(|_| fdo::Error::Failed("capture registry unavailable".into()))?
            .clone();
        for (capture, _) in captures {
            if Arc::ptr_eq(&capture.stopped, &self.grant.stopped) {
                capture.start_streams()?;
            }
        }
        Ok(())
    }
    async fn stop(&self, #[zbus(header)] header: zbus::message::Header<'_>) -> fdo::Result<()> {
        self.admitted(&header, false)?;
        self.grant.stopped.store(true, Ordering::SeqCst);
        self.to_loop
            .send(ToLoop::RemoteStop {
                session_id: self.grant.id,
            })
            .map_err(|_| fdo::Error::Failed("compositor gone".into()))?;
        Ok(())
    }
    #[zbus(signal)]
    pub async fn closed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;
    async fn notify_keyboard_keycode(
        &self,
        keycode: u32,
        state: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.admitted(&header, true)?;
        valid_key(keycode)?;
        self.send(Input::Key {
            evdev: keycode,
            pressed: state,
        })
    }
    async fn notify_keyboard_keysym(
        &self,
        _keysym: u32,
        _state: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.unsupported(&header)
    }
    async fn notify_pointer_button(
        &self,
        button: i32,
        state: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.admitted(&header, true)?;
        self.send(Input::Button {
            button: valid_button(button)?,
            pressed: state,
        })
    }
    async fn notify_pointer_motion_relative(
        &self,
        dx: f64,
        dy: f64,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.admitted(&header, true)?;
        finite_pair(dx, dy)?;
        self.send(Input::Relative { dx, dy })
    }
    async fn notify_pointer_motion_absolute(
        &self,
        stream: String,
        x: f64,
        y: f64,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.admitted(&header, true)?;
        self.send(self.absolute(&stream, x, y)?)
    }
    async fn notify_pointer_axis(
        &self,
        dx: f64,
        dy: f64,
        flags: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.admitted(&header, true)?;
        finite_pair(dx, dy)?;
        if flags & !15 != 0 || (flags & 14).count_ones() > 1 {
            return Err(fdo::Error::InvalidArgs("invalid axis flags".into()));
        }
        self.send(Input::Axis { dx, dy })
    }
    async fn notify_pointer_axis_discrete(
        &self,
        axis: u32,
        steps: i32,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.admitted(&header, true)?;
        if axis > 1 || steps.unsigned_abs() > 1000 {
            return Err(fdo::Error::InvalidArgs("invalid discrete axis".into()));
        }
        let delta = f64::from(steps) * 120.0;
        self.send(Input::Axis {
            dx: if axis == 1 { delta } else { 0.0 },
            dy: if axis == 0 { delta } else { 0.0 },
        })
    }
    async fn notify_touch_down(
        &self,
        _stream: String,
        _slot: u32,
        _x: f64,
        _y: f64,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.unsupported(&header)
    }
    async fn notify_touch_motion(
        &self,
        _stream: String,
        _slot: u32,
        _x: f64,
        _y: f64,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.unsupported(&header)
    }
    async fn notify_touch_up(
        &self,
        _slot: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.unsupported(&header)
    }
    #[zbus(name = "ConnectToEIS")]
    async fn connect_to_eis(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<zbus::zvariant::OwnedFd> {
        self.admitted(&header, true)?;
        let types = device_types(options.get("device-types"))?;
        if self.grant.eis_opened.swap(true, Ordering::SeqCst) {
            return Err(fdo::Error::LimitsExceeded(
                "one EIS connection per remote session".into(),
            ));
        }
        let result = (|| {
            let (server, client) =
                UnixStream::pair().map_err(|e| fdo::Error::Failed(e.to_string()))?;
            let (reply, answer) = std::sync::mpsc::channel();
            self.to_loop
                .send(ToLoop::RemoteKeymap { reply })
                .map_err(|_| fdo::Error::Failed("compositor gone".into()))?;
            let keymap = answer
                .recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|_| fdo::Error::Failed("keyboard unavailable".into()))?;
            self.authority.unlocked()?;
            super::remote_eis::start(
                server,
                self.grant.clone(),
                types,
                keymap,
                self.outputs.clone(),
                self.to_loop.clone(),
            )
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
            Ok(std::os::fd::OwnedFd::from(client).into())
        })();
        if result.is_err() {
            self.grant.eis_opened.store(false, Ordering::SeqCst);
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_introspection_uses_gnome_eis_acronym() {
        let (sender, _receiver) = calloop::channel::channel();
        let session = RemoteSession {
            grant: RemoteGrant {
                id: 1,
                owner: ":1.1".into(),
                stopped: Arc::new(AtomicBool::new(false)),
                started: Arc::new(AtomicBool::new(false)),
                pending: Arc::new(AtomicU32::new(0)),
                eis_opened: Arc::new(AtomicBool::new(false)),
            },
            authority: Default::default(),
            captures: Default::default(),
            outputs: Default::default(),
            to_loop: sender,
        };
        let mut xml = String::new();
        zbus::object_server::Interface::introspect_to_writer(&session, &mut xml, 0);
        assert!(xml.contains("method name=\"ConnectToEIS\""));
        assert!(!xml.contains("method name=\"ConnectToEis\""));
    }
    #[test]
    fn selected_capabilities_reject_malformed_or_unadvertised_types() {
        assert_eq!(device_types(None).unwrap(), 3);
        for allowed in [1u32, 2, 3] {
            assert_eq!(
                device_types(Some(&OwnedValue::from(allowed))).unwrap(),
                allowed
            );
        }
        for denied in [0u32, 4, u32::MAX] {
            assert!(device_types(Some(&OwnedValue::from(denied))).is_err());
        }
        assert!(device_types(Some(&OwnedValue::from(true))).is_err());
    }
    #[test]
    fn queue_bound_revocation_and_channel_failure_are_enforced() {
        let grant = RemoteGrant {
            id: 1,
            owner: ":1.1".into(),
            stopped: Arc::new(AtomicBool::new(false)),
            started: Arc::new(AtomicBool::new(true)),
            pending: Arc::new(AtomicU32::new(0)),
            eis_opened: Arc::new(AtomicBool::new(false)),
        };
        let (sender, receiver) = calloop::channel::channel();
        for _ in 0..256 {
            queue_input(&grant, &sender, Input::Relative { dx: 1.0, dy: 0.0 }).unwrap();
        }
        assert!(queue_input(&grant, &sender, Input::Relative { dx: 1.0, dy: 0.0 }).is_err());
        assert_eq!(grant.pending.load(Ordering::SeqCst), 256);
        grant.pending.store(0, Ordering::SeqCst);
        grant.stopped.store(true, Ordering::SeqCst);
        assert!(queue_input(&grant, &sender, Input::Relative { dx: 1.0, dy: 0.0 }).is_err());
        assert_eq!(grant.pending.load(Ordering::SeqCst), 0);
        grant.stopped.store(false, Ordering::SeqCst);
        drop(receiver);
        assert!(queue_input(&grant, &sender, Input::Relative { dx: 1.0, dy: 0.0 }).is_err());
        assert_eq!(grant.pending.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn remote_coordinates_and_codes_are_bounded() {
        assert!(finite_pair(0.0, -32.0).is_ok());
        for bad in [f64::NAN, f64::INFINITY, -f64::INFINITY, 1_000_001.0] {
            assert!(finite_pair(bad, 0.0).is_err());
        }
        assert!(valid_key(0x2ff).is_ok());
        assert!(valid_key(0x300).is_err());
        assert_eq!(valid_button(0x110).unwrap(), 0x110);
        assert!(valid_button(-1).is_err());
    }
}
