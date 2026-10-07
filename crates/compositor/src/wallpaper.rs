//! Image background behind the solid clear (settings-compat M4).
//!
//! The shell publishes the Settings wallpaper URI out-of-band: it
//! writes the URI to [`wallpaper_drop_path`] in the runtime dir, and
//! the compositor re-reads that file on every frame. An optional second
//! line carries GNOME's `primary-color` (`#rrggbb`), the colour shown
//! when there is no picture or it cannot be read, as in GNOME. No
//! wallpaper state crosses the control protocol by design. Any
//! failure — missing file, unparsable URI, undecodable image,
//! oversized output — degrades to the solid clear, never an error.

use crate::slideshow::{Sample, Timeline, MAX_XML_BYTES};
use roost_shell_control::background::{BackgroundMetadata, PictureSettings};
use roost_wallpaper::background::Geometry;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::{Logical, Physical, Point, Rectangle, Size, Transform};

/// Drop-file name in the runtime dir carrying the wallpaper URI.
pub const WALLPAPER_FILE: &str = "roost-wallpaper";
/// Largest output area (in pixels) a wallpaper is scaled to; bigger
/// outputs keep the solid clear rather than burning memory.
pub const MAX_WALLPAPER_AREA: u64 = 7680 * 4320;

/// Runtime-dir drop path for the wallpaper URI, beside the control
/// socket and the Wayland socket.
pub fn wallpaper_drop_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(WALLPAPER_FILE)
}

/// Parse a `file://` wallpaper URI into a filesystem path. Anything
/// else (http, resource, empty) is not a file the compositor can
/// open and reads as absent.
pub fn wallpaper_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.trim().strip_prefix("file://")?;
    let encoded = if rest.starts_with('/') {
        rest
    } else {
        let slash = rest.find('/')?;
        if !rest[..slash].eq_ignore_ascii_case("localhost") {
            return None;
        }
        &rest[slash..]
    };
    // URI delimiters are distinct from escaped filename bytes. Decode once:
    // %2520 names a literal "%20", not a space. Native filenames need not be
    // UTF-8, so preserve their bytes rather than using a lossy display string.
    let encoded = encoded.split(['?', '#']).next()?.as_bytes();
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        let value = if encoded[index] == b'%' {
            let digit = |byte: u8| (byte as char).to_digit(16).map(|d| d as u8);
            let high = digit(*encoded.get(index + 1)?)?;
            let low = digit(*encoded.get(index + 2)?)?;
            index += 3;
            let value = high * 16 + low;
            // GIO's local file URI conversion rejects escaped separators.
            if value == b'/' {
                return None;
            }
            value
        } else {
            let value = encoded[index];
            index += 1;
            value
        };
        if value == 0 {
            return None;
        }
        bytes.push(value);
    }
    let path = PathBuf::from(OsString::from_vec(bytes));
    path.is_absolute().then_some(path)
}

/// Cached wallpaper image, re-scaled to the output that showed it.
#[derive(Debug)]
struct Loaded {
    paint: Option<serde_json::Value>,
    epoch: Option<String>,
    /// URI text that produced this image (or failed to).
    uri: String,
    /// Output size the pixels match (`None` when the URI failed).
    size: Option<Size<i32, Logical>>,
    /// ARGB8888 pixels at `size`, ready to upload.
    pixels: Option<Vec<u8>>,
    /// The uploadable buffer, kept so its texture is uploaded once, not
    /// every frame.
    buffer: Option<MemoryRenderBuffer>,
}

/// One rendered overview card (GNOME's workspace background), cached by
/// what it shows and its size.
struct CardCache {
    key: (String, Size<i32, Logical>, i32, i32, Option<[u8; 3]>),
    buffer: MemoryRenderBuffer,
    margin: i32,
}

/// Most overview cards cached at once (active and neighbor sizes).
const MAX_CARDS: usize = 4;

/// Session wallpaper: polls the drop file, decodes on change. One
/// cache slot per output size (multi-monitor): each output's
/// cover-crop is decoded once and reused while its size and the URI
/// hold. Slots are area-capped individually and count-capped
/// together, so extra outputs cannot grow memory without bound.
#[derive(Debug, Default)]
pub struct Wallpaper {
    loaded: Vec<Loaded>,
    /// Journey-only observation of the last attempted refresh, not a paint claim.
    refresh_stage: Option<&'static str>,
    identities: Vec<IdentitySnapshot>,
    identity_pending: Vec<(
        String,
        std::sync::mpsc::Receiver<std::sync::Arc<SourceObservation>>,
    )>,
    animation_samples: Vec<AnimationSample>,
    paint_state: Option<serde_json::Value>,
    image_cache: std::sync::Arc<BackgroundCaches>,
    /// Decodes running on worker threads, by URI and output size: a
    /// 4K JPEG XL takes long enough to stall frames, so the clear
    /// colour shows until the picture is ready (GNOME loads
    /// backgrounds asynchronously too).
    pending: Vec<Pending>,
    /// `primary-color` from the drop file's second line.
    color: Option<[f32; 3]>,
    /// The accent color line, when published.
    accent: Option<[f32; 3]>,
    /// Overview cards.
    cards: Vec<CardCache>,
    /// The lock screen's blurred, dimmed copy, by URI and output size.
    locked: Vec<(String, Size<i32, Logical>, MemoryRenderBuffer)>,
}

impl std::fmt::Debug for CardCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CardCache").field("key", &self.key).finish()
    }
}

/// Parse `#rrggbb` (GNOME's `primary-color` format) into 0..1 floats.
pub fn parse_color(text: &str) -> Option<[f32; 3]> {
    let hex = text.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([
        f32::from(channel(0)?) / 255.0,
        f32::from(channel(2)?) / 255.0,
        f32::from(channel(4)?) / 255.0,
    ])
}

#[derive(Debug)]
struct Pending {
    paint: Option<serde_json::Value>,
    epoch: Option<String>,
    uri: String,
    size: Size<i32, Logical>,
    result: std::sync::mpsc::Receiver<Option<Vec<u8>>>,
}

/// Most cached sizes at once; each slot holds at most
/// `MAX_WALLPAPER_AREA` pixels, so the cache stays bounded while
/// covering the common multi-output case.
const MAX_WALLPAPER_SLOTS: usize = 4;

#[derive(Debug)]
struct IdentitySnapshot {
    uri: String,
    value: std::sync::Arc<SourceObservation>,
    observed: std::time::Instant,
}

type ImageCache = std::sync::Mutex<Vec<(FileIdentity, std::sync::Arc<image::RgbaImage>)>>;

type PlacedKey = (
    FileIdentity,
    roost_shell_control::background::Placement,
    Geometry,
);
type PlacementCache = std::sync::Mutex<
    Vec<(
        PlacedKey,
        std::sync::Arc<roost_wallpaper::background::PlacedImage>,
    )>,
