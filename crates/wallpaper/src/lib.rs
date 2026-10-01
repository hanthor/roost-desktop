//! Wallpaper decoding for the compositor.
//!
//! A crate of its own so the image work is compiled optimised even in
//! debug builds (see the workspace `profile.dev.package` table): the
//! decoders and the resize are generic, so they would otherwise be
//! instantiated, unoptimised, inside the compositor. GNOME 51's default
//! wallpapers are 4096x4096 JPEG XL files.

use image::GenericImageView;

/// Decode `bytes` and cover-scale them to exactly `w` x `h`, returning
/// ARGB8888 pixels (B, G, R, A in memory). `None` on any failure.
pub fn decode_cover_argb(bytes: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    if w == 0 || h == 0 {
        return None;
    }
    let image = decode(bytes)?.to_rgba8();
    let cover = cover_crop(&image, w, h)?;
    let mut argb = cover.into_raw();
    // RGBA to little-endian ARGB8888: swap R and B in place.
    for px in argb.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Some(argb)
}

/// Decode any format `image` reads, plus JPEG XL (gnome-backgrounds'
/// adwaita-l.jxl and adwaita-d.jxl).
pub fn decode(bytes: &[u8]) -> Option<image::DynamicImage> {
    if is_jxl(bytes) {
        let decoder = jxl_oxide::integration::JxlDecoder::new(std::io::Cursor::new(bytes)).ok()?;
        return image::DynamicImage::from_decoder(decoder).ok();
    }
    image::load_from_memory(bytes).ok()
}

/// JPEG XL signatures: the bare codestream, or the ISO BMFF container.
pub fn is_jxl(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0x0a])
        || bytes.starts_with(&[
            0, 0, 0, 0x0c, b'J', b'X', b'L', b' ', 0x0d, 0x0a, 0x87, 0x0a,
        ])
}

/// Scale `image` to fill `(w, h)` and center-crop the overflow, so the
/// wallpaper covers the output with no bars and no distortion (GNOME's
/// "zoom" picture option).
pub fn cover_crop(image: &image::RgbaImage, w: u32, h: u32) -> Option<image::RgbaImage> {
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

    #[test]
    fn cover_crop_fills_exact_output_for_wide_and_tall_inputs() {
        let wide = image::RgbaImage::new(160, 90);
        let tall = image::RgbaImage::new(90, 160);
        for source in [&wide, &tall] {
            assert_eq!(cover_crop(source, 64, 64).unwrap().dimensions(), (64, 64));
            assert_eq!(cover_crop(source, 96, 48).unwrap().dimensions(), (96, 48));
        }
        assert!(cover_crop(&wide, 0, 64).is_none());
        assert!(cover_crop(&wide, 64, 0).is_none());
    }

    #[test]
    fn pixels_are_argb8888_in_memory_order() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 255, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let pixels = decode_cover_argb(&png, 2, 2).unwrap();
        assert_eq!(
            &pixels[..4],
            &[255, 0, 0, 255],
            "blue is B=255, G=0, R=0, A=255"
        );
    }

    #[test]
    fn jpeg_xl_is_recognised() {
        assert!(is_jxl(&[0xff, 0x0a, 0, 0]));
        assert!(is_jxl(b"\0\0\0\x0cJXL \r\n\x87\n...."));
        assert!(!is_jxl(b"\x89PNG\r\n"));
    }

    #[test]
    fn garbage_is_none() {
        assert!(decode_cover_argb(b"not an image", 8, 8).is_none());
        assert!(decode_cover_argb(&[0xff, 0x0a, 1, 2, 3], 8, 8).is_none());
    }
}
