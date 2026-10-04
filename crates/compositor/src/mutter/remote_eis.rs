//! EIS file descriptors are delivered only to an authenticated, started remote grant.
use super::{
    remote_desktop::{self, Input, RemoteGrant},
    Outputs, ToLoop,
};
use reis::{
    calloop::{EisRequestSource, EisRequestSourceEvent},
    eis,
    request::{Connection, DeviceCapability, EisRequest},
};
use std::{
    io::{self, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    sync::atomic::Ordering,
    time::Duration,
};

struct Worker {
    grant: RemoteGrant,
    to_loop: calloop::channel::Sender<ToLoop>,
    capabilities: reis::enumflags2::BitFlags<DeviceCapability>,
    seat: Option<reis::request::Seat>,
    device: Option<reis::request::Device>,
    keymap: std::fs::File,
    keymap_len: u32,
    outputs: Vec<super::OutputSnapshot>,
    origin: (i32, i32),
    connection: Option<Connection>,
    disconnected: bool,
    bindings: u32,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.grant.stopped.store(true, Ordering::SeqCst);
        let _ = self.to_loop.send(ToLoop::RemoteStop {
            session_id: self.grant.id,
        });
    }
}
impl Worker {
    fn event(
        &mut self,
        event: EisRequestSourceEvent,
        connection: &Connection,
    ) -> calloop::PostAction {
        if self.grant.stopped.load(Ordering::SeqCst) {
            return calloop::PostAction::Remove;
        }
        self.connection = Some(connection.clone());
        match event {
            EisRequestSourceEvent::Connected => {
                if connection.context_type() != eis::handshake::ContextType::Sender {
                    self.disconnected = true;
                    return calloop::PostAction::Remove;
                }
                self.seat = Some(connection.add_seat(Some("Roost remote seat"), self.capabilities));
            }
            EisRequestSourceEvent::Request(EisRequest::Disconnect) => {
                self.disconnected = true;
                return calloop::PostAction::Remove;
            }
            EisRequestSourceEvent::Request(EisRequest::Bind(request)) => {
                // Rebinding emits device objects and keymap FDs. Bound churn
                // even if a malicious client never acknowledges old objects.
                self.bindings += 1;
                if self.bindings > 32 {
                    self.disconnected = true;
                    return calloop::PostAction::Remove;
                }
                if let Some(device) = self.device.take() {
                    device.remove();
                }
                let capabilities = request.capabilities & self.capabilities;
                if !capabilities.is_empty() {
                    let device = request.seat.add_device(
                        Some("Roost remote input"),
                        eis::device::DeviceType::Virtual,
                        capabilities,
                        |device| {
                            if let Some(keyboard) = device.interface::<eis::Keyboard>() {
                                keyboard.keymap(
                                    eis::keyboard::KeymapType::Xkb,
                                    self.keymap_len,
                                    self.keymap.as_fd(),
                                );
                            }
                            if device.has_capability(DeviceCapability::PointerAbsolute) {
                                for output in &self.outputs {
                                    let width =
                                        (f64::from(output.width) / output.scale).round().max(1.0)
                                            as u32;
                                    let height =
                                        (f64::from(output.height) / output.scale).round().max(1.0)
                                            as u32;
                                    device.device().region(
                                        (output.x - self.origin.0) as u32,
                                        (output.y - self.origin.1) as u32,
                                        width,
                                        height,
                                        output.scale as f32,
                                    );
                                }
                            }
                        },
                    );
                    device.resumed();
                    self.device = Some(device);
                }
            }
            EisRequestSourceEvent::Request(request) => {
                let input = match request {
                    EisRequest::KeyboardKey(event)
                        if remote_desktop::valid_key(event.key).is_ok() =>
                    {
                        Some(Input::Key {
                            evdev: event.key,
                            pressed: event.state == eis::keyboard::KeyState::Press,
                        })
                    }
                    EisRequest::Button(event)
                        if remote_desktop::valid_button(event.button as i32).is_ok() =>
                    {
                        Some(Input::Button {
                            button: event.button,
                            pressed: event.state == eis::button::ButtonState::Press,
                        })
                    }
                    EisRequest::PointerMotion(event)
                        if remote_desktop::finite_pair(
                            f64::from(event.dx),
                            f64::from(event.dy),
                        )
                        .is_ok() =>
                    {
                        Some(Input::Relative {
                            dx: f64::from(event.dx),
                            dy: f64::from(event.dy),
                        })
                    }
                    EisRequest::PointerMotionAbsolute(event) => {
                        let x = f64::from(event.dx_absolute) + f64::from(self.origin.0);
                        let y = f64::from(event.dy_absolute) + f64::from(self.origin.1);
                        if remote_desktop::finite_pair(x, y).is_ok()
                            && self.outputs.iter().any(|out| {
                                x >= f64::from(out.x)
                                    && y >= f64::from(out.y)
                                    && x < f64::from(out.x) + f64::from(out.width) / out.scale
                                    && y < f64::from(out.y) + f64::from(out.height) / out.scale
                            })
                        {
                            Some(Input::Absolute { x, y })
                        } else {
                            None
                        }
                    }
                    EisRequest::ScrollDelta(event)
                        if remote_desktop::finite_pair(
                            f64::from(event.dx),
                            f64::from(event.dy),
                        )
                        .is_ok() =>
                    {
                        Some(Input::Axis {
                            dx: f64::from(event.dx),
                            dy: f64::from(event.dy),
                        })
                    }
                    EisRequest::ScrollDiscrete(event)
                        if event.discrete_dx.unsigned_abs() <= 120_000
                            && event.discrete_dy.unsigned_abs() <= 120_000 =>
                    {
                        Some(Input::Axis {
                            dx: f64::from(event.discrete_dx),
                            dy: f64::from(event.discrete_dy),
                        })
                    }
                    _ => None,
                };
                if let Some(input) = input {
                    if remote_desktop::queue_input(&self.grant, &self.to_loop, input).is_err() {
                        self.disconnected = true;
                        return calloop::PostAction::Remove;
                    }
                }
            }
        }
        if connection.flush().is_err() {
            self.disconnected = true;
            return calloop::PostAction::Remove;
        }
        calloop::PostAction::Continue
    }
}

