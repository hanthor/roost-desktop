//! DRM/KMS hardware session backend (#52).
//!
//! The nested winit backend draws into a window of a host session; this
//! backend owns the display hardware itself, so Roost can start from a
//! TTY through greetd:
//!
//! - **Session**: libseat (logind or seatd) grants device access and
//!   reports VT pause/activate. Ctrl+Alt+F1..F12 switches VTs.
//! - **Device**: udev's primary GPU for the seat (or
//!   `ROOST_DRM_DEVICE`), opened through the session; GBM allocates
//!   scanout buffers and EGL/GLES renders into them.
//! - **Outputs**: every connected connector with its preferred mode and
//!   a free CRTC, tiled left to right in the same order the output
//!   inventory uses. Each output renders when its previous page flip
//!   completed (vblank), so frames are paced by the display.
//! - **Input**: libinput on the session's seat. Relative pointer motion
//!   is accumulated here and clamped to the output union, so the window
//!   manager keeps receiving absolute positions like the nested backend.
//!
//! The pointer is drawn in software (an arrow of solid rectangles):
//! there is no host cursor on bare hardware. Client cursor surfaces are
//! not composited yet.

use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::Fourcc;
use smithay::backend::drm::{
    DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEvent, DrmEventMetadata, DrmEventTime,
    GbmBufferedSurface,
};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::input::{Event, InputEvent, KeyState, KeyboardKeyEvent, PointerMotionEvent};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::drm::control::{connector, crtc, Device as ControlDevice, ModeTypeFlags};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{DeviceFd, Logical, Physical, Point, Rectangle, Size};

use crate::windows::ManagerInput;

/// Scanout surface type: GBM buffers on the DRM device, each queued
/// frame carrying the presentation feedback of what it drew (#89).
pub type ScanoutSurface = GbmBufferedSurface<GbmAllocator<DrmDeviceFd>, OutputPresentationFeedback>;

/// A completed page flip: the frame is on the screen.
pub struct PageFlip {
    /// The flip was the primary output's (it paces fifo and commit
    /// timers, as one output's frame clock does in Mutter).
    pub primary: bool,
    /// Feedback of the surfaces the frame drew.
    pub feedback: Option<OutputPresentationFeedback>,
    /// Kernel vblank timestamp on CLOCK_MONOTONIC, when it gave one.
    pub time: Option<std::time::Duration>,
    /// Kernel vblank sequence.
    pub sequence: u64,
    /// The output's refresh interval.
    pub refresh: std::time::Duration,
}

/// One lit connector.
pub struct DrmOutput {
    /// Inventory name, e.g. `eDP-1`.
    pub name: String,
    /// CRTC driving the connector.
    pub crtc: crtc::Handle,
    pub connector: connector::Handle,
    native_owner: Option<roost_shell_control::NativeOutputInfo>,
    /// GBM swapchain bound to the CRTC.
    pub surface: ScanoutSurface,
    /// Protocol output advertised to clients.
    pub output: Output,
    /// Mode size in physical pixels.
    pub size: Size<i32, Physical>,
    /// Top-left in the global logical space: GNOME's arrangement from
    /// monitors.xml, else left-to-right.
    pub loc: (i32, i32),
    /// Output scale from monitors.xml (`ROOST_SCALE` without one), #59.
    pub scale: f64,
    /// A frame is queued and its page flip has not completed yet.
    pub pending: bool,
    pub(crate) last_frame: Option<crate::native_repaint::FrameSignature>,
    pub(crate) damage_tracker: Option<smithay::backend::renderer::damage::OutputDamageTracker>,
    wake_trace: bool,
}

impl DrmOutput {
    pub fn trace_wake_submission(&mut self) {
        if !std::mem::take(&mut self.wake_trace) {
            return;
        }
        let state = self.surface.surface().get_crtc(self.crtc);
        eprintln!(
            "roost-compositor: drm: wake frame queued {} commit_pending={} crtc={state:?}",
            self.name,
            self.surface.surface().commit_pending(),
        );
    }

    /// Size in logical pixels (mode size over scale).
    pub fn logical_size(&self) -> (i32, i32) {
        crate::runtime::logical_size(self.size.w, self.size.h, self.scale)
    }
}

