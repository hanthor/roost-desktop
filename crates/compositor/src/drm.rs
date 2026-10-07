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

use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::Fourcc;
use smithay::backend::drm::{
    DrmCookieEventMetadata as DrmEventMetadata, DrmDevice,
    DrmDeviceCookieNotifier as DrmDeviceNotifier, DrmDeviceFd, DrmEvent, DrmEventTime,
    GbmBufferedSurface,
};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::input::{Event, InputEvent, KeyState, KeyboardKeyEvent, PointerMotionEvent};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{UdevBackend, UdevEvent};
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
    pub completed: bool,
    pub queued_origin: Option<crate::monitor_scanout::Queued>,
    /// Feedback of the surfaces the frame drew.
    pub feedback: Option<OutputPresentationFeedback>,
    /// Kernel vblank timestamp on CLOCK_MONOTONIC, when it gave one.
    pub time: Option<std::time::Duration>,
    /// Kernel vblank sequence.
    pub sequence: u64,
    /// The output's refresh interval.
    pub refresh: std::time::Duration,
    pub output: Output,
    /// Present only for a real owned pending frame completed successfully.
    pub surface_ids: Option<Vec<smithay::backend::renderer::element::Id>>,
}

/// Actual compositor-thread KMS snapshot used by discovery/reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPlan {
    pub name: String,
    pub connector: connector::Handle,
    pub crtc: crtc::Handle,
    pub mode: smithay::reexports::drm::control::Mode,
    pub millimeters: (u32, u32),
}

fn plan_geometry_valid(name: &str, mode: (u16, u16), millimeters: (u32, u32)) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && mode.0 != 0
        && mode.1 != 0
        && millimeters.0 <= i32::MAX as u32
        && millimeters.1 <= i32::MAX as u32
}

fn validate_plan(plan: &OutputPlan) -> Result<(), String> {
    if plan_geometry_valid(&plan.name, plan.mode.size(), plan.millimeters) {
        Ok(())
    } else {
        Err("invalid bounded KMS output geometry".into())
    }
}

fn protocol_output(
    plan: &OutputPlan,
    native_edid: Option<&crate::monitor_edid::Observation>,
) -> Output {
    let output = Output::new(
        plan.name.clone(),
        PhysicalProperties {
            size: (plan.millimeters.0 as i32, plan.millimeters.1 as i32).into(),
            subpixel: Subpixel::Unknown,
            make: native_edid
                .map(|edid| edid.identity.vendor.clone())
                .unwrap_or_default(),
            // Connector label is UI fallback, never persistent EDID identity.
            model: native_edid
                .map(|edid| edid.identity.product.clone())
                .unwrap_or_else(|| plan.name.clone()),
        },
    );
    let mode = Mode::from(plan.mode);
    output.change_current_state(Some(mode), None, None, None);
    output.set_preferred(mode);
    output
}

fn create_output(
    drm: &mut DrmDevice,
    gbm: &GbmDevice<DrmDeviceFd>,
    render_formats: &FormatSet,
    plan: &OutputPlan,
    native_owner: Option<roost_shell_control::NativeOutputInfo>,
    native_edid: Option<crate::monitor_edid::Observation>,
) -> Result<DrmOutput, String> {
    validate_plan(plan)?;
    let surface = drm
        .create_surface(plan.crtc, plan.mode, &[plan.connector])
        .map_err(|error| error.to_string())?;
    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let surface = GbmBufferedSurface::new(
        surface,
        allocator,
        &[Fourcc::Argb8888, Fourcc::Xrgb8888],
        render_formats.iter().copied(),
    )
    .map_err(|error| error.to_string())?;
    let (w, h) = plan.mode.size();
    let output = protocol_output(plan, native_edid.as_ref());
    Ok(DrmOutput {
        plan: plan.clone(),
        name: plan.name.clone(),
        crtc: plan.crtc,
        connector: plan.connector,
        native_owner,
        native_edid,
        surface,
        output,
        size: (i32::from(w), i32::from(h)).into(),
        loc: (0, 0),
        scale: 1.0,
        pending: false,
        pending_surface_ids: Vec::new(),
        pending_origin: None,
        last_kernel_completion: None,
        kernel_completion_count: 0,
        rejected_kernel_cookie_count: 0,
        kernel_queue_error_count: 0,
        last_frame: None,
        damage_tracker: None,
        wake_trace: false,
    })
}

fn kernel_scanout_readback(
    drm: &DrmDevice,
    output: &DrmOutput,
) -> Result<crate::monitor_scanout::Readback, String> {
    use crate::monitor_scanout::Readback;
    let current = drm
        .get_crtc(output.crtc)
        .map_err(|error| error.to_string())?;
    if drm.is_atomic() {
        let plane = drm
            .get_plane(output.surface.surface().plane())
            .map_err(|error| error.to_string())?;
        Ok(Readback {
            crtc: current.handle().into(),
            plane: Some(plane.handle().into()),
            plane_crtc: plane.crtc().map(u32::from),
            framebuffer: plane.framebuffer().map(u32::from),
            mode_matches: current.mode() == Some(output.plan.mode),
        })
    } else {
        Ok(Readback {
            crtc: current.handle().into(),
            plane: None,
            plane_crtc: None,
            framebuffer: current.framebuffer().map(u32::from),
            mode_matches: current.mode() == Some(output.plan.mode),
        })
    }
}

fn discard_pending_output(output: &mut DrmOutput) {
    for mut feedback in output.surface.discard_pending_frames() {
        feedback.discarded();
    }
    output.pending = false;
    output.pending_surface_ids.clear();
    output.pending_origin = None;
    output.last_frame = None;
    output.damage_tracker = None;
}

