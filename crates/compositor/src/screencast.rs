//! PipeWire screen-cast streams (#61), adapted from niri's
//! `src/screencasting/pw_utils.rs` (GPL-3.0-or-later, like Roost).
//!
//! Monitor and window streams in BGRx over shared memory (PipeWire
//! allocates and maps the memfd buffers), full frames at most every
//! [`FRAME_INTERVAL`]. dmabuf zero-copy, damage tracking and cursor
//! metadata, which niri also does, come later.

use std::cell::RefCell;
use std::os::fd::{AsFd, BorrowedFd};
use std::rc::Rc;
use std::time::{Duration, Instant};

use pipewire::context::ContextRc;
use pipewire::core::CoreRc;
use pipewire::loop_::Timeout;
use pipewire::main_loop::MainLoopRc;
use pipewire::properties::PropertiesBox;
use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pipewire::spa::param::format_utils::parse_format;
use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
use pipewire::spa::param::ParamType;
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{self, ChoiceValue, Pod, Property};
use pipewire::spa::sys::*;
use pipewire::spa::utils::{
    Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Rectangle, SpaTypes,
};
use pipewire::stream::{StreamFlags, StreamListener, StreamRc, StreamState};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use zbus::object_server::SignalEmitter;

use crate::mutter::CastTarget;

/// Longest gap a client waits between frames, and the shortest we send.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const BYTES_PER_PIXEL: i32 = 4;

/// The PipeWire connection, its loop driven by the compositor's.
pub struct PipeWire {
    _context: ContextRc,
    core: CoreRc,
}

impl PipeWire {
    /// Connect to the session's PipeWire; `None` when it is not running.
    pub fn new<D: 'static>(event_loop: &LoopHandle<'static, D>) -> Option<Self> {
        pipewire::init();
        let main_loop = MainLoopRc::new(None).ok()?;
        let context = ContextRc::new(&main_loop, None).ok()?;
        let core = context.connect_rc(None).ok()?;
        struct LoopFd(MainLoopRc);
        impl AsFd for LoopFd {
            fn as_fd(&self) -> BorrowedFd<'_> {
                self.0.loop_().fd()
            }
        }
        event_loop
            .insert_source(
                Generic::new(LoopFd(main_loop), Interest::READ, Mode::Level),
                |_, wrapper, _| {
                    wrapper.0.loop_().iterate(Timeout::None);
                    Ok(PostAction::Continue)
                },
            )
            .ok()?;
        Some(Self {
            _context: context,
            core,
        })
    }

    /// Start a stream of `width` x `height` physical pixels for one
    /// session. The node id goes to the client once PipeWire assigns it.
    pub fn start_cast(
        &self,
        session_id: u64,
        target: CastTarget,
        width: i32,
        height: i32,
        signal: SignalEmitter<'static>,
    ) -> Option<Cast> {
        let session_signal = signal.clone();
        let stream =
            StreamRc::new(self.core.clone(), "roost-screen-cast", PropertiesBox::new()).ok()?;
        let inner = Rc::new(RefCell::new(Inner::default()));
        let listener = stream
            .add_local_listener_with_user_data(())
            .state_changed({
                let inner = inner.clone();
                move |stream, (), old, new| {
                    eprintln!("roost-compositor: screen cast {session_id}: {old:?} -> {new:?}");
                    let mut inner = inner.borrow_mut();
                    match new {
                        StreamState::Paused => {
                            if inner.node_id.is_none() {
                                let id = stream.node_id();
                                inner.node_id = Some(id);
                                crate::mutter::stream_added(&signal, id);
                            }
                            inner.streaming = false;
                        }
                        StreamState::Streaming => inner.streaming = true,
                        StreamState::Error(_) => {
                            inner.streaming = false;
                            inner.failed = true;
                        }
                        StreamState::Unconnected | StreamState::Connecting => {}
                    }
                }
            })
            .param_changed({
                let inner = inner.clone();
                move |stream, (), id, pod| {
                    if ParamType::from_raw(id) != ParamType::Format {
                        return;
                    }
                    let Some(pod) = pod else { return };
                    let Ok((media_type, media_subtype)) = parse_format(pod) else {
                        return;
                    };
                    if media_type != MediaType::Video || media_subtype != MediaSubtype::Raw {
                        return;
                    }
                    let mut format = VideoInfoRaw::new();
                    if format.parse(pod).is_err() {
                        return;
                    }
                    let (w, h) = (format.size().width as i32, format.size().height as i32);
                    let stride = w * BYTES_PER_PIXEL;
                    // Shared memory the size of one frame, plus a header.
                    let buffers = pod::object!(
                        SpaTypes::ObjectParamBuffers,
                        ParamType::Buffers,
                        Property::new(
                            SPA_PARAM_BUFFERS_buffers,
                            pod::Value::Choice(ChoiceValue::Int(Choice(
                                ChoiceFlags::empty(),
                                ChoiceEnum::Range {
                                    default: 4,
                                    min: 2,
                                    max: 8
                                }
                            ))),
                        ),
                        Property::new(SPA_PARAM_BUFFERS_blocks, pod::Value::Int(1)),
                        Property::new(SPA_PARAM_BUFFERS_size, pod::Value::Int(stride * h)),
                        Property::new(SPA_PARAM_BUFFERS_stride, pod::Value::Int(stride)),
                        Property::new(
                            SPA_PARAM_BUFFERS_dataType,
                            pod::Value::Choice(ChoiceValue::Int(Choice(
                                ChoiceFlags::empty(),
                                ChoiceEnum::Flags {
                                    default: 1 << SPA_DATA_MemFd,
                                    flags: vec![1 << SPA_DATA_MemFd],
                                },
                            ))),
                        ),
                    );
                    let header = pod::object!(
                        SpaTypes::ObjectParamMeta,
                        ParamType::Meta,
                        Property::new(
                            SPA_PARAM_META_type,
                            pod::Value::Id(pipewire::spa::utils::Id(SPA_META_Header))
                        ),
                        Property::new(
                            SPA_PARAM_META_size,
                            pod::Value::Int(std::mem::size_of::<spa_meta_header>() as i32)
                        ),
                    );
                    let (mut b1, mut b2) = (Vec::new(), Vec::new());
                    let mut params = [make_pod(&mut b1, buffers), make_pod(&mut b2, header)];
                    if stream.update_params(&mut params).is_ok() {
                        inner.borrow_mut().size = Some((w, h));
                    }
                }
            })
            .register()
            .ok()?;
        let mut buffer = Vec::new();
        let format = make_pod(&mut buffer, video_format(width as u32, height as u32));
        stream
            .connect(
                Direction::Output,
                None,
                StreamFlags::DRIVER | StreamFlags::MAP_BUFFERS,
                &mut [format],
            )
            .ok()?;
        Some(Cast {
            session_id,
            target,
            offered: (width, height),
            signal: session_signal,
            stream,
            _listener: listener,
            inner,
            last_frame: None,
            sequence: 0,
        })
    }
}