/// Hardware session state.
pub struct DrmBackend {
    /// libseat session (device access, VT switching).
    pub session: LibSeatSession,
    /// Opened KMS device.
    pub drm: DrmDevice,
    /// GLES renderer on the device's EGL display.
    pub renderer: GlesRenderer,
    /// Lit outputs, primary first.
    pub outputs: Vec<DrmOutput>,
    /// libinput context, suspended while the session is paused.
    pub libinput: Libinput,
    /// Whether the session currently owns the VT.
    pub active: bool,
    sleep_reset_pending: bool,
    wake_scanout_blocked: bool,
    wake_event_traces: u8,
    pointer: Point<f64, Logical>,
    corner_pressure: crate::corner_pressure::CornerPressure,
    hot_corner_active: bool,
    relative_motion_events: u64,
    ctrl: bool,
    alt: bool,
    /// libinput devices, for GNOME's touchpad and mouse settings (#60).
    devices: Vec<smithay::reexports::input::Device>,
    input_settings: roost_shell_control::InputSettings,
}

/// Event sources the runtime installs on its loop.
pub struct DrmSources {
    /// Session pause/activate.
    pub session: LibSeatSessionNotifier,
    /// VBlank and device errors.
    pub drm: DrmDeviceNotifier,
    /// Input events.
    pub input: LibinputInputBackend,
}

/// Why the hardware session could not start.
#[derive(Debug)]
pub struct DrmInitError(pub String);

impl std::fmt::Display for DrmInitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "drm backend: {}", self.0)
    }
}

fn err(context: &str) -> impl Fn(String) -> DrmInitError + '_ {
    move |e| DrmInitError(format!("{context}: {e}"))
}

// Evdev keycodes (backend keycodes minus the XKB offset of 8).
const KEY_LEFTCTRL: u32 = 29;
const KEY_RIGHTCTRL: u32 = 97;
const KEY_LEFTALT: u32 = 56;
const KEY_RIGHTALT: u32 = 100;
const KEY_F1: u32 = 59;
const KEY_F10: u32 = 68;
const KEY_F11: u32 = 87;
const KEY_F12: u32 = 88;
const XKB_OFFSET: u32 = 8;

/// VT number for an evdev function-key code, if it is F1..F12.
pub fn vt_for_key(keycode: u32) -> Option<i32> {
    match keycode {
        KEY_F1..=KEY_F10 => Some((keycode - KEY_F1 + 1) as i32),
        KEY_F11 => Some(11),
        KEY_F12 => Some(12),
        _ => None,
    }
}

/// Clamp a pointer position into the union of output rectangles.
pub fn clamp_to_outputs(
    pos: Point<f64, Logical>,
    outputs: &[Rectangle<i32, Logical>],
) -> Point<f64, Logical> {
    if outputs.iter().any(|r| r.to_f64().contains(pos)) {
        return pos;
    }
    // Nearest point on the nearest output.
    let mut best = pos;
    let mut best_d = f64::INFINITY;
    for r in outputs {
        let r = r.to_f64();
        let x = pos.x.clamp(r.loc.x, r.loc.x + r.size.w - 1.0);
        let y = pos.y.clamp(r.loc.y, r.loc.y + r.size.h - 1.0);
        let d = (x - pos.x).powi(2) + (y - pos.y).powi(2);
        if d < best_d {
            best_d = d;
            best = (x, y).into();
        }
    }
    best
}

impl DrmBackend {
    pub fn native_outputs(&self) -> Vec<roost_shell_control::NativeOutputInfo> {
        use std::os::fd::AsFd;
        if !self.active || !self.drm.is_active() {
            return Vec::new();
        }
        let Ok(device) = crate::native_output::device(self.drm.device_fd().as_fd()) else {
            return Vec::new();
        };
        if device != self.drm.device_id() {
            return Vec::new();
        }
        // Identity is captured during actual connector discovery, not scanned
        // every render tick. Shell admission revalidates canonical status/object.
        self.outputs
            .iter()
            .filter_map(|output| output.native_owner.as_ref())
            .filter(|owner| owner.drm_device == device)
            .cloned()
            .collect()
    }