>;
#[derive(Debug, Default)]
struct BackgroundCaches {
    images: ImageCache,
    placed: PlacementCache,
}

#[derive(Debug, Clone, Default)]
struct SourceObservation {
    generation: String,
    identity: Option<FileIdentity>,
    xml: bool,
    timeline: Option<Timeline>,
    references: Vec<(PathBuf, Option<FileIdentity>)>,
}
#[derive(Debug)]
struct AnimationSample {
    key: String,
    sampled_wall: f64,
    sampled_mono: std::time::Instant,
    sample: Sample,
}
type AnimationPaint = (Sample, Vec<(PathBuf, Option<FileIdentity>)>);

#[derive(Debug, Clone)]
struct Paint {
    epoch: Option<String>,
    uri: String,
    identity: Option<FileIdentity>,
    animation: Option<AnimationPaint>,
}

impl Wallpaper {
    /// Empty wallpaper (solid clear until a drop file appears).
    pub fn new() -> Self {
        Self::default()
    }

    /// Bounded in-memory observations only. Never inspect an arbitrary source
    /// path from the frame/snapshot thread and never equate decoded pixels
    /// with a successfully painted desktop frame.
    pub fn diagnostics(&self) -> serde_json::Value {
        fn text(value: &str) -> serde_json::Value {
            let prefix: String = value.chars().take(1024).collect();
            serde_json::json!({"truncated": prefix.len() < value.len(), "bytes": value.len(), "prefix": prefix})
        }
        serde_json::json!({
            "last_refresh_stage": self.refresh_stage,
            "list_limit": MAX_WALLPAPER_SLOTS,
            "identity_count": self.identities.len(),
            "identities": self.identities.iter().take(MAX_WALLPAPER_SLOTS).map(|item| serde_json::json!({
                "uri": text(&item.uri), "identity": text(&format!("{:?}", item.value.identity)),
                "age_ms": item.observed.elapsed().as_millis(),
            })).collect::<Vec<_>>(),
            "identity_pending_count": self.identity_pending.len(),
            "identity_pending": self.identity_pending.iter().take(MAX_WALLPAPER_SLOTS).map(|item| text(&item.0)).collect::<Vec<_>>(),
            "decode_pending_count": self.pending.len(),
            "decode_pending": self.pending.iter().take(MAX_WALLPAPER_SLOTS).map(|item| serde_json::json!({
                "request_key": text(&item.uri), "size": [item.size.w, item.size.h],
            })).collect::<Vec<_>>(),
            "decode_result_count": self.loaded.len(),
            "decode_results": self.loaded.iter().take(MAX_WALLPAPER_SLOTS).map(|item| serde_json::json!({
                "request_key": text(&item.uri), "size": item.size.map(|s| [s.w, s.h]),
                "pixel_bytes": item.pixels.as_ref().map(Vec::len),
                "buffer_available": item.buffer.is_some(),
            })).collect::<Vec<_>>(),
        })
    }

    /// Arbitrary configured paths can reside on slow network/FUSE mounts.
    /// Observe them only on bounded workers; the frame path uses snapshots.
    #[cfg(test)]
    fn poll_identity(&mut self, uri: &str) -> Option<Option<FileIdentity>> {
        self.poll_source(uri).map(|source| source.identity.clone())
    }

