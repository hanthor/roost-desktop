use std::os::unix::io::AsFd;
use std::sync::Arc;

use drm::control::{connector, crtc, plane, Mode};
use drm::{Device, DriverCapability};
use indexmap::IndexSet;

use crate::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use crate::backend::allocator::format::get_opaque;
use crate::backend::allocator::gbm::{GbmBuffer, GbmConvertError};
use crate::backend::allocator::{Allocator, Format, Fourcc, Modifier, Slot, Swapchain};
use crate::backend::drm::error::AccessError;
use crate::backend::drm::gbm::{framebuffer_from_bo, GbmFramebuffer};
use crate::backend::drm::{plane_has_property, DrmError, DrmSurface};
use crate::backend::renderer::sync::SyncPoint;
use crate::backend::SwapBuffersError;
use crate::utils::{DevPath, Physical, Rectangle, Transform};

use tracing::{debug, error, info_span, instrument, trace, warn};

use super::{PlaneConfig, PlaneDamageClips, PlaneState, VrrSupport};

#[derive(Debug)]
struct QueuedFb<U> {
    slot: Slot<GbmBuffer>,
    sync: Option<SyncPoint>,
    damage: Option<Vec<Rectangle<i32, Physical>>>,
    user_data: U,
    cookie: Option<std::num::NonZeroU64>,
}

/// Actual submission acceptance, independent of software queued buffers.
#[derive(Debug, Default)]
struct CommitCookies {
    accepted: Option<std::num::NonZeroU64>,
}
impl CommitCookies {
    fn submitted(&mut self, cookie: Option<std::num::NonZeroU64>, succeeded: bool) {
        self.accepted = if succeeded { cookie } else { None };
    }
    fn matches(&self, cookie: std::num::NonZeroU64) -> bool {
        self.accepted == Some(cookie)
    }
    fn revoke(&mut self) {
        self.accepted = None;
    }
    fn retire(&mut self, cookie: std::num::NonZeroU64) -> bool {
        if !self.matches(cookie) {
            return false;
        }
        self.revoke();
        true
    }
}

/// Simplified abstraction of a swapchain for gbm-buffers displayed on a [`DrmSurface`].
#[derive(Debug)]
pub struct GbmBufferedSurface<A: Allocator<Buffer = GbmBuffer> + 'static, U> {
    current_fb: Slot<GbmBuffer>,
    pending_fb: Option<(Slot<GbmBuffer>, U)>,
    commit_cookies: CommitCookies,
    queued_fb: Option<QueuedFb<U>>,
    next_fb: Option<Slot<GbmBuffer>>,
    swapchain: Swapchain<A>,
    drm: Arc<DrmSurface>,
    is_opaque: bool,
    supports_fencing: bool,
    span: tracing::Span,
}