    /// Open the seat, the primary GPU, every connected output, and
    /// libinput. Fails with a readable reason (no seat daemon, no GPU,
    /// nothing connected) so the launcher can report it.
    pub fn new() -> Result<(Self, DrmSources), DrmInitError> {
        let (mut session, session_notifier) =
            LibSeatSession::new().map_err(|e| err("libseat session")(e.to_string()))?;
        let seat = session.seat();
        let path = match std::env::var_os("ROOST_DRM_DEVICE") {
            Some(path) => std::path::PathBuf::from(path),
            None => smithay::backend::udev::primary_gpu(&seat)
                .ok()
                .flatten()
                .or_else(|| {
                    smithay::backend::udev::all_gpus(&seat)
                        .ok()
                        .and_then(|gpus| gpus.into_iter().next())
                })
                .ok_or_else(|| DrmInitError(format!("no GPU on seat {seat}")))?,
        };
        let fd = session
            .open(
                &path,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            )
            .map_err(|e| err("open drm device")(format!("{}: {e}", path.display())))?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (mut drm, drm_notifier) =
            DrmDevice::new(fd.clone(), true).map_err(|e| err("drm device")(e.to_string()))?;
        let gbm = GbmDevice::new(fd.clone()).map_err(|e| err("gbm device")(e.to_string()))?;
        // SAFETY: the GBM device outlives the display; both are held for
        // the backend's lifetime through the renderer's context.
        let egl = unsafe { EGLDisplay::new(gbm.clone()) }
            .map_err(|e| err("egl display")(e.to_string()))?;
        let context = EGLContext::new(&egl).map_err(|e| err("egl context")(e.to_string()))?;
        let render_formats = context.dmabuf_render_formats().clone();
        // SAFETY: the context is current on this thread only.
        let renderer =
            unsafe { GlesRenderer::new(context) }.map_err(|e| err("gles")(e.to_string()))?;

        let resources = drm
            .resource_handles()
            .map_err(|e| err("drm resources")(e.to_string()))?;
        use std::os::fd::AsFd;
        let native_device = crate::native_output::device(drm.device_fd().as_fd())
            .ok()
            .filter(|device| *device == drm.device_id());
        let mut used: Vec<crtc::Handle> = Vec::new();
        let mut outputs = Vec::new();
        let mut next_x = 0;
        for handle in resources.connectors() {
            let Ok(info) = drm.get_connector(*handle, false) else {
                continue;
            };
            if info.state() != connector::State::Connected {
                continue;
            }
            let Some(mode) = info
                .modes()
                .iter()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .or_else(|| info.modes().first())
                .copied()
            else {
                continue;
            };
            let crtc = info
                .encoders()
                .iter()
                .filter_map(|enc| drm.get_encoder(*enc).ok())
                .flat_map(|enc| resources.filter_crtcs(enc.possible_crtcs()))
                .find(|crtc| !used.contains(crtc));
            let Some(crtc) = crtc else {
                continue;
            };
            let surface = match drm.create_surface(crtc, mode, &[*handle]) {
                Ok(surface) => surface,
                Err(e) => {
                    eprintln!("roost-compositor: drm: skip connector: {e}");
                    continue;
                }
            };
            let allocator = GbmAllocator::new(
                gbm.clone(),
                GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
            );
            let surface = match GbmBufferedSurface::new(
                surface,
                allocator,
                &[Fourcc::Argb8888, Fourcc::Xrgb8888],
                render_formats.iter().copied(),
            ) {
                Ok(surface) => surface,
                Err(e) => {
                    eprintln!("roost-compositor: drm: skip connector: {e}");
                    continue;
                }
            };
            used.push(crtc);
            let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
            let (w, h) = mode.size();
            let size: Size<i32, Physical> = (i32::from(w), i32::from(h)).into();
            let (mm_w, mm_h) = info.size().unwrap_or((0, 0));
            let output = Output::new(
                name.clone(),
                PhysicalProperties {
                    size: (mm_w as i32, mm_h as i32).into(),
                    subpixel: Subpixel::Unknown,
                    make: "Roost".to_owned(),
                    model: name.clone(),
                },
            );
            let out_mode = Mode::from(mode);
            output.change_current_state(Some(out_mode), None, None, None);
            output.set_preferred(out_mode);
            let native_owner = native_device.and_then(|device| {
                crate::native_output::resolve(
                    std::path::Path::new("/sys"),
                    device,
                    &name,
                    u32::from(*handle),
                )
                .ok()
            });
            outputs.push(DrmOutput {
                name,
                crtc,
                connector: *handle,
                native_owner,
                surface,
                output,
                size,
                loc: (0, 0),
                scale: 1.0,
                pending: false,
                last_frame: None,
                damage_tracker: None,
                wake_trace: false,
            });
        }
        // GNOME's arrangement for exactly these connectors (#59): scale
        // and logical position per output; without one, ROOST_SCALE (or
        // 1) and left-to-right logical placement.
        let names: Vec<String> = outputs.iter().map(|o| o.name.clone()).collect();
        let arrangement = crate::monitors::load(&names);
        let fallback_scale = std::env::var("ROOST_SCALE")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .map(crate::runtime::clamp_scale)
            .unwrap_or(1.0);
        for out in outputs.iter_mut() {
            match arrangement.get(&out.name) {
                Some(config) => {
                    out.scale = crate::runtime::clamp_scale(config.scale);
                    out.loc = (config.x, config.y);
                }
                None => {
                    out.scale = fallback_scale;
                    out.loc = (next_x, 0);
                }
            }
            next_x = next_x.max(out.loc.0 + out.logical_size().0);
            out.output.change_current_state(
                None,
                None,
                Some(if out.scale == 1.0 {
                    smithay::output::Scale::Integer(1)
                } else {
                    smithay::output::Scale::Fractional(out.scale)
                }),
                Some(out.loc.into()),
            );
            eprintln!(
                "roost-compositor: drm: output {} {}x{} at {},{} scale {}",
                out.name, out.size.w, out.size.h, out.loc.0, out.loc.1, out.scale
            );
        }
        // GNOME's primary monitor first: it carries the top bar.
        if let Some(primary) = outputs
            .iter()
            .position(|o| arrangement.get(&o.name).is_some_and(|c| c.primary))
        {
            let primary = outputs.remove(primary);
            outputs.insert(0, primary);
        }
        if outputs.is_empty() {
            return Err(DrmInitError(format!(
                "no connected output on {}",
                path.display()
            )));
        }

        let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
        libinput
            .udev_assign_seat(&seat)
            .map_err(|()| DrmInitError(format!("libinput: cannot assign seat {seat}")))?;
        let input = LibinputInputBackend::new(libinput.clone());
        let (first_w, first_h) = outputs[0].logical_size();
        let first_loc = outputs[0].loc;
        Ok((
            Self {
                session,
                drm,
                renderer,
                outputs,
                libinput,
                active: true,
                sleep_reset_pending: false,
                wake_scanout_blocked: false,
                wake_event_traces: 0,
                pointer: (
                    f64::from(first_loc.0) + f64::from(first_w) / 2.0,
                    f64::from(first_loc.1) + f64::from(first_h) / 2.0,
                )
                    .into(),
                ctrl: false,
                alt: false,
                devices: Vec::new(),
                input_settings: Default::default(),
                corner_pressure: Default::default(),
                hot_corner_active: true,
                relative_motion_events: 0,
            },
            DrmSources {
                session: session_notifier,
                drm: drm_notifier,
                input,
            },
        ))
    }