#[derive(Default)]
struct Inner {
    node_id: Option<u32>,
    streaming: bool,
    failed: bool,
    /// Negotiated frame size.
    size: Option<(i32, i32)>,
}

/// One running stream.
pub struct Cast {
    pub session_id: u64,
    /// Monitor or window this stream shows.
    pub target: CastTarget,
    /// Size last offered to the consumer (window casts follow resizes).
    offered: (i32, i32),
    /// The stream's D-Bus emitter, for ending the session.
    signal: SignalEmitter<'static>,
    stream: StreamRc,
    _listener: StreamListener<()>,
    inner: Rc<RefCell<Inner>>,
    last_frame: Option<Instant>,
    sequence: u64,
}

impl Cast {
    /// Whether a frame is wanted now: streaming, negotiated, and at
    /// least [`FRAME_INTERVAL`] since the last one.
    pub fn wants_frame(&self) -> Option<(i32, i32)> {
        let inner = self.inner.borrow();
        if !inner.streaming {
            return None;
        }
        if self
            .last_frame
            .is_some_and(|t| t.elapsed() < FRAME_INTERVAL)
        {
            return None;
        }
        inner.size
    }

    /// Offer the consumer a new frame size (a cast window resized), the
    /// way niri does: a fresh format; frames keep the old size until the
    /// consumer accepts and new buffers arrive.
    pub fn resize(&mut self, width: i32, height: i32) {
        if self.offered == (width, height) || width <= 0 || height <= 0 {
            return;
        }
        let mut buffer = Vec::new();
        let format = make_pod(&mut buffer, video_format(width as u32, height as u32));
        if self.stream.update_params(&mut [format]).is_ok() {
            self.offered = (width, height);
        }
    }

    /// Tell the consumer its session ended (the cast window closed).
    pub fn close(&self) {
        crate::mutter::session_closed(&self.signal, self.session_id);
    }

