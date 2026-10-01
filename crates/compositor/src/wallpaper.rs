//! Image background behind the solid clear (settings-compat M4).
//!
//! The shell publishes the Settings wallpaper URI out-of-band: it
//! writes the URI to [`wallpaper_drop_path`] in the runtime dir, and
//! the compositor re-reads that one-line file on every frame. No
//! wallpaper state crosses the control protocol by design. Any
//! failure — missing file, unparsable URI, undecodable image,
//! oversized output — degrades to the solid clear, never an error.

use std::path::PathBuf;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform};

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
    let uri = uri.trim();
    if uri.is_empty() {
        return None;
    }
    if let Some(path) = uri.strip_prefix("file://") {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return Some(path);
        }
        return None;
    }
    None
}

/// Cached wallpaper image, re-scaled to the output that showed it.
#[derive(Debug)]
struct Loaded {
    /// URI text that produced this image (or failed to).
    uri: String,
    /// Output size the pixels match (`None` when the URI failed).
    size: Option<Size<i32, Logical>>,
    /// ARGB8888 pixels at `size`, ready to upload.
    pixels: Option<Vec<u8>>,
}

/// Session wallpaper: polls the drop file, decodes on change. One
/// cache slot per output size (multi-monitor): each output's
/// cover-crop is decoded once and reused while its size and the URI
/// hold. Slots are area-capped individually and count-capped
/// together, so extra outputs cannot grow memory without bound.
#[derive(Debug, Default)]
pub struct Wallpaper {
    loaded: Vec<Loaded>,
}

/// Most cached sizes at once; each slot holds at most
/// `MAX_WALLPAPER_AREA` pixels, so the cache stays bounded while
/// covering the common multi-output case.
const MAX_WALLPAPER_SLOTS: usize = 4;

impl Wallpaper {
    /// Empty wallpaper (solid clear until a drop file appears).
    pub fn new() -> Self {
        Self::default()
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
    ) -> Option<MemoryRenderBufferRenderElement<GlesRenderer>> {
        let output = Size::<i32, Logical>::from((w, h));
        let area = output.w.max(0) as u64 * output.h.max(0) as u64;
        if area == 0 || area > MAX_WALLPAPER_AREA {
            return None;
        }
        let uri = std::fs::read_to_string(wallpaper_drop_path())
            .map(|text| text.trim().to_owned())
            .unwrap_or_default();
        if !self
            .loaded
            .iter()
            .any(|loaded| loaded.uri == uri && loaded.size == Some(output))
        {
            if self.loaded.len() >= MAX_WALLPAPER_SLOTS {
                self.loaded.remove(0);
            }
            self.loaded.push(Loaded {
                uri: uri.clone(),
                size: if uri.is_empty() { None } else { Some(output) },
                pixels: load_wallpaper(&uri, output),
            });
        }
        let loaded = self
            .loaded
            .iter()
            .find(|loaded| loaded.uri == uri && loaded.size == Some(output))?;
        let pixels = loaded.pixels.as_ref()?;
        let size = loaded.size?;
        let buffer = MemoryRenderBuffer::from_slice(
            pixels,
            Fourcc::Argb8888,
            (size.w, size.h),
            1,
            Transform::Normal,
            Some(vec![Rectangle::from_size(Size::<i32, Buffer>::from((
                size.w, size.h,
            )))]),
        );
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            Point::<f64, Physical>::from((0.0, 0.0)),
            &buffer,
            None,
            None,
            Some(size),
            Kind::Unspecified,
        )
        .ok()
    }
}

/// Decode `uri` and scale it to cover `output` exactly (center-crop),
/// returning ARGB8888 pixels. `None` on any failure: the caller
/// keeps the solid clear.
fn load_wallpaper(uri: &str, output: Size<i32, Logical>) -> Option<Vec<u8>> {
    let path = wallpaper_uri_to_path(uri)?;
    let bytes = std::fs::read(&path).ok()?;
    // Bound the decode: refuse absurd files before pixels exist.
    if bytes.len() > 64 * 1024 * 1024 {
        return None;
    }
    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (w, h) = (output.w as u32, output.h as u32);
    if w == 0 || h == 0 {
        return None;
    }
    // Cover-crop: scale to fill, then center-crop the overflow, so
    // the wallpaper covers the output with no bars and no distortion.
    let cover = cover_crop(&image, w, h)?;
    let rgba = cover.into_raw();
    let mut argb = Vec::with_capacity(rgba.len());
    let (chunks, _) = rgba.as_chunks::<4>();
    for px in chunks {
        argb.push(px[3]);
        argb.push(px[2]);
        argb.push(px[1]);
        argb.push(px[0]);
    }
    Some(argb)
}

/// Scale `image` to fill `(w, h)` and center-crop the overflow, so
/// the wallpaper covers the output with no bars and no distortion.
fn cover_crop(image: &image::RgbaImage, w: u32, h: u32) -> Option<image::RgbaImage> {
    use image::GenericImageView;
    let (iw, ih) = image.dimensions();
    if iw == 0 || ih == 0 || w == 0 || h == 0 {
        return None;
    }
    let scale = (w as f64 / iw as f64).max(h as f64 / ih as f64);
    let sw = ((iw as f64 * scale).ceil() as u32).max(w);
    let sh = ((ih as f64 * scale).ceil() as u32).max(h);
    let scaled = image::imageops::resize(image, sw, sh, image::imageops::FilterType::Triangle);
    let x = (sw - w) / 2;
    let y = (sh - h) / 2;
    Some(scaled.view(x, y, w, h).to_image())
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
    fn cover_crop_fills_exact_output_for_wide_and_tall_inputs() {
        let wide = image::RgbaImage::new(160, 90);
        let tall = image::RgbaImage::new(90, 160);
        for source in [&wide, &tall] {
            let out = cover_crop(source, 64, 64).expect("cover always fills");
            assert_eq!(out.dimensions(), (64, 64));
            let out = cover_crop(source, 96, 48).expect("cover always fills");
            assert_eq!(out.dimensions(), (96, 48));
        }
        assert!(cover_crop(&wide, 0, 64).is_none());
        assert!(cover_crop(&wide, 64, 0).is_none());
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
}