    /// Output rectangles in the global logical space.
    pub fn output_rects(&self) -> Vec<Rectangle<i32, Logical>> {
        self.outputs
            .iter()
            .map(|o| Rectangle::new(o.loc.into(), o.logical_size().into()))
            .collect()
    }

    /// Current pointer position (global logical space).
    pub fn pointer(&self) -> Point<f64, Logical> {
        self.pointer
    }

    /// Session pause/activate from libseat.
    pub fn on_session_event(&mut self, event: SessionEvent) {
        self.corner_pressure.reset();
        match event {
            SessionEvent::PauseSession => {
                eprintln!("roost-compositor: drm: session paused");
                self.active = false;
                self.libinput.suspend();
                self.drm.pause();
            }
            SessionEvent::ActivateSession => {
                eprintln!("roost-compositor: drm: session activated");
                if self.libinput.resume().is_err() {
                    eprintln!("roost-compositor: drm: libinput resume failed");
                }
                if let Err(e) = self.drm.activate(false) {
                    eprintln!("roost-compositor: drm: activate failed: {e}");
                }
                for out in &mut self.outputs {
                    out.surface.reset_buffers();
                    out.pending = false;
                    out.last_frame = None;
                    out.damage_tracker = None;
                }
                self.active = true;
                if self.sleep_reset_pending {
                    self.resume_from_sleep();
                }
            }
        }
    }