/// Validated acquisition candidate, still awaiting serialized reconciliation.
/// Its existence is not an accepted output inventory or restored authority.
pub struct MonitorDiscovery {
    pub ticket: crate::monitor_refresh::Ticket,
    pub plans: Vec<OutputPlan>,
    pub metadata: Vec<crate::monitor_worker::Metadata>,
}

/// One lit connector.
pub struct DrmOutput {
    plan: OutputPlan,
    /// Inventory name, e.g. `eDP-1`.
    pub name: String,
    /// CRTC driving the connector.
    pub crtc: crtc::Handle,
    pub connector: connector::Handle,
    native_owner: Option<roost_shell_control::NativeOutputInfo>,
    /// Optional EDID observed during original connector discovery. Never authority.
    native_edid: Option<crate::monitor_edid::Observation>,
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
    pub pending_surface_ids: Vec<smithay::backend::renderer::element::Id>,
    pub pending_origin: Option<crate::monitor_scanout::Queued>,
    /// Historical actual callback receipt, never retained authority.
    last_kernel_completion: Option<crate::monitor_scanout::KernelCompletion>,
    kernel_completion_count: u64,
    rejected_kernel_cookie_count: u64,
    pub kernel_queue_error_count: u64,
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
    gbm: GbmDevice<DrmDeviceFd>,
    render_formats: FormatSet,
    /// Lit outputs, primary first.
    pub outputs: Vec<DrmOutput>,
    /// libinput context, suspended while the session is paused.
    pub libinput: Libinput,
    /// Whether the session currently owns the VT.
    pub active: bool,
    monitor_refresh: crate::monitor_refresh::RefreshState,
    monitor_event_fault: crate::monitor_refresh::EventFault,
    monitor_worker: Option<crate::monitor_worker::Worker>,
    monitor_scanout: Option<crate::monitor_scanout::Pending>,
    discovery_plans: Option<(crate::monitor_refresh::Ticket, Vec<OutputPlan>)>,
    sleep_reset_pending: bool,
    wake_scanout_blocked: bool,
    /// Resource inventory is publishable only after a real serialized reset/reconcile.
    monitor_resources_known: bool,
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
    /// Genuine DRM device changes for the actual seat.
    pub udev: UdevBackend,
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
    fn original_monitor_fd_valid(&self) -> bool {
        use std::os::fd::AsFd;
        self.active
            && self.drm.is_active()
            && crate::native_output::device(self.drm.device_fd().as_fd()).ok()
                == Some(self.drm.device_id())
    }