    fn poll_source(&mut self, uri: &str) -> Option<std::sync::Arc<SourceObservation>> {
        let mut index = 0;
        while index < self.identity_pending.len() {
            let result = match self.identity_pending[index].1.try_recv() {
                Ok(value) => Some(value),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(std::sync::Arc::new(SourceObservation::default()))
                }
            };
            if let Some(value) = result {
                let (completed, _) = self.identity_pending.remove(index);
                self.identities.retain(|snapshot| snapshot.uri != completed);
                if self.identities.len() >= MAX_WALLPAPER_SLOTS {
                    self.identities.remove(0);
                }
                self.identities.push(IdentitySnapshot {
                    uri: completed,
                    value,
                    observed: std::time::Instant::now(),
                });
            } else {
                index += 1;
            }
        }
        let snapshot = self.identities.iter().find(|snapshot| snapshot.uri == uri);
        let value = snapshot.map(|snapshot| snapshot.value.clone());
        let due = snapshot.is_none_or(|snapshot| {
            snapshot.observed.elapsed() >= std::time::Duration::from_secs(1)
        });
        if due
            && self.identity_pending.len() < MAX_WALLPAPER_SLOTS
            && !self
                .identity_pending
                .iter()
                .any(|(pending, _)| pending == uri)
        {
            let (send, result) = std::sync::mpsc::channel();
            let path = uri.to_owned();
            if std::thread::Builder::new()
                .name("roost-wallpaper-identity".into())
                .spawn(move || {
                    let _ = send.send(std::sync::Arc::new(SourceObservation::observe(&path)));
                })
                .is_ok()
            {
                self.identity_pending.push((uri.to_owned(), result));
            }
        }
        value
    }

    /// Start (or collect) the background decode of `uri` at `output`.
    fn poll_decode(
        &mut self,
        key: &str,
        paint: Paint,
        output: Size<i32, Logical>,
        settings: PictureSettings,
        geometry: Geometry,
    ) {
        let index = self
            .pending
            .iter()
            .position(|p| p.uri == key && p.size == output);
        let Some(index) = index else {
            // Keep running jobs counted until they finish; dropping old receivers
            // would allow rapid settings changes to spawn unbounded workers.
            self.pending.retain(|p| {
                !matches!(
                    p.result.try_recv(),
                    Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected)
                )
            });
            if self.pending.len() >= MAX_WALLPAPER_SLOTS {
                return;
            }
            let (send, result) = std::sync::mpsc::channel();
            let epoch = paint.epoch.clone();
            let paint_record = paint.animation.as_ref().map(|(sample,references)| serde_json::json!({
                "sample_wall": sample.wall, "progress": sample.progress, "interval": sample.interval,
                "from": sample.from, "to": sample.to, "slide": sample.slide,
                "generation": epoch, "source_uri": paint.uri, "root_identity": format!("{:?}",paint.identity),
                "source_identities": references.iter().map(|(path,id)| serde_json::json!({"path":path.to_string_lossy(),"identity":format!("{id:?}")})).collect::<Vec<_>>(), "geometry": {"physical":geometry.physical,
                    "origin":geometry.origin,"desktop":geometry.desktop,"scale":geometry.scale()},
            }));
            let job = paint;
            let image_cache = self.image_cache.clone();
            let spawned = std::thread::Builder::new()
                .name("roost-wallpaper".into())
                .spawn(move || {
                    let started = std::time::Instant::now();
                    let pixels = if let Some((sample, references)) = job.animation {
                        load_animation_wallpaper(
                            &job.uri,
                            job.identity,
                            sample,
                            references,
                            settings,
                            geometry,
                            &image_cache,
                        )
                    } else {
                        load_static_wallpaper(&job.uri, settings, geometry, job.identity)
                    };
                    eprintln!(
                        "roost-compositor: wallpaper {} for {}x{} in {} ms",
                        if pixels.is_some() {
                            "decoded"
                        } else {
                            "unreadable"
                        },
                        output.w,
                        output.h,
                        started.elapsed().as_millis()
                    );
                    let _ = send.send(pixels);
                });
            if spawned.is_ok() {
                self.pending.push(Pending {
                    paint: paint_record,
                    epoch,
                    uri: key.to_owned(),
                    size: output,
                    result,
                });
            }
            return;
        };
        let pixels = match self.pending[index].result.try_recv() {
            Ok(pixels) => pixels,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        let pending = self.pending.remove(index);
        if self.loaded.len() >= MAX_WALLPAPER_SLOTS {
            self.loaded.remove(0);
        }
        let buffer = pixels.as_ref().map(|pixels| {
            MemoryRenderBuffer::from_slice(
                pixels,
                Fourcc::Argb8888,
                (output.w, output.h),
                1,
                Transform::Normal,
                None,
            )
        });
        self.loaded.push(Loaded {
            paint: pending.paint,
            epoch: pending.epoch,
            uri: key.to_owned(),
            size: Some(output),
            pixels,
            buffer,
        });
    }

    /// GNOME's accent color as last published (the shell resolves
    /// `accent-color` to RGB), else GNOME's default blue.
    pub fn accent(&self) -> [f32; 3] {
        self.accent
            .unwrap_or([53.0 / 255.0, 132.0 / 255.0, 228.0 / 255.0])
    }

    /// GNOME's `primary-color` as last published, for the clear under
    /// the picture (and instead of it when there is none).
    pub fn paint_state(&self) -> Option<&serde_json::Value> {
        self.paint_state.as_ref()
    }

    pub fn color(&self) -> Option<[f32; 3]> {
        self.color
    }

    /// Render element stretching the current image over a `w` x `h`
    /// output, or `None` when no usable image is loaded. The caller
    /// still clears first: the image covers the output exactly, but
    /// the clear stays the honest fallback underneath. Call once per
    /// output with that output's size; crops are cached per size.
    pub fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        w: i32,
        h: i32,
        geometry: Geometry,
    ) -> Option<MemoryRenderBufferRenderElement<GlesRenderer>> {
        let output = Size::<i32, Logical>::from((w, h));
        self.paint_state = None;
        let uri = self.refresh(output, geometry)?;
        let loaded = self
            .loaded
            .iter()
            .find(|loaded| loaded.uri == uri && loaded.size == Some(output))?;
        let buffer = loaded.buffer.as_ref()?;
        let size = loaded.size?;
        let element = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            Point::<f64, Physical>::from((0.0, 0.0)),
            buffer,
            None,
            None,
            Some(size),
            Kind::Unspecified,
        )
        .ok()?;
        self.paint_state = loaded.paint.clone();
        Some(element)
    }

    /// GNOME's lock-screen background over a `w` x `h` output: the
    /// wallpaper blurred and dimmed (`roost_wallpaper::LOCK_*`), cached
    /// per URI and size. `None` while the picture is still decoding or
    /// when there is none (the caller clears to the dimmed
    /// primary-color).
    pub fn lock_element(
        &mut self,
        renderer: &mut GlesRenderer,
        w: i32,
        h: i32,
        geometry: Geometry,
    ) -> Option<MemoryRenderBufferRenderElement<GlesRenderer>> {
        let output = Size::<i32, Logical>::from((w, h));
        let uri = self.refresh_picture(output, true, geometry)?;
        if !self
            .locked
            .iter()
            .any(|(u, s, _)| *u == uri && *s == output)
        {
            let full = self
                .loaded
                .iter()
                .find(|l| l.uri == uri && l.size == Some(output))
                .and_then(|l| l.pixels.as_deref())?;
            let pixels = roost_wallpaper::blur_dim_argb(
                full,
                w as u32,
                h as u32,
                roost_wallpaper::LOCK_BLUR_SIGMA,
                roost_wallpaper::LOCK_BRIGHTNESS,
            )?;
            if self.locked.len() >= MAX_CARDS {
                self.locked.remove(0);
            }
            self.locked.push((
                uri.clone(),
                output,
                MemoryRenderBuffer::from_slice(
                    &pixels,
                    Fourcc::Argb8888,
                    (w, h),
                    1,
                    Transform::Normal,
                    None,
                ),
            ));
        }
        let (_, size, buffer) = self
            .locked
            .iter()
            .find(|(u, s, _)| *u == uri && *s == output)?;
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            Point::<f64, Physical>::from((0.0, 0.0)),
            buffer,
            None,
            None,
            Some(*size),
            Kind::Unspecified,
        )
        .ok()
    }

    /// Re-read the drop file and start or collect the decode for
    /// `output`; the current URI when there is a picture to show.
    fn refresh(&mut self, output: Size<i32, Logical>, geometry: Geometry) -> Option<String> {
        self.refresh_picture(output, false, geometry)
    }

    fn refresh_picture(
        &mut self,
        output: Size<i32, Logical>,
        lock: bool,
        geometry: Geometry,
    ) -> Option<String> {
        self.refresh_stage = Some("output-area");
        let area = output.w.max(0) as u64 * output.h.max(0) as u64;
        if area == 0 || area > MAX_WALLPAPER_AREA {
            return None;
        }
        self.refresh_stage = Some("drop-open");
        let mut drop_file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(wallpaper_drop_path())
            .ok()?;
        self.refresh_stage = Some("drop-metadata");
        let meta = drop_file.metadata().ok()?;
        if !meta.is_file() || meta.len() > 16 * 1024 {
            return None;
        }
        self.refresh_stage = Some("drop-read-bounded-utf8");
        let mut text = String::new();
        drop_file
            .by_ref()
            .take(16 * 1024 + 1)
            .read_to_string(&mut text)
            .ok()?;
        if text.len() > 16 * 1024 {
            return None;
        }
        self.refresh_stage = Some("drop-parse-metadata");
        let mut lines = text.lines();
        let mut uri = lines.next().unwrap_or_default().trim().to_owned();
        self.color = lines.next().and_then(parse_color);
        self.accent = lines.next().and_then(parse_color);
        let lock_uri = lines.next().unwrap_or_default().trim();
        let settings = if let Some(line) = lines.next() {
            // Present but malformed/version-mismatched metadata is an error,
            // never an apparent successful legacy zoom rendering.
            let metadata: BackgroundMetadata = serde_json::from_str(line).ok()?;
            if metadata.version != 1 || lines.next().is_some() {
                return None;
            }
            if lock {
                metadata.lock
            } else {
                metadata.desktop
            }
        } else {
            let primary = self
                .color
                .map(|rgb| rgb.map(|v| (v * 255.0).round() as u8))
                .unwrap_or([2, 60, 136]);
            PictureSettings {
                primary,
                ..PictureSettings::default()
            }
        };
        if lock && !lock_uri.is_empty() {
            uri = lock_uri.to_owned();
        }
        self.refresh_stage = Some("source-identity-pending");
        let observation = self.poll_source(&uri)?;
        let identity = observation.identity.clone();
        let epoch = observation.timeline.as_ref().map(|_| {
            format!(
                "{uri}\n{settings:?}\n{geometry:?}\n{}",
                observation.generation
            )
        });
        let animation = if observation.xml
            && settings.placement != roost_shell_control::background::Placement::None
        {
            let timeline = observation.timeline.as_ref()?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs_f64();
            let sample_key = format!(
                "{uri}\n{identity:?}\n{geometry:?}\n{}",
                observation.generation
            );
            let index = self
                .animation_samples
                .iter()
                .position(|s| s.key == sample_key);
            let previous = index.map(|i| &self.animation_samples[i]);
            let due = previous.is_none_or(|old| {
                old.sampled_mono.elapsed().as_secs_f64() >= old.sample.interval
                    || ((now - old.sampled_wall) - old.sampled_mono.elapsed().as_secs_f64()).abs()
                        > 1.0
            });
            if due {
                let logical = [
                    (f64::from(geometry.physical[0]) / geometry.scale()).round() as i32,
                    (f64::from(geometry.physical[1]) / geometry.scale()).round() as i32,
                ];
                let mut sample = timeline.sample(now, logical)?;
                if sample.to.is_none() {
                    sample.progress = 0.0;
                }
                if let Some(index) = index {
                    self.animation_samples.remove(index);
                }
                if self.animation_samples.len() >= MAX_WALLPAPER_SLOTS {
                    self.animation_samples.remove(0);
                }
                self.animation_samples.push(AnimationSample {
                    key: sample_key.clone(),
                    sampled_wall: now,
                    sampled_mono: std::time::Instant::now(),
                    sample,
                });
            }
            let sample = self
                .animation_samples
                .iter()
                .find(|s| s.key == sample_key)?
                .sample
                .clone();
            let references = observation
                .references
                .iter()
                .filter(|&(path, _)| *path == sample.from || sample.to.as_ref() == Some(path))
                .cloned()
                .collect::<Vec<_>>();
            Some((sample, references))
        } else {
            None
        };
        // XML epoch, selected keyframe identities and actual sampled progress
        // participate in the cache key. Old results cannot satisfy a new key.
        let key =
            format!("{uri}\n{settings:?}\n{geometry:?}\n{identity:?}\n{epoch:?}\n{animation:?}");

        if !self
            .loaded
            .iter()
            .any(|loaded| loaded.uri == key && loaded.size == Some(output))
        {
            self.poll_decode(
                &key,
                Paint {
                    epoch: epoch.clone(),
                    uri,
                    identity,
                    animation,
                },
                output,
                settings,
                geometry,
            );
        }
        if epoch.is_some()
            && !self
                .loaded
                .iter()
                .any(|loaded| loaded.uri == key && loaded.buffer.is_some())
        {
            // Retain a completed frame only within the exact same XML/settings/
            // geometry/source epoch while its next transition step is pending.
            // Source or calendar changes never reuse obsolete epoch pixels.
            if let Some(previous) = self.loaded.iter().rev().find(|loaded| {
                loaded.epoch == epoch
                    && loaded.buffer.is_some()
                    && loaded.paint.as_ref().is_some_and(|record| {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|t| t.as_secs_f64())
                            .unwrap_or(f64::INFINITY);
                        let age = now - record["sample_wall"].as_f64().unwrap_or(f64::NEG_INFINITY);
                        age >= 0.0 && age <= record["interval"].as_f64().unwrap_or(0.0) + 0.5
                    })
            }) {
                return Some(previous.uri.clone());
            }
        }
        self.refresh_stage = Some("request-accepted");
        Some(key)
    }

    /// GNOME's overview workspace card (`.workspace-background`): the
    /// desktop below the top bar (`work_top` pixels down an `output`-sized
    /// screen) scaled into `card`, corners rounded to 30px and a
    /// `0 4px 16px 4px` shadow at 20%, both scaled with the card as GNOME
    /// scales them. Without a picture it shows `primary-color`.
    pub fn card_element(
        &mut self,
        renderer: &mut GlesRenderer,
        output: Size<i32, Logical>,
        work_top: i32,
        card: Rectangle<i32, Physical>,
        alpha: f32,
        geometry: Geometry,
    ) -> Option<MemoryRenderBufferRenderElement<GlesRenderer>> {
        if card.size.w <= 0 || card.size.h <= 0 {
            return None;
        }
        let uri = self.refresh(output, geometry).unwrap_or_default();
        let color = self.color.map(|[r, g, b]| {
            [
                (r * 255.0).round() as u8,
                (g * 255.0).round() as u8,
                (b * 255.0).round() as u8,
            ]
        });
        let key = (uri.clone(), output, card.size.w, card.size.h, color);
        if !self.cards.iter().any(|c| c.key == key) {
            let full = self
                .loaded
                .iter()
                .find(|l| l.uri == uri && l.size == Some(output))
                .and_then(|l| l.pixels.as_deref());
            // Still decoding: draw nothing new yet rather than cache a
            // colour card the picture will replace.
            if !uri.is_empty() && full.is_none() {
                return None;
            }
            let scale = f64::from(card.size.w) / f64::from(output.w.max(1));
            let s = scale as f32;
            let top = work_top.clamp(0, output.h - 1) as u32;
            let rendered = roost_wallpaper::card(
                full.map(|p| (p, output.w as u32, output.h as u32)),
                (0, top, output.w as u32, output.h as u32 - top),
                color.unwrap_or([0x14, 0x17, 0x1c]),
                card.size.w as u32,
                card.size.h as u32,
                30.0 * s,
                roost_wallpaper::Shadow {
                    dy: 4.0 * s,
                    blur: 16.0 * s,
                    spread: 4.0 * s,
                    alpha: 0.2,
                },
            )?;
            if self.cards.len() >= MAX_CARDS {
                self.cards.remove(0);
            }
            self.cards.push(CardCache {
                key: key.clone(),
                buffer: MemoryRenderBuffer::from_slice(
                    &rendered.pixels,
                    Fourcc::Argb8888,
                    (rendered.width as i32, rendered.height as i32),
                    1,
                    Transform::Normal,
                    None,
                ),
                margin: rendered.margin as i32,
            });
        }
        let cached = self.cards.iter().find(|c| c.key == key)?;
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            Point::<f64, Physical>::from((
                f64::from(card.loc.x - cached.margin),
                f64::from(card.loc.y - cached.margin),
            )),
            &cached.buffer,
            Some(alpha),
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FileIdentity {
    path: PathBuf,
    dev: u64,
    ino: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl FileIdentity {
    fn from_metadata(path: PathBuf, m: &std::fs::Metadata) -> Option<Self> {
        if !m.is_file() || m.len() > 64 * 1024 * 1024 {
            return None;
        }
        Some(Self {
            path,
            dev: m.dev(),
            ino: m.ino(),
            bytes: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        })
    }
    fn for_uri(uri: &str) -> Option<Self> {
        Self::for_path(&wallpaper_uri_to_path(uri)?)
    }
    fn for_path(path: &std::path::Path) -> Option<Self> {
        let path = path.canonicalize().ok()?;
        Self::from_metadata(path.clone(), &std::fs::metadata(path).ok()?)
    }
}
/// This entire observer executes on a bounded worker, including timezone
/// calendar conversion and every referenced image's arbitrary-path metadata.
impl SourceObservation {
    fn observe(uri: &str) -> Self {
        let identity = FileIdentity::for_uri(uri);
        let mut observed = Self {
            identity: identity.clone(),
            ..Self::default()
        };
        let Some(identity) = identity else {
            return observed;
        };
        // GNOME slideshow descriptors use XML. Sniff their bounded content too,
        // rather than treating an XML descriptor as an undecodable static image.
        let Ok(mut file) = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&identity.path)
        else {
            return observed;
        };
        let mut prefix = [0; 256];
        let count = file.read(&mut prefix).unwrap_or_default();
        observed.xml = identity.path.extension().is_some_and(|e| e == "xml");
        let markup = prefix[..count]
            .iter()
            .copied()
            .find(|v| !v.is_ascii_whitespace())
            == Some(b'<');
        if !observed.xml && !markup {
            return observed;
        }
        let Some(bytes) = read_identity(&identity, MAX_XML_BYTES as u64) else {
            return observed;
        };
        // SVG is an image XML vocabulary, not a GNOME slideshow. A successful
        // <background> parser is required before a non-.xml file becomes one.
        let timeline = Timeline::parse(&bytes);
        if timeline.is_some() {
            observed.xml = true;
        }
        let Some(timeline) = timeline else {
            return observed;
        };
        for path in timeline
            .slides
            .iter()
            .flat_map(|s| s.from.iter().chain(s.to.iter()))
            .map(|v| &v.path)
        {
            if !observed.references.iter().any(|(p, _)| p == path) {
                if observed.references.len() >= 128 {
                    return observed;
                }
                observed
                    .references
                    .push((path.clone(), FileIdentity::for_path(path)));
            }
        }
        if FileIdentity::for_uri(uri) == observed.identity {
            observed.generation = format!(
                "{:?}\n{}\n{:?}",
                observed.identity,
                timeline.start.to_bits(),
                observed.references
            );
            observed.timeline = Some(timeline);
        }
        observed
    }
}
fn read_identity(expected: &FileIdentity, limit: u64) -> Option<Vec<u8>> {
    if expected.bytes > limit {
        return None;
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&expected.path)
        .ok()?;
    if FileIdentity::from_metadata(expected.path.clone(), &file.metadata().ok()?)
        != Some(expected.clone())
    {
        return None;
    }
    let mut bytes = Vec::new();
    file.by_ref().take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 == expected.bytes
        && FileIdentity::from_metadata(expected.path.clone(), &file.metadata().ok()?)
            == Some(expected.clone()))
    .then_some(bytes)
}
fn load_animation_wallpaper(
    uri: &str,
    identity: Option<FileIdentity>,
    sample: Sample,
    references: Vec<(PathBuf, Option<FileIdentity>)>,
    settings: PictureSettings,
    geometry: Geometry,
    caches: &BackgroundCaches,
) -> Option<Vec<u8>> {
    let unchanged = || {
        FileIdentity::for_uri(uri) == identity
            && references
                .iter()
                .all(|(path, id)| FileIdentity::for_path(path) == *id)
    };
    if !unchanged() {
        return None;
    }
    let decode = |path: &PathBuf| -> Option<std::sync::Arc<image::RgbaImage>> {
        let identity = references.iter().find(|(p, _)| p == path)?.1.as_ref()?;
        if let Some(found) = caches
            .images
            .lock()
            .ok()?
            .iter()
            .find(|(key, _)| key == identity)
            .map(|(_, image)| image.clone())
        {
            return Some(found);
        }
        let image = std::sync::Arc::new(
            roost_wallpaper::decode(&read_identity(identity, 64 * 1024 * 1024)?)?.to_rgba8(),
        );
        if FileIdentity::for_path(path) != Some(identity.clone()) {
            return None;
        }
        let mut slots = caches.images.lock().ok()?;
        let mut bytes = slots
            .iter()
            .map(|(_, image)| image.as_raw().len())
            .sum::<usize>();
        if image.as_raw().len() <= 64 * 1024 * 1024 {
            while !slots.is_empty()
                && (slots.len() >= MAX_WALLPAPER_SLOTS
                    || bytes + image.as_raw().len() > 64 * 1024 * 1024)
            {
                bytes -= slots.remove(0).1.as_raw().len();
            }
            slots.push((identity.clone(), image.clone()));
        }
        Some(image)
    };
    let from = decode(&sample.from);
    let to = sample.to.as_ref().and_then(decode);
    let progress = if sample.to.is_some() {
        sample.progress
    } else {
        0.0
    };
    let placed = |path: &PathBuf, image: Option<&image::RgbaImage>| {
        let image = image?;
        let identity = references.iter().find(|(p, _)| p == path)?.1.clone()?;
        let key = (identity, settings.placement, geometry);
        if let Some(found) = caches
            .placed
            .lock()
            .ok()?
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, image)| image.clone())
        {
            return Some(found);
        }
        let image = std::sync::Arc::new(roost_wallpaper::background::place(
            image,
            settings.placement,
            geometry,
        )?);
        let mut slots = caches.placed.lock().ok()?;
        let mut bytes = slots.iter().map(|(_, image)| image.bytes()).sum::<usize>();
        while !slots.is_empty()
            && (bytes + image.bytes() > 64 * 1024 * 1024 || slots.len() >= MAX_WALLPAPER_SLOTS)
        {
            bytes -= slots.remove(0).1.bytes();
        }
        slots.push((key, image.clone()));
        Some(image)
    };
    let placed_from = placed(&sample.from, from.as_deref());
    let placed_to = sample
        .to
        .as_ref()
        .and_then(|path| placed(path, to.as_deref()));
    let pixels =
        if (from.is_none() || placed_from.is_some()) && (to.is_none() || placed_to.is_some()) {
            roost_wallpaper::background::render_placed_blend(
                placed_from.as_deref(),
                placed_to.as_deref(),
                progress,
                settings,
                geometry,
            )
        } else {
            // An output larger than the placement-cache byte budget still renders
            // faithfully on its bounded worker, without keeping large float planes.
            roost_wallpaper::background::render_blend(
                from.as_deref(),
                to.as_deref(),
                progress,
                settings,
                geometry,
            )
        }?;
    unchanged().then_some(pixels)
}