    /// Recover scanout after real system sleep, independently of VT events.
    pub fn resume_from_sleep(&mut self) {
        self.wake_scanout_blocked = true;
        if !self.active {
            // Never program KMS off-seat; retain the wake until activation.
            self.sleep_reset_pending = true;
            return;
        }
        self.sleep_reset_pending = false;
        eprintln!("roost-compositor: drm: system wake scanout reset");
        let trace = std::env::var_os("ROOST_LOCK_TRACE").is_some();
        self.wake_event_traces = if trace { 2 } else { 0 };
        // Pending and queued flips can be lost across S3. Drop both without
        // submitting an old queued scene or claiming presentation. Ordinary
        // frame_submitted() would submit queued_fb as a side effect.
        for out in &mut self.outputs {
            for mut feedback in out.surface.discard_pending_frames() {
                feedback.discarded();
            }
            out.surface.reset_buffers();
            out.pending = false;
            out.last_frame = None;
            out.damage_tracker = None;
            out.wake_trace = trace;
        }
        // Reset actual connector/plane state too: an active VT does not imply
        // that the kernel restored its framebuffer. The next locked frame
        // commits the retained modes and surfaces again.
        if let Err(error) = self.drm.reset_state() {
            eprintln!("roost-compositor: drm: wake KMS reset failed: {error}");
            // An unsuccessful reset cannot retire old kernel completions.
            // Stay masked without submitting a frame whose feedback could
            // be attached to an abandoned flip. VT activation can recover.
            self.sleep_reset_pending = true;
            return;
        }
        // The blocking disable commit has retired previous scanout. Drain
        // its already queued completions before the first new submission.
        // A valid modeset event can report the last vblank before submission,
        // so its timestamp cannot identify an abandoned pre-sleep frame.
        match drain_reset_events(|| self.drm.receive_events().map(|events| events.count())) {
            Ok(count) if trace => {
                eprintln!("roost-compositor: drm: wake obsolete events drained={count}");
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("roost-compositor: drm: wake event drain failed: {error}");
                self.sleep_reset_pending = true;
                return;
            }
        }
        self.wake_scanout_blocked = false;
        if trace {
            for out in &self.outputs {
                eprintln!(
                    "roost-compositor: drm: wake reset {} atomic={} commit_pending={} crtc={:?}",
                    out.name,
                    self.drm.is_atomic(),
                    out.surface.surface().commit_pending(),
                    self.drm.get_crtc(out.crtc),
                );
            }
        }
    }

    /// A failed wake reset/drain must not submit until recovery succeeds.
    pub fn scanout_ready(&self) -> bool {
        self.active && !self.wake_scanout_blocked
    }

    /// Page flip completion: the output may render again, and what its
    /// frame drew has been presented.
    pub fn on_drm_event(
        &mut self,
        event: DrmEvent,
        metadata: &mut Option<DrmEventMetadata>,
    ) -> Option<PageFlip> {
        match event {
            DrmEvent::VBlank(crtc) => {
                if self.wake_event_traces != 0 {
                    self.wake_event_traces -= 1;
                    eprintln!(
                        "roost-compositor: drm: wake pageflip crtc={crtc:?} metadata={metadata:?}",
                    );
                }
                let index = self.outputs.iter().position(|o| o.crtc == crtc)?;
                let out = &mut self.outputs[index];
                let feedback = match out.surface.frame_submitted() {
                    Ok(feedback) => feedback,
                    Err(e) => {
                        eprintln!("roost-compositor: drm: frame_submitted: {e}");
                        None
                    }
                };
                out.pending = false;
                let (time, sequence) = match metadata.as_ref() {
                    Some(meta) => (
                        match meta.time {
                            DrmEventTime::Monotonic(time) => Some(time),
                            DrmEventTime::Realtime(_) => None,
                        },
                        u64::from(meta.sequence),
                    ),
                    None => (None, 0),
                };
                Some(PageFlip {
                    primary: index == 0,
                    feedback,
                    time,
                    sequence,
                    refresh: crate::frame_timing::refresh_of(&out.output),
                })
            }
            DrmEvent::Error(e) => {
                eprintln!("roost-compositor: drm: device error: {e}");
                None
            }
        }
    }