impl<A, U> GbmBufferedSurface<A, U>
where
    A: Allocator<Buffer = GbmBuffer>,
    A::Error: std::error::Error + Send + Sync,
{
    /// Create a new `GbmBufferedSurface` from a given compatible combination
    /// of a surface, an allocator and renderer formats.
    ///
    /// The provided color_formats are tested in order until a working configuration is found.
    ///
    /// To successfully call this function, you need to have a renderer,
    /// which can render into a Dmabuf, and a gbm allocator that can produce
    /// buffers of a supported format for rendering.
    pub fn new(
        drm: DrmSurface,
        mut allocator: A,
        color_formats: &[Fourcc],
        renderer_formats: impl IntoIterator<Item = Format>,
    ) -> Result<GbmBufferedSurface<A, U>, Error<A::Error>> {
        let span = info_span!(parent: drm.span(), "drm_gbm");
        let _guard = span.enter();

        let mut error = None;
        let drm = Arc::new(drm);
        let renderer_formats = renderer_formats.into_iter().collect::<Vec<_>>();

        for format in color_formats {
            debug!("Testing color format: {}", format);
            match Self::new_internal(drm.clone(), allocator, renderer_formats.clone(), *format) {
                Ok((current_fb, swapchain, is_opaque)) => {
                    drop(_guard);
                    let supports_fencing = !drm.is_legacy()
                        && drm
                            .get_driver_capability(DriverCapability::SyncObj)
                            .map(|val| val != 0)
                            .map_err(|err| {
                                Error::DrmError(DrmError::Access(AccessError {
                                    errmsg: "Failed to query driver capability",
                                    dev: drm.dev_path(),
                                    source: err,
                                }))
                            })?
                        && plane_has_property(&*drm, drm.plane(), "IN_FENCE_FD")?;

                    return Ok(GbmBufferedSurface {
                        current_fb,
                        pending_fb: None,
                        commit_cookies: CommitCookies::default(),
                        queued_fb: None,
                        next_fb: None,
                        swapchain,
                        drm,
                        is_opaque,
                        supports_fencing,
                        span,
                    });
                }
                Err((alloc, err)) => {
                    warn!("Preferred format {} not available: {:?}", format, err);
                    allocator = alloc;
                    error = Some(err);
                }
            }
        }
        Err(error.unwrap())
    }

    #[allow(clippy::type_complexity)]
    fn new_internal(
        drm: Arc<DrmSurface>,
        allocator: A,
        mut renderer_formats: Vec<Format>,
        code: Fourcc,
    ) -> Result<(Slot<GbmBuffer>, Swapchain<A>, bool), (A, Error<A::Error>)> {
        // select a format
        let mut plane_formats = drm.plane_info().formats.iter().copied().collect::<IndexSet<_>>();
        let opaque_code = get_opaque(code).unwrap_or(code);
        if !plane_formats
            .iter()
            .any(|fmt| fmt.code == code || fmt.code == opaque_code)
        {
            return Err((allocator, Error::NoSupportedPlaneFormat));
        }
        plane_formats.retain(|fmt| fmt.code == code || fmt.code == opaque_code);
        renderer_formats.retain(|fmt| fmt.code == code);

        let plane_modifiers = plane_formats
            .iter()
            .map(|fmt| fmt.modifier)
            .collect::<IndexSet<_>>();
        let renderer_modifiers = renderer_formats
            .iter()
            .map(|fmt| fmt.modifier)
            .collect::<IndexSet<_>>();

        trace!("Plane formats: {:?}", plane_formats);
        trace!("Renderer formats: {:?}", renderer_formats);
        debug!(
            "Remaining intersected modifiers: {:?}",
            plane_modifiers
                .intersection(&renderer_modifiers)
                .collect::<IndexSet<_>>()
        );

        if plane_formats.is_empty() {
            return Err((allocator, Error::NoSupportedPlaneFormat));
        } else if renderer_formats.is_empty() {
            return Err((allocator, Error::NoSupportedRendererFormat));
        }

        let formats = {
            // Special case: if a format supports explicit LINEAR (but no implicit Modifiers)
            // and the other doesn't support any modifier, force Implicit.
            // This should at least result in a working pipeline possibly with a linear buffer,
            // but we cannot be sure.
            if (plane_formats.len() == 1
                && plane_formats.iter().next().unwrap().modifier == Modifier::Invalid
                && renderer_formats.iter().all(|x| x.modifier != Modifier::Invalid)
                && renderer_formats.iter().any(|x| x.modifier == Modifier::Linear))
                || (renderer_formats.len() == 1
                    && renderer_formats.first().unwrap().modifier == Modifier::Invalid
                    && plane_formats.iter().all(|x| x.modifier != Modifier::Invalid)
                    && plane_formats.iter().any(|x| x.modifier == Modifier::Linear))
            {
                vec![Format {
                    code,
                    modifier: Modifier::Invalid,
                }]
            } else {
                plane_modifiers
                    .intersection(&renderer_modifiers)
                    .cloned()
                    .map(|modifier| Format { code, modifier })
                    .collect::<Vec<_>>()
            }
        };
        debug!("Testing Formats: {:?}", formats);

        let modifiers = formats.iter().map(|x| x.modifier).collect::<Vec<_>>();
        let mode = drm.pending_mode();

        let mut swapchain: Swapchain<A> = Swapchain::new(
            allocator,
            mode.size().0 as u32,
            mode.size().1 as u32,
            code,
            modifiers,
        );

        // Test format
        let buffer = match swapchain.acquire() {
            Ok(buffer) => buffer.unwrap(),
            Err(err) => return Err((swapchain.allocator, Error::GbmError(err))),
        };
        let format = Format {
            code,
            modifier: buffer.modifier(), // no guarantee
                                         // that this is stable across allocations, but
                                         // we want to print that here for debugging proposes.
                                         // It has no further use.
        };

        let use_opaque = !plane_formats.iter().any(|f| f.code == code);
        let fb = match framebuffer_from_bo(drm.device_fd(), &buffer, use_opaque) {
            Ok(fb) => fb,
            Err(err) => return Err((swapchain.allocator, Error::DrmError(err.into()))),
        };
        match buffer.export() {
            Ok(dmabuf) => dmabuf,
            Err(err) => return Err((swapchain.allocator, err.into())),
        };
        buffer.userdata().insert_if_missing(|| fb);

        let handle = buffer.userdata().get::<GbmFramebuffer>().unwrap();

        let plane_state = PlaneState {
            handle: drm.plane(),
            config: Some(PlaneConfig {
                src: Rectangle::from_size((mode.size().0 as i32, mode.size().1 as i32).into()).to_f64(),
                dst: Rectangle::from_size((mode.size().0 as i32, mode.size().1 as i32).into()),
                alpha: 1.0,
                transform: Transform::Normal,
                damage_clips: None,
                fb: *handle.as_ref(),
                fence: None,
            }),
        };

        match drm.test_state([plane_state], true) {
            Ok(_) => {
                debug!("Choosen format: {:?}", format);
                Ok((buffer, swapchain, use_opaque))
            }
            Err(err) => {
                warn!(
                    "Mode-setting failed with automatically selected buffer format {:?}: {}",
                    format, err
                );
                Err((swapchain.allocator, err.into()))
            }
        }
    }

    /// Retrieves the next buffer to be rendered into and it's age.
    ///
    /// *Note*: This function can be called multiple times and
    /// will return the same buffer until it is queued (see [`GbmBufferedSurface::queue_buffer`]).
    #[instrument(level = "trace", skip_all, parent = &self.span, err)]
    #[profiling::function]
    pub fn next_buffer(&mut self) -> Result<(Dmabuf, u8), Error<A::Error>> {
        if !self.drm.is_active() {
            return Err(Error::<A::Error>::DrmError(DrmError::DeviceInactive));
        }

        if self.next_fb.is_none() {
            let slot = self
                .swapchain
                .acquire()
                .map_err(Error::GbmError)?
                .ok_or(Error::NoFreeSlotsError)?;

            let maybe_buffer = slot.userdata().get::<GbmFramebuffer>();
            if maybe_buffer.is_none() {
                let fb = framebuffer_from_bo(self.drm.device_fd(), &slot, self.is_opaque)
                    .map_err(|err| Error::DrmError(err.into()))?;
                slot.userdata().insert_if_missing(|| fb);
            }

            self.next_fb = Some(slot);
        }

        let slot = self.next_fb.as_ref().unwrap();
        Ok((slot.export()?, slot.age()))
    }

    /// Queues the current buffer for rendering.
    ///
    /// Returns [`Error::NoBuffer`] in case [`GbmBufferedSurface::next_buffer`] has not been called
    /// prior to this function.
    ///
    /// *Note*: This function needs to be followed up with [`GbmBufferedSurface::frame_submitted`]
    /// when a vblank event is received, that denotes successful scanout of the buffer.
    /// Otherwise the underlying swapchain will eventually run out of buffers.
    ///
    /// `user_data` can be used to attach some data to a specific buffer and later retrieved with [`GbmBufferedSurface::frame_submitted`]
    #[profiling::function]
    pub fn queue_buffer(
        &mut self,
        sync: Option<SyncPoint>,
        damage: Option<Vec<Rectangle<i32, Physical>>>,
        user_data: U,
    ) -> Result<(), Error<A::Error>> {
        self.queue_buffer_with_data(sync, damage, user_data, None)
    }
    /// Queue a buffer with a nonzero actual kernel-commit completion cookie.
    /// A queued successor is not accepted until its actual ioctl succeeds.
    pub fn queue_buffer_with_cookie(
        &mut self,
        sync: Option<SyncPoint>,
        damage: Option<Vec<Rectangle<i32, Physical>>>,
        user_data: U,
        cookie: std::num::NonZeroU64,
    ) -> Result<(), Error<A::Error>> {
        self.queue_buffer_with_data(sync, damage, user_data, Some(cookie))
    }
    fn queue_buffer_with_data(
        &mut self,
        sync: Option<SyncPoint>,
        damage: Option<Vec<Rectangle<i32, Physical>>>,
        user_data: U,
        cookie: Option<std::num::NonZeroU64>,
    ) -> Result<(), Error<A::Error>> {
        if !self.drm.is_active() {
            return Err(Error::<A::Error>::DrmError(DrmError::DeviceInactive));
        }

        let next_fb = self.next_fb.take().ok_or(Error::<A::Error>::NoBuffer)?;

        self.swapchain.submitted(&next_fb);

        self.queued_fb = Some(QueuedFb {
            slot: next_fb,
            sync,
            damage,
            user_data,
            cookie,
        });
        if self.pending_fb.is_none() {
            self.submit()?;
        }
        Ok(())
    }

    /// Discard buffers abandoned by a device reset, without submitting any
    /// queued frame or claiming that a pending frame was displayed.
    ///
    /// Returns the pending and queued user data (at most two entries), so the
    /// caller can discard presentation feedback. The unqueued next buffer is
    /// released too. The current scanout buffer remains retained until KMS
    /// state is reset. Callers must reset the device/surface state and buffer
    /// pool before rendering again, and ignore stale pre-reset completions.
    pub fn discard_pending_frames(&mut self) -> Vec<U> {
        let mut discarded = Vec::with_capacity(2);
        self.next_fb = None;
        self.commit_cookies.revoke();
        if let Some((_slot, user_data)) = self.pending_fb.take() {
            discarded.push(user_data);
        }
        if let Some(queued) = self.queued_fb.take() {
            discarded.push(queued.user_data);
        }
        discarded
    }

    /// Actual pending GBM framebuffer, only when completing it cannot submit
    /// another queued buffer as a side effect. This is buffer identity for
    /// independent KMS-state readback, not kernel event-userdata provenance.
    pub fn pending_scanout_framebuffer(&self) -> Option<drm::control::framebuffer::Handle> {
        if self.queued_fb.is_some() {
            return None;
        }
        let (slot, _) = self.pending_fb.as_ref()?;
        slot.userdata()
            .get::<GbmFramebuffer>()
            .map(|framebuffer| *framebuffer.as_ref())
    }

    /// Marks the current frame as submitted.
    ///
    /// *Note*: Needs to be called, after the vblank event of the matching [`DrmDevice`](super::super::DrmDevice)
    /// was received after calling [`GbmBufferedSurface::queue_buffer`] on this surface.
    /// Otherwise the underlying swapchain will run out of buffers eventually.
    ///
    /// Returns the user data that was stored with [`GbmBufferedSurface::queue_buffer`] if a buffer was pending, otherwise
    /// `None` is returned.
    #[profiling::function]
    pub fn frame_submitted(&mut self) -> Result<Option<U>, Error<A::Error>> {
        self.commit_cookies.revoke();
        if let Some((mut pending, user_data)) = self.pending_fb.take() {
            std::mem::swap(&mut pending, &mut self.current_fb);
            if self.queued_fb.is_some() {
                self.submit()?;
            }
            Ok(Some(user_data))
        } else {
            Ok(None)
        }
    }

    /// Cookie from an actual successful event-generating kernel commit only.
    pub fn pending_commit_cookie(&self) -> Option<std::num::NonZeroU64> {
        self.pending_fb.as_ref()?;
        self.commit_cookies.accepted
    }
    /// Complete only the exact pending kernel commit; mismatch never consumes
    /// a buffer or submits any queued successor.
    pub fn frame_submitted_with_cookie(
        &mut self,
        cookie: std::num::NonZeroU64,
    ) -> Result<Option<U>, Error<A::Error>> {
        if self.pending_fb.is_none() || !self.commit_cookies.matches(cookie) {
            return Ok(None);
        }
        self.frame_submitted()
    }

    /// Retire only the exact completed pending buffer without submitting any
    /// queued successor. Returns its original data for explicit discard when
    /// the caller's current layout no longer qualifies presentation feedback.
    /// The now-scanned-out buffer remains retained as current until replaced.
    pub fn retire_pending_with_cookie(&mut self, cookie: std::num::NonZeroU64) -> Option<U> {
        if self.pending_fb.is_none() || !self.commit_cookies.retire(cookie) {
            return None;
        }
        let (mut pending, data) = self.pending_fb.take().unwrap();
        std::mem::swap(&mut pending, &mut self.current_fb);
        Some(data)
    }

    #[profiling::function]
    fn submit(&mut self) -> Result<(), Error<A::Error>> {
        // yes it does not look like it, but both of these lines should be safe in all cases.
        let QueuedFb {
            slot,
            sync,
            damage,
            user_data,
            cookie,
        } = self.queued_fb.take().unwrap();
        let handle = slot.userdata().get::<GbmFramebuffer>().unwrap();
        let mode = self.drm.pending_mode();
        let src = Rectangle::from_size((mode.size().0 as i32, mode.size().1 as i32).into()).to_f64();
        let dst = Rectangle::from_size((mode.size().0 as i32, mode.size().1 as i32).into());

        let damage_clips = damage.and_then(|damage| {
            PlaneDamageClips::from_damage(self.drm.device_fd(), src, dst, damage)
                .ok()
                .flatten()
        });

        // Try to extract a native fence out of the supplied sync point if any
        // If the sync point has no native fence or the surface does not support
        // fencing force a wait
        let fence = if let Some(sync) = sync {
            if !self.supports_fencing {
                // we probably don't want to fail to submit in this case, lets hope on implicit sync
                let _ = sync.wait();
                None
            } else {
                let fence = sync.export();

                if fence.is_none() {
                    let _ = sync.wait();
                }

                fence
            }
        } else {
            None
        };

        let plane_state = PlaneState {
            handle: self.plane(),
            config: Some(PlaneConfig {
                src,
                dst,
                transform: Transform::Normal,
                alpha: 1.0,
                damage_clips: damage_clips.as_ref().map(|d| d.blob()),
                fb: *handle.as_ref(),
                fence: fence.as_ref().map(|fence| fence.as_fd()),
            }),
        };

        let flip = match (self.drm.commit_pending(), cookie) {
            (true, Some(cookie)) => self.drm.commit_with_cookie([plane_state], cookie),
            (false, Some(cookie)) => self.drm.page_flip_with_cookie([plane_state], cookie),
            (true, None) => self.drm.commit([plane_state], true),
            (false, None) => self.drm.page_flip([plane_state], true),
        };
        self.commit_cookies.submitted(cookie, flip.is_ok());
        if flip.is_ok() {
            self.pending_fb = Some((slot, user_data));
        }
        flip.map_err(Error::DrmError)
    }

    /// Reset the underlying buffers
    pub fn reset_buffers(&mut self) {
        self.commit_cookies.revoke();
        self.swapchain.reset_buffers()
    }

    /// Reset the age for all buffers.
    ///
    /// This can be used to efficiently clear the damage history without having to
    /// modify the damage for each surface.
    pub fn reset_buffer_ages(&mut self) {
        self.swapchain.reset_buffer_ages();
    }

    /// Returns the underlying [`crtc`](drm::control::crtc) of this surface
    pub fn crtc(&self) -> crtc::Handle {
        self.drm.crtc()
    }

    /// Returns the underlying [`plane`](drm::control::plane) of this surface
    pub fn plane(&self) -> plane::Handle {
        self.drm.plane()
    }

    /// Currently used [`connector`](drm::control::connector)s of this `Surface`
    pub fn current_connectors(&self) -> impl IntoIterator<Item = connector::Handle> {
        self.drm.current_connectors()
    }

    /// Returns the pending [`connector`](drm::control::connector)s
    /// used for the next frame queued via [`queue_buffer`](GbmBufferedSurface::queue_buffer).
    pub fn pending_connectors(&self) -> impl IntoIterator<Item = connector::Handle> {
        self.drm.pending_connectors()
    }

    /// Tries to add a new [`connector`](drm::control::connector)
    /// to be used after the next commit.
    ///
    /// **Warning**: You need to make sure, that the connector is not used with another surface
    /// or was properly removed via `remove_connector` + `commit` before adding it to another surface.
    /// Behavior if failing to do so is undefined, but might result in rendering errors or the connector
    /// getting removed from the other surface without updating it's internal state.
    ///
    /// Fails if the `connector` is not compatible with the underlying [`crtc`](drm::control::crtc)
    /// (e.g. no suitable [`encoder`](drm::control::encoder) may be found)
    /// or is not compatible with the currently pending
    /// [`Mode`](drm::control::Mode).
    pub fn add_connector(&self, connector: connector::Handle) -> Result<(), Error<A::Error>> {
        self.drm.add_connector(connector).map_err(Error::DrmError)
    }

    /// Tries to mark a [`connector`](drm::control::connector)
    /// for removal on the next commit.    
    pub fn remove_connector(&self, connector: connector::Handle) -> Result<(), Error<A::Error>> {
        self.drm.remove_connector(connector).map_err(Error::DrmError)
    }

    /// Tries to replace the current connector set with the newly provided one on the next commit.
    ///
    /// Fails if one new `connector` is not compatible with the underlying [`crtc`](drm::control::crtc)
    /// (e.g. no suitable [`encoder`](drm::control::encoder) may be found)
    /// or is not compatible with the currently pending
    /// [`Mode`](drm::control::Mode).    
    pub fn set_connectors(&self, connectors: &[connector::Handle]) -> Result<(), Error<A::Error>> {
        self.drm.set_connectors(connectors).map_err(Error::DrmError)
    }

    /// Returns the currently active [`Mode`](drm::control::Mode)
    /// of the underlying [`crtc`](drm::control::crtc)    
    pub fn current_mode(&self) -> Mode {
        self.drm.current_mode()
    }

    /// Returns the currently pending [`Mode`](drm::control::Mode)
    /// to be used after the next commit.    
    pub fn pending_mode(&self) -> Mode {
        self.drm.pending_mode()
    }

    /// Tries to set a new [`Mode`](drm::control::Mode)
    /// to be used after the next commit.
    ///
    /// Fails if the mode is not compatible with the underlying
    /// [`crtc`](drm::control::crtc) or any of the
    /// pending [`connector`](drm::control::connector)s.
    pub fn use_mode(&mut self, mode: Mode) -> Result<(), Error<A::Error>> {
        self.drm.use_mode(mode).map_err(Error::DrmError)?;
        let (w, h) = mode.size();
        self.swapchain.resize(w as _, h as _);
        Ok(())
    }

    /// Returns if Variable Refresh Rate is advertised as supported by the given connector.
    ///
    /// See [`DrmSurface::vrr_supported`] for more details.
    pub fn vrr_supported(&self, conn: connector::Handle) -> Result<VrrSupport, Error<A::Error>> {
        self.drm.vrr_supported(conn).map_err(Error::DrmError)
    }

    /// Returns whether the next frame state would set Variable Refresh Rate as enabled.
    ///
    /// See [`DrmSurface::vrr_enabled`] for more details.
    pub fn vrr_enabled(&self) -> bool {
        self.drm.vrr_enabled()
    }

    /// Tries to set Variable Refresh Rate (VRR) for the next frame.
    //
    /// Doing so might cause the next frame to trigger a modeset.
    /// Check [`GbmBufferedSurface::vrr_supported`], which indicates if VRR can be
    /// used without a modeset on the attached connectors./
    pub fn use_vrr(&self, vrr: bool) -> Result<(), Error<A::Error>> {
        self.drm.use_vrr(vrr).map_err(Error::DrmError)
    }

    /// Returns a reference to the underlying drm surface
    pub fn surface(&self) -> &DrmSurface {
        &self.drm
    }

    /// Get the format of the underlying swapchain
    pub fn format(&self) -> Fourcc {
        self.swapchain.format()
    }
}