pub(crate) fn start(
    socket: UnixStream,
    grant: RemoteGrant,
    types: u32,
    keymap: String,
    outputs: Outputs,
    to_loop: calloop::channel::Sender<ToLoop>,
) -> io::Result<()> {
    if keymap.is_empty() || keymap.len() > 1_048_576 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid EIS keymap",
        ));
    }
    let fd = rustix::fs::memfd_create(
        "roost-remote-keymap",
        rustix::fs::MemfdFlags::CLOEXEC | rustix::fs::MemfdFlags::ALLOW_SEALING,
    )?;
    let mut file = std::fs::File::from(fd);
    file.write_all(keymap.as_bytes())?;
    file.write_all(&[0])?;
    rustix::fs::fcntl_add_seals(
        &file,
        rustix::fs::SealFlags::GROW
            | rustix::fs::SealFlags::SHRINK
            | rustix::fs::SealFlags::WRITE
            | rustix::fs::SealFlags::SEAL,
    )?;
    let keymap_len = keymap.len() as u32 + 1;
    let outputs = outputs
        .lock()
        .map_err(|_| io::Error::other("output registry unavailable"))?
        .clone();
    if outputs.is_empty()
        || outputs.len() > 64
        || outputs.iter().any(|out| {
            out.width <= 0
                || out.height <= 0
                || out.width > 32768
                || out.height > 32768
                || !out.scale.is_finite()
                || out.scale < 0.25
                || out.scale > 8.0
                || out.x.abs_diff(0) > 1_000_000
                || out.y.abs_diff(0) > 1_000_000
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid EIS output regions",
        ));
    }
    let origin = (
        outputs.iter().map(|o| o.x).min().unwrap_or(0),
        outputs.iter().map(|o| o.y).min().unwrap_or(0),
    );
    let mut capabilities = reis::enumflags2::BitFlags::empty();
    if types & 1 != 0 {
        capabilities |= DeviceCapability::Keyboard;
    }
    if types & 2 != 0 {
        capabilities |= DeviceCapability::Pointer
            | DeviceCapability::PointerAbsolute
            | DeviceCapability::Button
            | DeviceCapability::Scroll;
    }
    let context = eis::Context::new(socket)?;
    let thread_grant = grant.clone();
    std::thread::Builder::new()
        .name(format!("roost-eis-{}", grant.id))
        .stack_size(256 * 1024)
        .spawn(move || {
            let mut worker = Worker {
                grant: thread_grant.clone(),
                to_loop: to_loop.clone(),
                capabilities,
                seat: None,
                device: None,
                keymap: file,
                keymap_len,
                outputs,
                origin,
                connection: None,
                disconnected: false,
                bindings: 0,
            };
            let Ok(mut event_loop) = calloop::EventLoop::<Worker>::try_new() else {
                thread_grant.stopped.store(true, Ordering::SeqCst);
                return;
            };
            if event_loop
                .handle()
                .insert_source(
                    EisRequestSource::new(context, 1),
                    |event, connection, worker| {
                        Ok(match event {
                            Ok(event) => worker.event(event, connection),
                            Err(_) => {
                                worker.disconnected = true;
                                calloop::PostAction::Remove
                            }
                        })
                    },
                )
                .is_err()
            {
                thread_grant.stopped.store(true, Ordering::SeqCst);
                return;
            }
            while !thread_grant.stopped.load(Ordering::SeqCst) && !worker.disconnected {
                if event_loop
                    .dispatch(Duration::from_millis(20), &mut worker)
                    .is_err()
                {
                    break;
                }
            }
            thread_grant.stopped.store(true, Ordering::SeqCst);
            if let Some(connection) = worker.connection.take() {
                connection.disconnected(reis::ei::connection::DisconnectReason::Disconnected, None);
            }
        })?;
    Ok(())
}