    /// Translate one libinput event into manager inputs, handling the
    /// pieces only the hardware backend owns: relative pointer motion
    /// (accumulated, clamped) and Ctrl+Alt+F<n> VT switching (consumed).
    pub fn translate(
        &mut self,
        event: InputEvent<LibinputInputBackend>,
        layout: &[(Rectangle<i32, Logical>, bool)],
    ) -> Vec<ManagerInput> {
        match event {
            InputEvent::PointerMotion { event } => {
                self.relative_motion_events = self.relative_motion_events.saturating_add(1);
                let delta = event.delta();
                let rects = self.output_rects();
                let (attempt, corner) = self.corner_pressure.motion(
                    self.pointer,
                    delta,
                    event.time() / 1000,
                    layout,
                    self.input_settings.right_to_left,
                    self.active && self.hot_corner_active && self.input_settings.hot_corners,
                );
                self.pointer = clamp_to_outputs(attempt, &rects);
                let mut inputs = vec![
                    ManagerInput::Motion {
                        pos: self.pointer,
                        time: (event.time() / 1000) as u32,
                    },
                    // Raw motion for relative-pointer clients (#89).
                    ManagerInput::RelativeMotion {
                        delta,
                        delta_unaccel: event.delta_unaccel(),
                        utime: event.time(),
                    },
                ];
                if let Some(pos) = corner {
                    inputs.push(ManagerInput::CornerPressure {
                        pos,
                        time: (event.time() / 1000) as u32,
                    });
                }
                inputs
            }
            InputEvent::DeviceAdded { mut device } => {
                apply_libinput(&self.input_settings, &mut device);
                self.devices.push(device);
                Vec::new()
            }
            InputEvent::DeviceRemoved { device } => {
                self.corner_pressure.reset();
                self.devices.retain(|d| *d != device);
                Vec::new()
            }
            // Touchpad swipes (#60): three fingers are the shell's.
            InputEvent::GestureSwipeBegin { event } => {
                use smithay::backend::input::GestureBeginEvent;
                vec![ManagerInput::SwipeBegin {
                    fingers: event.fingers(),
                    time: (event.time() / 1000) as u32,
                }]
            }
            InputEvent::GestureSwipeUpdate { event } => {
                use smithay::backend::input::GestureSwipeUpdateEvent;
                vec![ManagerInput::SwipeUpdate {
                    delta: event.delta(),
                    time: (event.time() / 1000) as u32,
                }]
            }
            InputEvent::GestureSwipeEnd { event } => {
                use smithay::backend::input::GestureEndEvent;
                vec![ManagerInput::SwipeEnd {
                    cancelled: event.cancelled(),
                    time: (event.time() / 1000) as u32,
                }]
            }
            InputEvent::Keyboard { event } => {
                let code = u32::from(event.key_code()).saturating_sub(XKB_OFFSET);
                let pressed = event.state() == KeyState::Pressed;
                match code {
                    KEY_LEFTCTRL | KEY_RIGHTCTRL => self.ctrl = pressed,
                    KEY_LEFTALT | KEY_RIGHTALT => self.alt = pressed,
                    _ => {}
                }
                if pressed && self.ctrl && self.alt {
                    if let Some(vt) = vt_for_key(code) {
                        if let Err(e) = self.session.change_vt(vt) {
                            eprintln!("roost-compositor: drm: change_vt({vt}): {e}");
                        }
                        return Vec::new();
                    }
                }
                crate::windows::translate_input::<LibinputInputBackend>(
                    InputEvent::Keyboard { event },
                    self.primary_size(),
                )
            }
            other => {
                let inputs = crate::windows::translate_input(other, self.primary_size());
                for input in &inputs {
                    if let ManagerInput::Motion { pos, .. } = input {
                        self.corner_pressure.observe_position(*pos);
                        self.pointer = *pos;
                    }
                }
                inputs
            }
        }
    }