    /// Whether PipeWire reported the stream broken.
    pub fn failed(&self) -> bool {
        self.inner.borrow().failed
    }

    /// Send one BGRx frame (`width * 4` bytes per row, top-down).
    pub fn send_frame(&mut self, width: i32, height: i32, bgrx: &[u8]) {
        self.last_frame = Some(Instant::now());
        let stride = (width * BYTES_PER_PIXEL) as usize;
        let size = stride * height as usize;
        if bgrx.len() < size {
            return;
        }
        unsafe {
            let buffer = self.stream.dequeue_raw_buffer();
            if buffer.is_null() {
                return;
            }
            let spa_buffer = (*buffer).buffer;
            let data = &mut *(*spa_buffer).datas;
            if !data.data.is_null() && data.maxsize as usize >= size {
                std::ptr::copy_nonoverlapping(bgrx.as_ptr(), data.data as *mut u8, size);
                let chunk = &mut *data.chunk;
                chunk.offset = 0;
                chunk.size = size as u32;
                chunk.stride = stride as i32;
                chunk.flags = SPA_CHUNK_FLAG_NONE as i32;
            } else {
                (*data.chunk).size = 0;
                (*data.chunk).flags = SPA_CHUNK_FLAG_CORRUPTED as i32;
            }
            self.sequence = self.sequence.wrapping_add(1);
            let header = spa_buffer_find_meta_data(
                spa_buffer,
                SPA_META_Header,
                std::mem::size_of::<spa_meta_header>(),
            ) as *mut spa_meta_header;
            if !header.is_null() {
                (*header).flags = 0;
                (*header).seq = self.sequence;
                (*header).pts = -1;
            }
            pipewire::sys::pw_stream_queue_buffer(self.stream.as_raw_ptr(), buffer);
        }
    }
}

/// The `(x, y, width, height)` part of a 4-byte-per-pixel frame of
/// `size`, rows top-down; `None` when the area leaves the frame.
pub fn crop(
    pixels: &[u8],
    (frame_w, frame_h): (i32, i32),
    (x, y, w, h): (i32, i32, i32, i32),
) -> Option<Vec<u8>> {
    if x < 0 || y < 0 || w <= 0 || h <= 0 || x + w > frame_w || y + h > frame_h {
        return None;
    }
    let bpp = BYTES_PER_PIXEL as usize;
    let stride = frame_w as usize * bpp;
    if pixels.len() < stride * frame_h as usize {
        return None;
    }
    let row = w as usize * bpp;
    let mut out = Vec::with_capacity(row * h as usize);
    for line in y as usize..(y + h) as usize {
        let start = line * stride + x as usize * bpp;
        out.extend_from_slice(&pixels[start..start + row]);
    }
    Some(out)
}

fn video_format(width: u32, height: u32) -> pod::Object {
    pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(FormatProperties::VideoFormat, Id, VideoFormat::BGRx),
        pod::property!(
            FormatProperties::VideoSize,
            Rectangle,
            Rectangle { width, height }
        ),
        pod::property!(
            FormatProperties::VideoFramerate,
            Fraction,
            Fraction { num: 0, denom: 1 }
        ),
        pod::property!(
            FormatProperties::VideoMaxFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 30, denom: 1 },
            Fraction { num: 1, denom: 1 },
            Fraction { num: 30, denom: 1 }
        ),
    )
}

fn make_pod(buffer: &mut Vec<u8>, object: pod::Object) -> &Pod {
    PodSerializer::serialize(
        std::io::Cursor::new(&mut *buffer),
        &pod::Value::Object(object),
    )
    .expect("pod serializes");
    Pod::from_bytes(buffer).expect("pod parses")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn areas_crop_out_of_frames() {
        // A 3x2 frame whose pixels count up.
        let frame: Vec<u8> = (0u8..6).flat_map(|p| [p, p, p, 255]).collect();
        let cropped = crop(&frame, (3, 2), (1, 0, 2, 2)).unwrap();
        let firsts: Vec<u8> = cropped.chunks(4).map(|p| p[0]).collect();
        assert_eq!(firsts, [1, 2, 4, 5]);
        assert!(
            crop(&frame, (3, 2), (2, 0, 2, 1)).is_none(),
            "past the edge"
        );
        assert!(crop(&frame, (3, 2), (0, 0, 0, 1)).is_none(), "empty");
    }
}