    /// Schedule on an actual refresh boundary or one allowed recovery. Busy
    /// orphan workers defer the request before any KMS probe/attempt is spent.
    /// Forced KMS probe can block; the deadline rejects late observations but
    /// does not cancel that ioctl or a worker's kernel read.
    pub fn start_monitor_refresh(&mut self) -> Result<bool, String> {
        use std::os::fd::AsFd;
        if !self.monitor_refresh.can_begin() || !crate::monitor_worker::Worker::available() {
            return Ok(false);
        }
        let valid = self.original_monitor_fd_valid();
        let Some(ticket) = self.monitor_refresh.begin(std::time::Instant::now(), valid) else {
            return Ok(false);
        };
        let prepared = (|| {
            // Constructor already force-probed the genuine startup boundary.
            // Epoch 1 rechecks that original plan without a second force probe;
            // actual later hotplug/activation/wake boundaries force refresh.
            let plans = self.scan_output_plans(ticket.epoch != 1)?;
            let original_fd = self
                .drm
                .device_fd()
                .as_fd()
                .try_clone_to_owned()
                .map_err(|error| error.to_string())?;
            let connectors = plans
                .iter()
                .map(|plan| crate::monitor_worker::Connector {
                    name: plan.name.clone(),
                    id: u32::from(plan.connector),
                })
                .collect();
            Ok::<_, String>((
                plans,
                crate::monitor_worker::Request {
                    ticket,
                    original_fd,
                    connectors,
                },
            ))
        })();
        let (plans, request) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let valid = self.original_monitor_fd_valid();
                self.monitor_refresh
                    .completed(ticket, std::time::Instant::now(), valid, false);
                return Err(error);
            }
        };
        match crate::monitor_worker::Worker::start(request) {
            Ok(worker) => {
                self.monitor_worker = Some(worker);
                self.discovery_plans = Some((ticket, plans));
                Ok(true)
            }
            Err(crate::monitor_worker::Failure::Busy) => {
                self.monitor_refresh.deferred_busy(ticket);
                Ok(false)
            }
            Err(_) => {
                let valid = self.original_monitor_fd_valid();
                self.monitor_refresh
                    .completed(ticket, std::time::Instant::now(), valid, false);
                Err("monitor metadata worker could not start".into())
            }
        }
    }

    /// Cheap polling until a real worker exit, then serialized fresh KMS plan
    /// equality. No sysfs reads run here. Accepted authority remains revoked
    /// until the caller reconciles all resources/snapshots successfully.
    pub fn poll_monitor_discovery(&mut self) -> Option<Result<MonitorDiscovery, String>> {
        if !self
            .monitor_worker
            .as_ref()
            .is_some_and(|worker| worker.ready())
        {
            return None;
        }
        let worker = self.monitor_worker.take()?;
        let completed = match worker.take() {
            Ok(completed) => completed,
            Err(worker) => {
                self.monitor_worker = Some(worker);
                return None;
            }
        };
        let valid = self.original_monitor_fd_valid();
        let prepared = self.discovery_plans.take();
        let result = (|| {
            let (ticket, plans) = prepared.ok_or("monitor acquisition plan absent")?;
            if ticket != completed.ticket
                || !self
                    .monitor_refresh
                    .accepts(ticket, std::time::Instant::now(), valid)
            {
                return Err("monitor acquisition ownership/epoch/deadline changed".into());
            }
            let metadata = completed
                .result
                .map_err(|_| "monitor metadata acquisition failed")?;
            if metadata.len() != plans.len()
                || metadata.iter().zip(&plans).any(|(metadata, plan)| {
                    metadata.connector.name != plan.name
                        || metadata.connector.id != u32::from(plan.connector)
                })
            {
                return Err("monitor acquisition metadata/plan mismatch".into());
            }
            if self.scan_output_plans(false)? != plans {
                return Err("monitor KMS plan changed during discovery".into());
            }
            // The final KMS query may block: its old pre-query timestamp and
            // FD observation cannot authorize the result after it returns.
            if !self.monitor_refresh.accepts(
                ticket,
                std::time::Instant::now(),
                self.original_monitor_fd_valid(),
            ) {
                return Err(
                    "monitor discovery expired or lost ownership during final probe".into(),
                );
            }
            Ok(MonitorDiscovery {
                ticket,
                plans,
                metadata,
            })
        })();
        if result.is_err() {
            let valid = self.original_monitor_fd_valid();
            self.monitor_refresh.completed(
                completed.ticket,
                std::time::Instant::now(),
                valid,
                false,
            );
        }
        Some(result)
    }

    /// Serialize resource mutation on the compositor thread. Errors leave
    /// actual partial resources, revoked authority and masked scanout; callers
    /// must reconcile public inventories from self.outputs even on failure.
    pub fn reconcile_monitor_resources(
        &mut self,
        discovery: MonitorDiscovery,
    ) -> Result<crate::monitor_refresh::Ticket, String> {
        let ticket = discovery.ticket;
        let result = (|| {
            if !self.monitor_refresh.accepts(
                ticket,
                std::time::Instant::now(),
                self.original_monitor_fd_valid(),
            ) || self.scan_output_plans(false)? != discovery.plans
            {
                return Err("monitor candidate no longer owns current KMS plan".into());
            }
            if !self.monitor_refresh.accepts(
                ticket,
                std::time::Instant::now(),
                self.original_monitor_fd_valid(),
            ) {
                return Err("monitor candidate expired before resource commit".into());
            }
            for plan in &discovery.plans {
                validate_plan(plan)?;
            }
            self.wake_scanout_blocked = true;
            self.monitor_resources_known = false;
            // Retire old kernel scanout before CRTC reuse. Do not rely on a
            // DrmSurface Drop implementation whose clear errors are warnings.
            self.drm.reset_state().map_err(|error| error.to_string())?;
            for output in &mut self.outputs {
                for mut feedback in output.surface.discard_pending_frames() {
                    feedback.discarded();
                }
                output.surface.reset_buffers();
                output.pending = false;
                output.pending_surface_ids.clear();
                output.pending_origin = None;
                output.last_frame = None;
                output.damage_tracker = None;
            }
            drain_reset_events(|| receive_cookie_events(&self.drm))
                .map_err(|error| format!("monitor reset event drain failed: {error}"))?;
            let mut previous = std::mem::take(&mut self.outputs);
            // Only successfully retained/created resources enter the public vector.
            self.monitor_resources_known = true;
            // Remove obsolete planes/surfaces before creating a new owner for
            // their CRTC. Surviving unchanged surface resources are retained.
            previous.retain(|output| {
                discovery
                    .plans
                    .iter()
                    .any(|plan| plan.connector == output.connector && plan.crtc == output.crtc)
            });
            let mut next_x = None;
            for output in &previous {
                let (width, height) = output.logical_size();
                let right = output
                    .loc
                    .0
                    .checked_add(width)
                    .ok_or("monitor right edge overflow")?;
                output
                    .loc
                    .1
                    .checked_add(height)
                    .ok_or("monitor bottom edge overflow")?;
                next_x = Some(next_x.map_or(right, |current: i32| current.max(right)));
            }
            let mut next_x = next_x.unwrap_or(0);
            let default_scale = previous.first().map(|output| output.scale).unwrap_or(1.0);
            for (plan, metadata) in discovery.plans.into_iter().zip(discovery.metadata) {
                if !self.monitor_refresh.accepts(
                    ticket,
                    std::time::Instant::now(),
                    self.original_monitor_fd_valid(),
                ) {
                    return Err("monitor candidate expired during resource commit".into());
                }
                let mut output = match previous.iter().position(|output| {
                    output.connector == plan.connector && output.crtc == plan.crtc
                }) {
                    Some(index) => {
                        let mut output = previous.remove(index);
                        if output.surface.pending_mode() != plan.mode {
                            output
                                .surface
                                .use_mode(plan.mode)
                                .map_err(|error| error.to_string())?;
                        }
                        let props = output.output.physical_properties();
                        let make = metadata
                            .edid
                            .as_ref()
                            .map(|edid| edid.identity.vendor.as_str())
                            .unwrap_or("");
                        let model = metadata
                            .edid
                            .as_ref()
                            .map(|edid| edid.identity.product.as_str())
                            .unwrap_or(&plan.name);
                        // Epoch revocation cleared optional owner/EDID authority,
                        // not immutable display properties. Avoid replacing an
                        // unchanged protocol object merely because VT paused.
                        let properties_changed = props.make != make || props.model != model;
                        if output.plan != plan || properties_changed {
                            output.output = protocol_output(&plan, metadata.edid.as_ref());
                        }
                        output.plan = plan.clone();
                        output.native_owner = metadata.owner;
                        output.native_edid = metadata.edid;
                        let (w, h) = plan.mode.size();
                        output.size = (i32::from(w), i32::from(h)).into();
                        output
                    }
                    None => {
                        let mut output =
                            self.create_planned_output(&plan, metadata.owner, metadata.edid)?;
                        output.loc = (next_x, 0);
                        output.scale = default_scale;
                        output
                    }
                };
                let (width, height) = output.logical_size();
                let right = output
                    .loc
                    .0
                    .checked_add(width)
                    .ok_or("monitor right edge overflow")?;
                output
                    .loc
                    .1
                    .checked_add(height)
                    .ok_or("monitor bottom edge overflow")?;
                next_x = next_x.max(right);
                output.output.change_current_state(
                    None,
                    None,
                    Some(if output.scale == 1.0 {
                        smithay::output::Scale::Integer(1)
                    } else {
                        smithay::output::Scale::Fractional(output.scale)
                    }),
                    Some(output.loc.into()),
                );
                self.outputs.push(output);
            }
            if !self.monitor_refresh.accepts(
                ticket,
                std::time::Instant::now(),
                self.original_monitor_fd_valid(),
            ) {
                return Err("monitor candidate expired after resource commit".into());
            }
            self.pointer = clamp_to_outputs(self.pointer, &self.output_rects());
            self.corner_pressure.reset();
            let crtcs = self
                .outputs
                .iter()
                .map(|output| u32::from(output.crtc))
                .collect::<Vec<_>>();
            self.monitor_scanout = Some(crate::monitor_scanout::Pending::new(ticket, &crtcs)?);
            // Permit provisional scanout so the actual modeset can complete.
            // Accepted ownership/capture/current-state publication stays revoked.
            self.wake_scanout_blocked = false;
            Ok(ticket)
        })();
        if result.is_err() {
            let valid = self.original_monitor_fd_valid();
            self.monitor_refresh
                .completed(ticket, std::time::Instant::now(), valid, false);
            self.invalidate_monitor_metadata();
        }
        result
    }

    /// Call only after real output globals/layout/windows/pointer/capture and
    /// public snapshots have been reconciled. No discovery-only authority.
    pub fn finish_monitor_reconciliation(
        &mut self,
        ticket: crate::monitor_refresh::Ticket,
    ) -> bool {
        let valid = self.original_monitor_fd_valid();
        let complete = self
            .monitor_scanout
            .as_ref()
            .is_some_and(|pending| pending.ticket == ticket && pending.complete());
        if !complete {
            return false;
        }
        self.monitor_scanout = None;
        let accepted =
            self.monitor_refresh
                .completed(ticket, std::time::Instant::now(), valid, true)
                == crate::monitor_refresh::Completion::Accepted;
        if accepted {
            self.wake_scanout_blocked = false;
        } else {
            self.invalidate_monitor_metadata();
            self.monitor_resources_known = false;
            self.wake_scanout_blocked = true;
        }
        accepted
    }

    pub fn monitor_queue_failed(&mut self, crtc: u32) {
        if let Some(pending) = self.monitor_scanout.as_mut() {
            if pending.requires(crtc) {
                pending.fail();
            }
        }
    }

    /// Commit-correlation lifecycle epoch, not accepted monitor authority.
    pub fn monitor_commit_epoch(&self) -> Option<u64> {
        if !self.original_monitor_fd_valid() {
            return None;
        }
        self.monitor_refresh.commit_epoch()
    }

    pub fn pending_monitor_epoch(&self) -> Option<u64> {
        self.monitor_scanout
            .as_ref()
            .map(|pending| pending.ticket.epoch)
    }

    /// Pure polling: no ioctls or filesystem reads. A missing/failed real flip
    /// never becomes accepted merely because resources were allocated.
    pub fn poll_monitor_scanout_completion(
        &mut self,
    ) -> Option<Result<crate::monitor_refresh::Ticket, String>> {
        let pending = self.monitor_scanout.as_ref()?;
        let ticket = pending.ticket;
        let admitted = self.monitor_refresh.accepts(
            ticket,
            std::time::Instant::now(),
            self.original_monitor_fd_valid(),
        );
        if !admitted || pending.failed() {
            self.monitor_scanout = None;
            self.wake_scanout_blocked = true;
            self.monitor_resources_known = false;
            self.invalidate_monitor_metadata();
            let valid = self.original_monitor_fd_valid();
            self.monitor_refresh
                .completed(ticket, std::time::Instant::now(), valid, false);
            return Some(Err("original-epoch scanout failed or expired".into()));
        }
        pending.complete().then_some(Ok(ticket))
    }

    /// Collect a bounded real KMS plan on the compositor thread only.
    /// Force-probing is reserved for genuine hotplug/activation/wake requests:
    /// drm-rs documents that this refreshes EDID but can block/flicker. The
    /// worker deadline cannot bound this kernel ioctl. Completion comparison
    /// uses force_probe=false and never races a threaded KMS/renderer mutation.
    pub fn scan_output_plans(&self, force_probe: bool) -> Result<Vec<OutputPlan>, String> {
        use std::os::fd::AsFd;
        if !self.active
            || !self.drm.is_active()
            || !self.monitor_refresh.original_available()
            || crate::native_output::device(self.drm.device_fd().as_fd()).ok()
                != Some(self.drm.device_id())
        {
            return Err("original active KMS ownership unavailable".into());
        }
        let resources = self
            .drm
            .resource_handles()
            .map_err(|error| error.to_string())?;
        if resources.connectors().len() > crate::monitor_worker::MAX_CONNECTORS {
            return Err("KMS connector inventory exceeds discovery bound".into());
        }
        let mut connected = Vec::new();
        for handle in resources.connectors() {
            let info = self
                .drm
                .get_connector(*handle, force_probe)
                .map_err(|error| error.to_string())?;
            if info.state() != connector::State::Connected {
                continue;
            }
            if info.modes().len() > 256 {
                return Err("KMS connector modes exceed discovery bound".into());
            }
            if info.modes().is_empty() {
                continue;
            }
            connected.push(info);
        }
        // Reserve surviving CRTC assignments before allocating new connectors.
        connected.sort_by_key(|info| {
            self.outputs
                .iter()
                .position(|output| output.connector == info.handle())
                .unwrap_or(usize::MAX)
        });
        let mut used = Vec::new();
        let mut plans = Vec::new();
        for info in connected {
            let previous = self
                .outputs
                .iter()
                .find(|output| output.connector == info.handle());
            let mut compatible = Vec::new();
            for encoder in info.encoders() {
                let encoder = self
                    .drm
                    .get_encoder(*encoder)
                    .map_err(|error| error.to_string())?;
                compatible.extend(resources.filter_crtcs(encoder.possible_crtcs()));
            }
            let crtc = previous
                .map(|output| output.crtc)
                .filter(|crtc| compatible.contains(crtc) && !used.contains(crtc))
                .or_else(|| compatible.into_iter().find(|crtc| !used.contains(crtc)))
                .ok_or("no free CRTC for connected output")?;
            let mode = previous
                .map(|output| output.surface.surface().pending_mode())
                .filter(|mode| info.modes().contains(mode))
                .or_else(|| {
                    info.modes()
                        .iter()
                        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
                        .copied()
                })
                .or_else(|| info.modes().first().copied())
                .ok_or("connected output has no mode")?;
            used.push(crtc);
            plans.push(OutputPlan {
                name: format!("{}-{}", info.interface().as_str(), info.interface_id()),
                connector: info.handle(),
                crtc,
                mode,
                millimeters: info.size().unwrap_or((0, 0)),
            });
        }
        Ok(plans)
    }

    /// Factory runs only on the compositor thread, serialized with KMS/render.
    /// Reconciliation must validate the current original plan before calling.
    pub fn create_planned_output(
        &mut self,
        plan: &OutputPlan,
        owner: Option<roost_shell_control::NativeOutputInfo>,
        edid: Option<crate::monitor_edid::Observation>,
    ) -> Result<DrmOutput, String> {
        create_output(
            &mut self.drm,
            &self.gbm,
            &self.render_formats,
            plan,
            owner,
            edid,
        )
    }

    /// Unknown reset/removed-original/FD-loss states expose no active outputs.
    /// Private handles remain available for teardown; this never asserts that a
    /// physical connector is disconnected merely because the backend failed.
    pub fn monitor_layout_available(&self) -> bool {
        use std::os::fd::AsFd;
        self.monitor_resources_known
            && self.monitor_refresh.original_device_present()
            && crate::native_output::device(self.drm.device_fd().as_fd()).ok()
                == Some(self.drm.device_id())
    }

    /// Original protocol globals may survive a benign changed/VT event, but
    /// active capture/display/brightness publication requires accepted epoch.
    pub fn monitor_inventory_available(&self) -> bool {
        self.monitor_layout_available() && self.native_ownership_generation().is_some()
    }

    /// Actual backend discovery generation, revoked during pending refresh.
    pub fn native_ownership_generation(&self) -> Option<u64> {
        use std::os::fd::AsFd;
        if !self.active
            || !self.drm.is_active()
            || crate::native_output::device(self.drm.device_fd().as_fd()).ok()
                != Some(self.drm.device_id())
        {
            return None;
        }
        self.monitor_refresh.ownership_generation()
    }

    /// Bounded historical callback receipts plus independently current native
    /// authority. Counters never identify unobserved kernel work as completed.
    pub fn kernel_commit_observation(&self, layout_generation: u64) -> serde_json::Value {
        serde_json::json!({
            "current_native_generation": self.native_ownership_generation(),
            "current_layout_generation": layout_generation,
            "commit_api": if self.drm.is_atomic() { "atomic" } else { "legacy" },
            "outputs": self.outputs.iter().take(64).map(|output| serde_json::json!({
                "crtc": u32::from(output.crtc),
                "last_completed": output.last_kernel_completion,
                "completion_count": output.kernel_completion_count,
                "rejected_cookie_count": output.rejected_kernel_cookie_count,
                "queue_error_count": output.kernel_queue_error_count,
                "accepted_pending_cookie": output.surface.pending_commit_cookie().map(|cookie| cookie.get()),
            })).collect::<Vec<_>>(),
        })
    }

    pub fn native_outputs(&self) -> Vec<roost_shell_control::NativeOutputInfo> {
        use std::os::fd::AsFd;
        if !self.active || !self.drm.is_active() || self.native_ownership_generation().is_none() {
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

    /// Cached discovery metadata, gated by active VT, the original KMS FD,
    /// and unchanged cached output ownership. No connector file I/O occurs on
    /// this frame/snapshot path. Future restore admission must freshly validate
    /// the connector and metadata; this cache does not detect EDID hotplug.
    pub fn monitor_identity(&self, name: &str) -> Option<&crate::monitor_edid::Identity> {
        let owners = self.native_outputs();
        let output = self.outputs.iter().find(|output| output.name == name)?;
        let owner = output.native_owner.as_ref()?;
        if !owners.contains(owner) {
            return None;
        }
        output.native_edid.as_ref()?.cached_identity(owner)
    }

    /// Optional cached discovery metadata. The existing native-output gate
    /// checks active VT and the original char-device FD without reading sysfs.
    /// Connector/EDID reads occur only at actual discovery, not render ticks.
    pub fn monitor_identities(&self) -> Vec<roost_shell_control::MonitorIdentityInfo> {
        self.native_outputs()
            .into_iter()
            .map(|owner| {
                let edid = self
                    .outputs
                    .iter()
                    .find(|output| output.native_owner.as_ref() == Some(&owner))
                    .and_then(|output| output.native_edid.as_ref())
                    .and_then(|edid| edid.cached_identity(&owner))
                    .map(roost_shell_control::EdidIdentityInfo::from);
                roost_shell_control::MonitorIdentityInfo { owner, edid }
            })
            .collect()
    }

    /// Open the seat, the primary GPU, every connected output, and
    /// libinput. Fails with a readable reason (no seat daemon, no GPU,
    /// nothing connected) so the launcher can report it.
    pub fn new() -> Result<(Self, DrmSources), DrmInitError> {
        let (mut session, session_notifier) =
            LibSeatSession::new().map_err(|e| err("libseat session")(e.to_string()))?;
        let seat = session.seat();
        let udev = UdevBackend::new(&seat).map_err(|e| err("drm udev monitor")(e.to_string()))?;
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
        // The cookie path never synthesizes a CRTC from userdata.
        if smithay::reexports::drm::Device::get_driver_capability(
            &fd,
            smithay::reexports::drm::DriverCapability::CRTCInVBlankEvent,
        )
        .map_err(|e| err("drm event CRTC capability")(e.to_string()))?
            != 1
        {
            return Err(err("drm event CRTC capability")(
                "actual kernel CRTC event identity unavailable".into(),
            ));
        }
        let drm_notifier = drm_notifier.with_commit_cookies();
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
            let Ok(info) = drm.get_connector(*handle, true) else {
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
            let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
            let plan = OutputPlan {
                name: name.clone(),
                connector: *handle,
                crtc,
                mode,
                millimeters: info.size().unwrap_or((0, 0)),
            };
            let native_owner = native_device.and_then(|device| {
                crate::native_output::resolve(
                    std::path::Path::new("/sys"),
                    device,
                    &name,
                    u32::from(*handle),
                )
                .ok()
            });
            let native_edid = native_owner.as_ref().and_then(|owner| {
                crate::monitor_edid::read_owned(drm.device_fd().as_fd(), owner)
                    .ok()
                    .flatten()
            });
            match create_output(
                &mut drm,
                &gbm,
                &render_formats,
                &plan,
                native_owner,
                native_edid,
            ) {
                Ok(output) => {
                    used.push(crtc);
                    outputs.push(output);
                }
                Err(error) => eprintln!("roost-compositor: drm: skip connector: {error}"),
            }
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
            let (width, height) = out.logical_size();
            let right = out
                .loc
                .0
                .checked_add(width)
                .ok_or_else(|| DrmInitError("monitor right edge overflow".into()))?;
            out.loc
                .1
                .checked_add(height)
                .ok_or_else(|| DrmInitError("monitor bottom edge overflow".into()))?;
            next_x = next_x.max(right);
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
                monitor_refresh: crate::monitor_refresh::RefreshState::new(drm.device_id()),
                monitor_event_fault: Default::default(),
                monitor_worker: None,
                monitor_scanout: None,
                discovery_plans: None,
                drm,
                renderer,
                gbm,
                render_formats,
                outputs,
                libinput,
                active: true,
                sleep_reset_pending: false,
                wake_scanout_blocked: true,
                monitor_resources_known: true,
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
                udev,
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

    fn invalidate_monitor_metadata(&mut self) {
        if let Some(pending) = self.monitor_scanout.take() {
            let valid = self.original_monitor_fd_valid();
            self.monitor_refresh
                .completed(pending.ticket, std::time::Instant::now(), valid, false);
        }
        for output in &mut self.outputs {
            output.native_edid = None;
        }
    }

    /// Events are matched against the original selected dev_t, never a name.
    /// Acquisition/reconciliation is a separate pending source-preparation step.
    pub fn on_udev_event(&mut self, event: UdevEvent) {
        use crate::monitor_refresh::DeviceEvent;
        let (device, event) = match event {
            UdevEvent::Changed { device_id } => (device_id, DeviceEvent::Changed),
            UdevEvent::Removed { device_id } => (device_id, DeviceEvent::Removed),
            UdevEvent::Added { device_id, .. } => (device_id, DeviceEvent::Added),
        };
        if self.monitor_refresh.device_event(device, event) {
            self.invalidate_monitor_metadata();
            if matches!(event, DeviceEvent::Removed) {
                self.monitor_resources_known = false;
            }
            self.wake_scanout_blocked = true;
        }
    }

    /// Session pause/activate from libseat.
    pub fn on_session_event(&mut self, event: SessionEvent) {
        self.corner_pressure.reset();
        if self
            .monitor_refresh
            .set_active(matches!(event, SessionEvent::ActivateSession))
        {
            self.invalidate_monitor_metadata();
        }
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
                    out.pending_surface_ids.clear();
                    out.pending_origin = None;
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
        self.monitor_refresh.request_refresh();
        self.invalidate_monitor_metadata();
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
            out.pending_surface_ids.clear();
            out.pending_origin = None;
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
        match drain_reset_events(|| receive_cookie_events(&self.drm)) {
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
        // Actual hotplug reconciliation must accept the fresh epoch before
        // scanout resumes, even when the independent wake reset succeeded.
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
        layout_generation: u64,
    ) -> Option<PageFlip> {
        match event {
            DrmEvent::VBlank(crtc) => {
                let admitted = self.original_monitor_fd_valid()
                    && self.monitor_scanout.as_ref().is_none_or(|pending| {
                        self.monitor_refresh.accepts(
                            pending.ticket,
                            std::time::Instant::now(),
                            true,
                        )
                    });
                if self.wake_scanout_blocked || !admitted {
                    if let Some(out) = self.outputs.iter_mut().find(|out| out.crtc == crtc) {
                        // Smithay owns at most pending_fb and queued_fb here;
                        // discarding those two never submits queued work. Calling
                        // frame_submitted would submit queued_fb as a side effect.
                        for mut feedback in out.surface.discard_pending_frames() {
                            feedback.discarded();
                        }
                        out.pending = false;
                        out.pending_surface_ids.clear();
                        out.pending_origin = None;
                        out.last_frame = None;
                        out.damage_tracker = None;
                    }
                    return None;
                }
                if self.wake_event_traces != 0 {
                    self.wake_event_traces -= 1;
                    eprintln!(
                        "roost-compositor: drm: wake pageflip crtc={crtc:?} metadata={metadata:?}",
                    );
                }
                let index = self.outputs.iter().position(|o| o.crtc == crtc)?;
                let Some(actual_cookie) = metadata
                    .as_ref()
                    .and_then(|meta| std::num::NonZeroU64::new(meta.user_data))
                else {
                    self.outputs[index].rejected_kernel_cookie_count = self.outputs[index]
                        .rejected_kernel_cookie_count
                        .saturating_add(1);
                    return None;
                };
                let output = &self.outputs[index];
                let original_fd = output
                    .surface
                    .surface()
                    .device_fd()
                    .downgrade()
                    .eq(self.drm.device_fd());
                let event = crate::monitor_scanout::KernelCommit {
                    user_data: actual_cookie.get(),
                    crtc: u32::from(crtc),
                    device: self.drm.device_id(),
                    commit_epoch: self.monitor_commit_epoch(),
                    layout_generation,
                    pending_cookie: output.surface.pending_commit_cookie(),
                    original_fd,
                };
                if !output.pending
                    || !output
                        .pending_origin
                        .is_some_and(|origin| origin.matches_pending_commit(&event))
                {
                    // Late/foreign/unsubmitted cookies must not consume a current
                    // slot, grant authority or implicitly submit a successor.
                    self.outputs[index].rejected_kernel_cookie_count = self.outputs[index]
                        .rejected_kernel_cookie_count
                        .saturating_add(1);
                    return None;
                }
                if !output
                    .pending_origin
                    .is_some_and(|origin| origin.matches_commit(&event))
                {
                    // This is the rightful old-layout commit, not a foreign
                    // callback. Retire ONLY its original buffer; never submit a
                    // successor or qualify current layout/native/perf feedback.
                    let out = &mut self.outputs[index];
                    if let Some(mut feedback) =
                        out.surface.retire_pending_with_cookie(actual_cookie)
                    {
                        feedback.discarded();
                        out.pending = false;
                        out.pending_surface_ids.clear();
                        out.pending_origin = None;
                        out.last_frame = None;
                        out.damage_tracker = None;
                    }
                    return None;
                }
                // Acquisition-only KMS ioctls, never ordinary per-frame reads.
                // An actual pending framebuffer and no queued successor are
                // required before frame_submitted can safely move the slot.
                let expected = if self
                    .monitor_scanout
                    .as_ref()
                    .is_some_and(|pending| pending.requires(u32::from(crtc)))
                {
                    let output = &self.outputs[index];
                    let framebuffer = output.surface.pending_scanout_framebuffer();
                    let plane = self
                        .drm
                        .is_atomic()
                        .then(|| u32::from(output.surface.surface().plane()));
                    let agreed = framebuffer.is_some_and(|framebuffer| {
                        kernel_scanout_readback(&self.drm, output).is_ok_and(|readback| {
                            readback.agrees(u32::from(crtc), plane, framebuffer.into())
                        })
                    });
                    let current = self.original_monitor_fd_valid()
                        && self.monitor_scanout.as_ref().is_some_and(|pending| {
                            self.monitor_refresh.accepts(
                                pending.ticket,
                                std::time::Instant::now(),
                                true,
                            )
                        });
                    if !agreed || !current {
                        if let Some(pending) = self.monitor_scanout.as_mut() {
                            pending.fail();
                        }
                        discard_pending_output(&mut self.outputs[index]);
                        return None;
                    }
                    framebuffer.map(|framebuffer| (u32::from(framebuffer), plane))
                } else {
                    None
                };
                let out = &mut self.outputs[index];
                let was_pending = out.pending;
                let mut completed = false;
                let feedback = match out.surface.frame_submitted_with_cookie(actual_cookie) {
                    Ok(feedback) => {
                        completed = was_pending && feedback.is_some();
                        feedback
                    }
                    Err(e) => {
                        eprintln!("roost-compositor: drm: frame_submitted: {e}");
                        None
                    }
                };
                out.pending = false;
                let queued_origin = out.pending_origin.take();
                // frame_submitted itself may perform kernel work. Admission
                // observed before it cannot authorize an expired final receipt.
                use std::os::fd::AsFd;
                let agreed_after = expected.is_none_or(|(framebuffer, plane)| {
                    kernel_scanout_readback(&self.drm, out)
                        .is_ok_and(|readback| readback.agrees(u32::from(crtc), plane, framebuffer))
                });
                let live = self.active
                    && self.drm.is_active()
                    && crate::native_output::device(self.drm.device_fd().as_fd()).ok()
                        == Some(self.drm.device_id());
                let admitted_final = agreed_after
                    && queued_origin.is_some_and(|origin| {
                        origin.cookie == actual_cookie
                            && origin.commit_epoch
                                == self.monitor_refresh.commit_epoch().unwrap_or(0)
                            && origin.layout_generation == layout_generation
                            && origin.crtc == u32::from(crtc)
                            && origin.device == self.drm.device_id()
                            && out
                                .surface
                                .surface()
                                .device_fd()
                                .downgrade()
                                .eq(self.drm.device_fd())
                    })
                    && live
                    && self.monitor_scanout.as_ref().is_none_or(|pending| {
                        self.monitor_refresh.accepts(
                            pending.ticket,
                            std::time::Instant::now(),
                            live,
                        )
                    });
                if !admitted_final {
                    if let Some(mut feedback) = feedback {
                        feedback.discarded();
                    }
                    out.pending_surface_ids.clear();
                    out.pending_origin = None;
                    if let Some(pending) = self.monitor_scanout.as_mut() {
                        pending.fail();
                    }
                    return None;
                }
                if completed {
                    self.monitor_event_fault.matched_completion();
                    if let (Some(origin), Some(meta)) = (queued_origin, metadata.as_ref()) {
                        let (clock, stamp) = match meta.timing.time {
                            DrmEventTime::Monotonic(time) => ("monotonic", time),
                            DrmEventTime::Realtime(time) => (
                                "realtime",
                                time.duration_since(std::time::SystemTime::UNIX_EPOCH)
                                    .unwrap_or_default(),
                            ),
                        };
                        out.last_kernel_completion =
                            Some(crate::monitor_scanout::KernelCompletion {
                                cookie: actual_cookie.get(),
                                crtc: u32::from(crtc),
                                device: origin.device,
                                commit_epoch: origin.commit_epoch,
                                layout_generation: origin.layout_generation,
                                sequence: meta.timing.sequence,
                                timestamp_clock: clock,
                                timestamp_secs: stamp.as_secs(),
                                timestamp_subsec_ns: stamp.subsec_nanos(),
                            });
                        out.kernel_completion_count = out.kernel_completion_count.saturating_add(1);
                    }
                }
                if let Some(pending) = self.monitor_scanout.as_mut() {
                    if completed {
                        pending.presented(
                            u32::from(crtc),
                            queued_origin.and_then(|origin| origin.monitor_epoch),
                            admitted_final,
                        );
                    } else if was_pending && pending.requires(u32::from(crtc)) {
                        pending.fail();
                    }
                }
                let (time, sequence) = match metadata.as_ref() {
                    Some(meta) => (
                        match meta.timing.time {
                            DrmEventTime::Monotonic(time) => Some(time),
                            DrmEventTime::Realtime(_) => None,
                        },
                        u64::from(meta.timing.sequence),
                    ),
                    None => (None, 0),
                };
                Some(PageFlip {
                    completed,
                    queued_origin,
                    output: out.output.clone(),
                    surface_ids: completed.then(|| std::mem::take(&mut out.pending_surface_ids)),
                    feedback,
                    time,
                    sequence,
                    refresh: crate::frame_timing::refresh_of(&out.output),
                })
            }
            DrmEvent::Error(e) => {
                if self.monitor_event_fault.revoke_once() {
                    eprintln!("roost-compositor: drm: device event error: {e}");
                    // Preserve original FD ownership; never reopen by dev_t.
                    // One bounded refresh episode, not a retry on every error.
                    self.monitor_refresh.request_refresh();
                    self.invalidate_monitor_metadata();
                    self.monitor_resources_known = false;
                    self.wake_scanout_blocked = true;
                    for output in &mut self.outputs {
                        discard_pending_output(output);
                    }
                }
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

// Use the bounded parser during recovery too. A malformed record is a failed
// drain, never silently counted as a retired kernel completion.
fn receive_cookie_events(device: &DrmDevice) -> std::io::Result<usize> {
    device
        .receive_events_with_user_data()?
        .try_fold(0usize, |count, event| event.map(|_| count + 1))
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

#[cfg(test)]
mod monitor_plan_geometry_tests {
    #[test]
    fn bounded_actual_geometry_never_wraps_wl_output_millimeters() {
        use super::plan_geometry_valid as valid;
        assert!(valid("eDP-1", (1920, 1080), (0, 0)));
        assert!(valid(
            "DP-1",
            (65535, 65535),
            (i32::MAX as u32, i32::MAX as u32)
        ));
        assert!(!valid("DP-1", (0, 1080), (0, 0)));
        assert!(!valid("DP-1", (1920, 0), (0, 0)));
        assert!(!valid("DP-1", (1920, 1080), (i32::MAX as u32 + 1, 0)));
        assert!(!valid("DP-1", (1920, 1080), (0, u32::MAX)));
        assert!(!valid("../card1", (1920, 1080), (0, 0)));
        assert!(!valid(&"x".repeat(129), (1920, 1080), (0, 0)));
    }
}