/// Errors thrown by a [`GbmBufferedSurface`]
#[derive(Debug, thiserror::Error)]
pub enum Error<E: std::error::Error + Send + Sync + 'static> {
    /// No supported pixel format for the given plane could be determined
    #[error("No supported plane buffer format found")]
    NoSupportedPlaneFormat,
    /// No supported pixel format for the given renderer could be determined
    #[error("No supported renderer buffer format found")]
    NoSupportedRendererFormat,
    /// The supported pixel formats of the renderer and plane are incompatible
    #[error("Supported plane and renderer buffer formats are incompatible")]
    FormatsNotCompatible,
    /// The swapchain is exhausted, you need to call `frame_submitted`
    #[error("Failed to allocate a new buffer")]
    NoFreeSlotsError,
    /// Failed to renderer using the given renderer
    #[error("Failed to render test frame")]
    InitialRenderingError,
    /// Error accessing the drm device
    #[error("The underlying drm surface encounted an error: {0}")]
    DrmError(#[from] DrmError),
    /// Error importing the rendered buffer to libgbm for scan-out
    #[error("The underlying gbm device encounted an error: {0}")]
    GbmError(#[source] E),
    /// Error exporting as Dmabuf
    #[error("The allocated buffer could not be exported as a dmabuf: {0}")]
    AsDmabufError(#[from] GbmConvertError),
    /// No buffer to queue
    #[error("No buffer has been acquired to get queued")]
    NoBuffer,
}

impl<E: std::error::Error + Send + Sync + 'static> From<Error<E>> for SwapBuffersError {
    #[inline]
    fn from(err: Error<E>) -> SwapBuffersError {
        match err {
            x @ Error::NoSupportedPlaneFormat
            | x @ Error::NoSupportedRendererFormat
            | x @ Error::FormatsNotCompatible
            | x @ Error::InitialRenderingError => SwapBuffersError::ContextLost(Box::new(x)),
            x @ Error::NoFreeSlotsError | x @ Error::NoBuffer => {
                SwapBuffersError::TemporaryFailure(Box::new(x))
            }
            Error::DrmError(err) => err.into(),
            Error::GbmError(err) => SwapBuffersError::ContextLost(Box::new(err)),
            Error::AsDmabufError(err) => SwapBuffersError::ContextLost(Box::new(err)),
        }
    }
}

#[cfg(test)]
mod roost_commit_cookie_tests {
    use super::CommitCookies;
    use std::num::NonZeroU64;
    fn cookie(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).unwrap()
    }
    #[test]
    fn queued_or_failed_commit_is_not_accepted_and_mismatch_preserves_current() {
        let mut state = CommitCookies::default();
        let original = cookie(0x1_0000_0001);
        let successor = cookie(0x2_0000_0001);
        assert!(!state.matches(original)); // queued-only has no submitted receipt
        state.submitted(Some(original), false);
        assert!(!state.matches(original));
        state.submitted(Some(original), true);
        assert!(state.matches(original));
        assert!(!state.matches(successor));
        assert!(state.matches(original)); // mismatch cannot consume current
        state.revoke(); // exact completion/discard/reset
        assert!(!state.matches(original));
        state.submitted(Some(successor), true);
        assert!(!state.matches(original));
        assert!(state.matches(successor));
        state.revoke();
        assert!(!state.matches(successor));
        state.submitted(None, true); // existing non-cookie APIs retain behavior
        assert!(!state.matches(original));
    }
}

#[cfg(test)]
mod roost_layout_cookie_tests {
    use super::CommitCookies;
    #[test]
    fn rightful_old_layout_retirement_allows_new_cookie_and_late_old_is_inert() {
        let old = std::num::NonZeroU64::new(90).unwrap();
        let new = std::num::NonZeroU64::new(91).unwrap();
        let mut state = CommitCookies::default();
        state.submitted(Some(old), true);
        assert!(!state.retire(new));
        assert_eq!(state.accepted, Some(old));
        assert!(state.retire(old));
        assert_eq!(state.accepted, None);
        state.submitted(Some(new), true);
        assert!(!state.retire(old));
        assert_eq!(state.accepted, Some(new));
        assert!(state.retire(new));
        assert_eq!(state.accepted, None);
    }
}