fn load_static_wallpaper(
    uri: &str,
    settings: PictureSettings,
    geometry: Geometry,
    expected: Option<FileIdentity>,
) -> Option<Vec<u8>> {
    if !geometry.valid() {
        return None;
    }
    if FileIdentity::for_uri(uri) != expected {
        return None;
    }
    let original_identity = expected.clone();
    let cache = static_cache_path(uri, settings, geometry, expected.as_ref());
    let want = geometry.physical[0] as usize * geometry.physical[1] as usize * 4;
    if let Some(path) = cache.as_ref() {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            if file
                .metadata()
                .is_ok_and(|m| m.is_file() && m.len() == want as u64)
            {
                let mut bytes = Vec::new();
                if file
                    .by_ref()
                    .take(want as u64 + 1)
                    .read_to_end(&mut bytes)
                    .is_ok()
                    && bytes.len() == want
                    && FileIdentity::for_uri(uri) == original_identity
                {
                    return Some(bytes);
                }
            }
        }
    }
    let image = if settings.placement == roost_shell_control::background::Placement::None {
        None
    } else if let Some(expected) = expected {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&expected.path)
            .ok()?;
        if FileIdentity::from_metadata(expected.path.clone(), &file.metadata().ok()?)
            != Some(expected.clone())
        {
            return None;
        }
        let mut bytes = Vec::new();
        file.by_ref()
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 != expected.bytes
            || FileIdentity::from_metadata(expected.path.clone(), &file.metadata().ok()?)
                != Some(expected.clone())
            || FileIdentity::for_uri(uri) != Some(expected)
        {
            return None;
        }
        roost_wallpaper::decode(&bytes).map(|im| im.to_rgba8())
    } else {
        None
    };
    let pixels = roost_wallpaper::background::render(image.as_ref(), settings, geometry)?;
    if FileIdentity::for_uri(uri) != original_identity {
        return None;
    }
    if let Some(path) = cache {
        let tmp = path.with_extension("tmp");
        if path
            .parent()
            .is_some_and(|d| std::fs::create_dir_all(d).is_ok())
            && std::fs::write(&tmp, &pixels).is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    Some(pixels)
}