    /// GNOME's touchpad and mouse settings on every device, now and as
    /// devices arrive (#60).
    /// Count native relative delivery without recording movement or input text.
    pub fn relative_motion_events(&self) -> u64 {
        self.relative_motion_events
    }

    pub fn set_hot_corner_active(&mut self, active: bool) {
        self.hot_corner_active = active;
        if !active {
            self.corner_pressure.reset();
        }
    }

    pub fn apply_input_settings(&mut self, settings: &roost_shell_control::InputSettings) {
        if self.input_settings.hot_corners != settings.hot_corners
            || self.input_settings.right_to_left != settings.right_to_left
        {
            self.corner_pressure.reset();
        }
        self.input_settings = settings.clone();
        for device in &mut self.devices {
            apply_libinput(settings, device);
        }
    }

    /// Move the drawn cursor to where the window manager put the pointer.
    pub fn set_pointer(&mut self, pos: Point<f64, Logical>) {
        if pos != self.pointer {
            self.corner_pressure.observe_position(pos);
        }
        self.pointer = pos;
    }

    fn primary_size(&self) -> Size<i32, Logical> {
        self.outputs
            .first()
            .map(|o| (o.size.w, o.size.h).into())
            .unwrap_or_else(|| (1, 1).into())
    }
}

/// Bound nonblocking 1024-byte DRM event reads after the synchronous reset.
/// No new frames are submitted until the old queue reaches WouldBlock.
fn drain_reset_events(
    mut receive: impl FnMut() -> std::io::Result<usize>,
) -> std::io::Result<usize> {
    let mut drained = 0;
    for _ in 0..32 {
        match receive() {
            Ok(0) => return Ok(drained),
            Ok(count) => drained += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(drained),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other(
        "DRM event queue did not become empty after reset",
    ))
}

#[cfg(test)]
mod wake_event_tests {
    use super::drain_reset_events;
    use std::io::{Error, ErrorKind};

    #[test]
    fn old_completions_are_drained_before_new_submission() {
        let mut reads = [
            Ok(2),
            Err(Error::from(ErrorKind::Interrupted)),
            Ok(1),
            Err(Error::from(ErrorKind::WouldBlock)),
        ]
        .into_iter();
        assert_eq!(drain_reset_events(|| reads.next().unwrap()).unwrap(), 3);
        assert!(reads.next().is_none());
    }

    #[test]
    fn unbounded_or_failed_drain_never_qualifies_as_empty() {
        let mut calls = 0;
        assert!(drain_reset_events(|| {
            calls += 1;
            Ok(1)
        })
        .is_err());
        assert_eq!(calls, 32);
        assert_eq!(
            drain_reset_events(|| Err(Error::from(ErrorKind::PermissionDenied)))
                .unwrap_err()
                .kind(),
            ErrorKind::PermissionDenied
        );
    }
}

/// One device's libinput configuration from GNOME's settings, as Mutter
/// applies them (niri's `apply_libinput_settings` shape): touchpads get
/// tap-to-click, natural scroll, disable-while-typing and their speed;
/// other pointers their natural scroll and speed.
fn apply_libinput(
    settings: &roost_shell_control::InputSettings,
    device: &mut smithay::reexports::input::Device,
) {
    let speed = |milli: i32| f64::from(milli.clamp(-1000, 1000)) / 1000.0;
    if device.config_tap_finger_count() > 0 {
        apply_handedness(device, settings.touchpad_left_handed);
        let _ = device.config_tap_set_enabled(settings.tap_to_click);
        let _ = device.config_scroll_set_natural_scroll_enabled(settings.touchpad_natural_scroll);
        let _ = device.config_dwt_set_enabled(settings.disable_while_typing);
        let _ = device.config_accel_set_speed(speed(settings.touchpad_speed_milli));
    } else if device.has_capability(smithay::reexports::input::DeviceCapability::Pointer) {
        apply_handedness(device, settings.mouse_left_handed);
        let _ = device.config_scroll_set_natural_scroll_enabled(settings.mouse_natural_scroll);
        let _ = device.config_accel_set_speed(speed(settings.mouse_speed_milli));
    }
}

fn apply_handedness(device: &smithay::reexports::input::Device, left_handed: bool) {
    if device.config_left_handed_is_available() {
        if let Err(error) = device.config_left_handed_set(left_handed) {
            eprintln!(
                "roost-compositor: libinput: primary button setting rejected for {}: {error:?}",
                device.name()
            );
        }
    } else if left_handed {
        eprintln!(
            "roost-compositor: libinput: primary button swapping unsupported for {}",
            device.name()
        );
    }
}

/// Rectangles in an output's physical pixel space.
pub type PixelRects = Vec<Rectangle<i32, Physical>>;

/// Software pointer: an arrow drawn as stacked rectangles (outline
/// first, fill second), relative to the hotspot at `pos` and offset by
/// the output's location. Returns `(outline, fill)` damage-style rects.
pub fn cursor_rects(
    pos: Point<f64, Logical>,
    output_loc: (i32, i32),
    scale: f64,
) -> (PixelRects, PixelRects) {
    let x = ((pos.x - f64::from(output_loc.0)) * scale).round() as i32;
    let y = ((pos.y - f64::from(output_loc.1)) * scale).round() as i32;
    // The arrow grows by whole pixels with the scale, staying crisp.
    let k = (scale.round() as i32).max(1);
    let rect = |dx: i32, dy: i32, w: i32| -> Rectangle<i32, Physical> {
        Rectangle::new((x + dx * k, y + dy * k).into(), (w * k, k).into())
    };
    let mut outline = Vec::new();
    let mut fill = Vec::new();
    // Left-aligned triangle, 12 rows tall, plus a short tail.
    for row in 0..12 {
        outline.push(rect(0, row, row + 2));
        if row > 0 && row < 11 {
            fill.push(rect(1, row, row));
        }
    }
    for row in 12..17 {
        outline.push(rect(4, row, 4));
        fill.push(rect(5, row, 2));
    }
    (outline, fill)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_scales_with_the_output() {
        // At scale 2 the arrow sits at the scaled position, twice as big.
        let (outline, _) = cursor_rects((110.0, 20.0).into(), (100, 0), 2.0);
        assert_eq!(outline[0].loc, (20, 40).into());
        assert_eq!(outline[0].size, (4, 2).into());
        let (outline, _) = cursor_rects((110.0, 20.0).into(), (100, 0), 1.0);
        assert_eq!(outline[0].size, (2, 1).into());
    }

    #[test]
    fn function_keys_map_to_vts() {
        assert_eq!(vt_for_key(KEY_F1), Some(1));
        assert_eq!(vt_for_key(KEY_F10), Some(10));
        assert_eq!(vt_for_key(KEY_F11), Some(11));
        assert_eq!(vt_for_key(KEY_F12), Some(12));
        assert_eq!(vt_for_key(30), None);
    }

    #[test]
    fn pointer_clamps_into_the_output_union() {
        let outputs = [
            Rectangle::new((0, 0).into(), (1920, 1080).into()),
            Rectangle::new((1920, 0).into(), (1280, 800).into()),
        ];
        let inside: Point<f64, Logical> = (2000.0, 100.0).into();
        assert_eq!(clamp_to_outputs(inside, &outputs), inside);
        // Below the shorter right-hand output: pulled up onto it.
        let below = clamp_to_outputs((2500.0, 1000.0).into(), &outputs);
        assert_eq!(below, (2500.0, 799.0).into());
        // Far left: onto the left edge of the first output.
        let left = clamp_to_outputs((-50.0, 10.0).into(), &outputs);
        assert_eq!(left, (0.0, 10.0).into());
    }

    #[test]
    fn cursor_is_offset_by_output_location() {
        let (outline, fill) = cursor_rects((1930.0, 5.0).into(), (1920, 0), 1.0);
        assert_eq!(outline[0].loc, (10, 5).into());
        assert!(!fill.is_empty());
    }
}