fn static_cache_path(
    uri: &str,
    settings: PictureSettings,
    geometry: Geometry,
    identity: Option<&FileIdentity>,
) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    if cfg!(test) {
        return None;
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    4u32.hash(&mut hash);
    uri.hash(&mut hash);
    settings.hash(&mut hash);
    geometry.hash(&mut hash);
    identity.hash(&mut hash);
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(
        base.join("roost/wallpaper")
            .join(format!("static-{:016x}.argb", hash.finish())),
    )
}

/// Decode `uri` and scale it to cover `output` exactly (center-crop),
/// returning ARGB8888 pixels. `None` on any failure: the caller
/// keeps the solid clear.
#[cfg(test)]
fn load_wallpaper(uri: &str, output: Size<i32, Logical>) -> Option<Vec<u8>> {
    let path = wallpaper_uri_to_path(uri)?;
    let bytes = std::fs::read(&path).ok()?;
    // Bound the decode: refuse absurd files before pixels exist.
    if bytes.len() > 64 * 1024 * 1024 || output.w <= 0 || output.h <= 0 {
        return None;
    }
    let (w, h) = (output.w as u32, output.h as u32);
    let cache = cache_path(&path, w, h);
    let want = w as usize * h as usize * 4;
    if let Some(cached) = cache.as_ref().and_then(|c| std::fs::read(c).ok()) {
        if cached.len() == want {
            return Some(cached);
        }
    }
    let pixels = roost_wallpaper::decode_cover_argb(&bytes, w, h)?;
    if let Some(cache) = cache {
        let tmp = cache.with_extension("tmp");
        if cache
            .parent()
            .is_some_and(|d| std::fs::create_dir_all(d).is_ok())
            && std::fs::write(&tmp, &pixels).is_ok()
        {
            let _ = std::fs::rename(&tmp, &cache);
        }
    }
    Some(pixels)
}

/// Where the scaled pixels of `path` at `w` x `h` are cached: GNOME's
/// 4K JPEG XL defaults take seconds to decode, so only the first
/// session after a wallpaper or output change pays for it. Keyed by
/// path, size, modification time and output size.
#[cfg(test)]
fn cache_path(path: &std::path::Path, w: u32, h: u32) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    if cfg!(test) {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Bumped whenever decoding changes what pixels come out (2: JPEG XL
    // converted to sRGB), so no session keeps showing stale ones.
    2u32.hash(&mut hasher);
    path.hash(&mut hasher);
    meta.len().hash(&mut hasher);
    meta.modified().ok()?.hash(&mut hasher);
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(
        base.join("roost")
            .join("wallpaper")
            .join(format!("{:016x}-{w}x{h}.argb", hasher.finish())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `XDG_RUNTIME_DIR` is process-global: every test that points it
    /// at a temp dir holds this lock and restores the prior value, so
    /// parallel tests never observe a borrowed runtime dir.
    static RUNTIME_DIR_LOCK: Mutex<()> = Mutex::new(());

    /// Point `XDG_RUNTIME_DIR` at `dir` while held; restores the prior
    /// value (or unsets) on drop.
    struct RuntimeDirGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl RuntimeDirGuard {
        fn point_at(dir: &std::path::Path) -> Self {
            let prior = std::env::var_os("XDG_RUNTIME_DIR");
            std::env::set_var("XDG_RUNTIME_DIR", dir);
            Self { prior }
        }
    }

    impl Drop for RuntimeDirGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    #[test]
    fn uri_to_path_accepts_absolute_file_uris() {
        assert_eq!(
            wallpaper_uri_to_path("file:///tmp/wall.png"),
            Some(PathBuf::from("/tmp/wall.png"))
        );
        // The drop file carries a trailing newline; URIs trim clean.
        assert_eq!(
            wallpaper_uri_to_path("  file:///tmp/wall.png\n"),
            Some(PathBuf::from("/tmp/wall.png"))
        );
    }

    #[test]
    fn file_uris_decode_filename_bytes_once() {
        assert_eq!(
            wallpaper_uri_to_path("file:///tmp/my%20caf%C3%A9%20%2520.png"),
            Some(PathBuf::from("/tmp/my café %20.png"))
        );
        assert_eq!(
            wallpaper_uri_to_path("file://localhost/tmp/my%23photo%3F.png"),
            Some(PathBuf::from("/tmp/my#photo?.png"))
        );
        assert_eq!(
            wallpaper_uri_to_path("file:///tmp/image.png?query#fragment"),
            Some(PathBuf::from("/tmp/image.png"))
        );
        assert_eq!(
            wallpaper_uri_to_path("file:///tmp/image%FF.png"),
            Some(PathBuf::from(OsString::from_vec(
                b"/tmp/image\xff.png".to_vec()
            )))
        );
    }

    #[test]
    fn file_uris_reject_invalid_escapes_and_foreign_authorities() {
        for uri in [
            "file:///tmp/invalid%.png",
            "file:///tmp/invalid%2.png",
            "file:///tmp/invalid%GG.png",
            "file:///tmp/null%00.png",
            "file:///tmp/escaped%2fseparator.png",
            "file://remote/tmp/image.png",
            "file://user@localhost/tmp/image.png",
        ] {
            assert_eq!(wallpaper_uri_to_path(uri), None, "{uri}");
        }
    }

    #[test]
    fn uri_to_path_rejects_non_file_uris() {
        for uri in [
            "",
            "   ",
            "garbage",
            "http://example.com/wall.png",
            "resource:///org/example/wall.png",
            "file://relative/path.png",
            "file://",
        ] {
            assert_eq!(
                wallpaper_uri_to_path(uri),
                None,
                "uri {uri:?} is not a file"
            );
        }
    }

    #[test]
    fn primary_colors_parse_like_gnome() {
        assert_eq!(
            parse_color("#023c88"),
            Some([2.0 / 255.0, 60.0 / 255.0, 136.0 / 255.0])
        );
        assert_eq!(parse_color(" #FFFFFF\n"), Some([1.0, 1.0, 1.0]));
        assert_eq!(parse_color("023c88"), None);
        assert_eq!(parse_color("#023c8"), None);
        assert_eq!(parse_color("#zz3c88"), None);
    }

    #[test]
    fn load_wallpaper_is_none_for_missing_or_undecodable_files() {
        let output = Size::<i32, Logical>::from((64, 64));
        assert!(load_wallpaper("", output).is_none());
        assert!(load_wallpaper("http://example.com/wall.png", output).is_none());
        assert!(load_wallpaper(
            "file:///definitely/missing/roost-test-wallpaper.png",
            output
        )
        .is_none());
        let dir = tempfile::TempDir::new().expect("tempdir");
        let junk = dir.path().join("junk.png");
        std::fs::write(&junk, b"not an image at all").expect("write junk");
        let uri = format!("file://{}", junk.display());
        assert!(load_wallpaper(&uri, output).is_none());
    }

    /// Checkerboard PNG so the decode path sees real pixels, not a
    /// solid fill that could hide a broken scale.
    fn checker_png(path: &std::path::Path, w: u32, h: u32) {
        let mut image = image::RgbaImage::new(w, h);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            let on = (x / 8 + y / 8) % 2 == 0;
            *pixel = image::Rgba(if on {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            });
        }
        image.save(path).expect("write test png");
    }

    #[test]
    fn drop_file_round_trip_decodes_to_exact_output_size() {
        let _lock = RUNTIME_DIR_LOCK.lock().expect("test lock");
        let dir = tempfile::TempDir::new().expect("tempdir");
        let _runtime = RuntimeDirGuard::point_at(dir.path());
        assert_eq!(
            wallpaper_drop_path(),
            dir.path().join(WALLPAPER_FILE),
            "drop path follows XDG_RUNTIME_DIR"
        );

        let png = dir.path().join("wall.png");
        checker_png(&png, 96, 64);
        let uri = format!("file://{}", png.display());
        std::fs::write(dir.path().join(WALLPAPER_FILE), format!("{uri}\n")).expect("drop uri");

        // `element` re-reads this same one-line file every frame; read
        // it back the same way and decode for a small output.
        let read_back = std::fs::read_to_string(wallpaper_drop_path())
            .map(|text| text.trim().to_owned())
            .expect("drop file reads back");
        assert_eq!(read_back, uri);
        let output = Size::<i32, Logical>::from((48, 32));
        let pixels = load_wallpaper(&read_back, output).expect("real png decodes");
        assert_eq!(pixels.len(), 48 * 32 * 4, "ARGB8888 pixels at output size");
        // A red/blue checker scaled to 48x32 cannot be a flat fill.
        let (chunks, _) = pixels.as_chunks::<4>();
        let distinct: std::collections::HashSet<&[u8]> =
            chunks.iter().map(|px| px.as_slice()).collect();
        assert!(
            distinct.len() > 1,
            "decoded pixels vary with the source checker"
        );
    }

    #[test]
    fn escaped_wallpaper_selects_real_pixels_without_double_decoding() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let picture = dir.path().join("my café %20.png");
        checker_png(&picture, 32, 32);
        image::RgbaImage::from_pixel(32, 32, image::Rgba([0, 255, 0, 255]))
            .save(dir.path().join("my café  .png"))
            .expect("write double-decode decoy");
        let uri = format!("file://{}/my%20caf%C3%A9%20%2520.png", dir.path().display());
        let pixels = load_wallpaper(&uri, (32, 32).into()).expect("escaped real image decodes");
        assert_eq!(
            &pixels[..4],
            &[0, 0, 255, 255],
            "first checker pixel is red, not decoy green"
        );
        assert_eq!(
            &pixels[8 * 4..9 * 4],
            &[255, 0, 0, 255],
            "second checker tile is blue"
        );
    }

    /// One source image covers two output sizes (multi-monitor): each
    /// crop fills its own output exactly — cover, never stretch — so a
    /// 1280x800 panel and a 1920x1080 sibling each get full-bleed
    /// ARGB8888 pixels at their own dimensions.
    #[test]
    fn load_wallpaper_covers_two_output_sizes_with_exact_dims() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let png = dir.path().join("wall.png");
        checker_png(&png, 96, 64);
        let uri = format!("file://{}", png.display());
        for (w, h) in [(48, 32), (64, 64)] {
            let output = Size::<i32, Logical>::from((w, h));
            let pixels = load_wallpaper(&uri, output).expect("real png decodes");
            assert_eq!(
                pixels.len(),
                (w * h * 4) as usize,
                "cover fills {w}x{h} exactly"
            );
            // A red/blue checker at any cover size cannot be a flat
            // fill: bars would mean stretch, one tone would mean crop
            // failure.
            let (chunks, _) = pixels.as_chunks::<4>();
            let distinct: std::collections::HashSet<&[u8]> =
                chunks.iter().map(|px| px.as_slice()).collect();
            assert!(
                distinct.len() > 1,
                "decoded pixels vary with the source checker at {w}x{h}"
            );
        }
    }
    fn test_geometry() -> Geometry {
        Geometry {
            physical: [8, 8],
            origin: [0, 0],
            desktop: [0, 0, 8, 8],
            scale_bits: 1.0f64.to_bits(),
        }
    }
    #[test]
    fn invalid_metadata_never_selects_the_legacy_zoom_path() {
        let _lock = RUNTIME_DIR_LOCK.lock().expect("runtime lock");
        let dir = tempfile::TempDir::new().unwrap();
        let _runtime = RuntimeDirGuard::point_at(dir.path());
        for metadata in [
            "{}",
            "{not-json}",
            "{\"version\":2,\"desktop\":{},\"lock\":{}}",
        ] {
            std::fs::write(
                wallpaper_drop_path(),
                format!("file:///tmp/image.png\n#023c88\n#3584e4\n\n{metadata}\n"),
            )
            .unwrap();
            assert!(Wallpaper::new()
                .refresh((8, 8).into(), test_geometry())
                .is_none());
        }
    }
    #[test]
    fn static_metadata_and_file_replacement_change_all_render_keys() {
        let _lock = RUNTIME_DIR_LOCK.lock().expect("runtime lock");
        let dir = tempfile::TempDir::new().unwrap();
        let _runtime = RuntimeDirGuard::point_at(dir.path());
        let path = dir.path().join("source.png");
        checker_png(&path, 16, 16);
        let uri = format!("file://{}", path.display());
        let mut m = BackgroundMetadata {
            version: 1,
            desktop: PictureSettings::default(),
            lock: PictureSettings {
                placement: roost_shell_control::background::Placement::Centered,
                ..PictureSettings::default()
            },
        };
        let publish = |m: &BackgroundMetadata| {
            std::fs::write(
                wallpaper_drop_path(),
                format!(
                    "{uri}\n#023c88\n#3584e4\n{uri}\n{}\n",
                    serde_json::to_string(m).unwrap()
                ),
            )
            .unwrap()
        };
        publish(&m);
        let mut wallpaper = Wallpaper::new();
        let wait_key = |wallpaper: &mut Wallpaper, previous: Option<&str>| {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                if let Some(key) = wallpaper.refresh((8, 8).into(), test_geometry()) {
                    if previous != Some(key.as_str()) {
                        break key;
                    }
                }
                assert!(
                    std::time::Instant::now() < end,
                    "asynchronous identity observation did not complete"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        let initial = wait_key(&mut wallpaper, None);
        let locked = wallpaper
            .refresh_picture((8, 8).into(), true, test_geometry())
            .unwrap();
        assert_ne!(
            initial, locked,
            "independent lock placement must not reuse desktop pixels"
        );
        m.desktop.shading = roost_shell_control::background::Shading::Horizontal;
        publish(&m);
        let changed = wallpaper.refresh((8, 8).into(), test_geometry()).unwrap();
        assert_ne!(
            initial, changed,
            "same URI and dimensions cannot mask metadata changes"
        );
        let replacement = dir.path().join("replacement.png");
        checker_png(&replacement, 16, 16);
        std::fs::rename(replacement, &path).unwrap();
        assert_ne!(
            changed,
            wait_key(&mut wallpaper, Some(&changed)),
            "replacement inode must invalidate cached pixels even for same dimensions/content"
        );
    }
    #[test]
    fn xml_reference_replacement_changes_epoch_and_rejects_stale_worker() {
        let dir = tempfile::tempdir().unwrap();
        let picture = dir.path().join("frame.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([240, 0, 0, 255]))
            .save(&picture)
            .unwrap();
        let xml = dir.path().join("timeline.xml");
        let document = format!(
            "<background><static><duration>4</duration><file>{}</file></static></background>",
            picture.display()
        );
        std::fs::write(&xml, &document).unwrap();
        let uri = format!("file://{}", xml.display());
        let original = SourceObservation::observe(&uri);
        let sample = original
            .timeline
            .as_ref()
            .unwrap()
            .sample(100.0, [8, 8])
            .unwrap();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 240, 255]))
            .save(&picture)
            .unwrap();
        let changed = SourceObservation::observe(&uri);
        assert_eq!(original.identity, changed.identity);
        assert_ne!(
            original.generation, changed.generation,
            "same XML URI must observe changed referenced-image identity"
        );
        assert!(
            load_animation_wallpaper(
                &uri,
                original.identity.clone(),
                sample.clone(),
                original.references.clone(),
                PictureSettings::default(),
                test_geometry(),
                &BackgroundCaches::default()
            )
            .is_none(),
            "stale source worker cannot publish old image identity"
        );
        std::fs::write(&xml, document.replace("<duration>4", "<duration>8")).unwrap();
        assert!(
            load_animation_wallpaper(
                &uri,
                changed.identity,
                sample,
                changed.references,
                PictureSettings::default(),
                test_geometry(),
                &BackgroundCaches::default()
            )
            .is_none(),
            "stale XML worker cannot publish after descriptor replacement"
        );
        let svg = dir.path().join("image.svg");
        std::fs::write(&svg, "<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap();
        assert!(
            !SourceObservation::observe(&format!("file://{}", svg.display())).xml,
            "SVG must not claim slideshow support"
        );
        std::fs::write(&xml, "<!DOCTYPE background [<!ENTITY a 'x'>]><background/>").unwrap();
        let invalid = SourceObservation::observe(&uri);
        assert!(invalid.xml && invalid.timeline.is_none());
    }

    #[test]
    fn stalled_identity_observation_does_not_block_frames_or_spawn_unbounded_jobs() {
        let mut wallpaper = Wallpaper::new();
        let mut senders = Vec::new();
        for index in 0..MAX_WALLPAPER_SLOTS {
            let (send, result) = std::sync::mpsc::channel();
            senders.push(send);
            wallpaper
                .identity_pending
                .push((format!("stalled-{index}"), result));
        }
        // Four still-live workers exhaust the independent identity budget.
        // Polling another source must return immediately without filesystem I/O.
        let start = std::time::Instant::now();
        assert!(wallpaper
            .poll_identity("file:///another-source.png")
            .is_none());
        assert!(start.elapsed() < std::time::Duration::from_millis(100));
        assert_eq!(wallpaper.identity_pending.len(), MAX_WALLPAPER_SLOTS);
        assert_eq!(senders.len(), MAX_WALLPAPER_SLOTS);
    }
}
