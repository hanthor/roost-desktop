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
        return decode_jxl(bytes);
    }
    image::load_from_memory(bytes).ok()
}

/// Decode JPEG XL converted to sRGB, as GNOME (mutter's colour
/// management) shows it: GNOME 51's Adwaita wallpapers are Display P3,
/// and read as sRGB unconverted their blues come out dull.
fn decode_jxl(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut jxl = jxl_oxide::JxlImage::builder()
        .read(std::io::Cursor::new(bytes))
        .ok()?;
    jxl.request_color_encoding(jxl_oxide::EnumColourEncoding::srgb(
        jxl_oxide::RenderingIntent::Relative,
    ));
    let render = jxl.render_frame(0).ok()?;
    let mut stream = render.stream();
    let (w, h, c) = (stream.width(), stream.height(), stream.channels());
    let mut buf = vec![0u8; (w as usize) * (h as usize) * (c as usize)];
    stream.write_to_buffer(&mut buf);
    match c {
        1 => image::GrayImage::from_raw(w, h, buf).map(image::DynamicImage::ImageLuma8),
        2 => image::GrayAlphaImage::from_raw(w, h, buf).map(image::DynamicImage::ImageLumaA8),
        3 => image::RgbImage::from_raw(w, h, buf).map(image::DynamicImage::ImageRgb8),
        4 => image::RgbaImage::from_raw(w, h, buf).map(image::DynamicImage::ImageRgba8),
        _ => None,
    }
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

/// GNOME 51's lock-screen background (`unlockDialog.js`): the desktop
/// blurred and dimmed. Fitted against GNOME's own capture: a Gaussian
/// of sigma 20 logical pixels and brightness 0.65.
pub const LOCK_BLUR_SIGMA: f32 = 20.0;
pub const LOCK_BRIGHTNESS: f32 = 0.65;

/// Blur ARGB8888 pixels (`w` x `h`, opaque) with a Gaussian of `sigma`
/// (three box passes each way, which match it closely) and scale every
/// colour by `brightness`. Returns new ARGB8888 pixels, or `None` when
/// the buffer does not hold `w` x `h` pixels.
pub fn blur_dim_argb(argb: &[u8], w: u32, h: u32, sigma: f32, brightness: f32) -> Option<Vec<u8>> {
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 || argb.len() < w * h * 4 {
        return None;
    }
    // Box widths for three passes approximating `sigma` (Kovesi).
    let n = 3.0f32;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut lo = ideal.floor() as i32;
    if lo % 2 == 0 {
        lo -= 1;
    }
    let lo = lo.max(1);
    let hi = lo + 2;
    let m = ((12.0 * sigma * sigma - n * (lo * lo) as f32 - 4.0 * n * lo as f32 - 3.0 * n)
        / (-4.0 * lo as f32 - 4.0))
        .round() as i32;
    let radii: Vec<usize> = (0..3)
        .map(|i| ((if i < m { lo } else { hi }) as usize - 1) / 2)
        .collect();
    let mut planes: Vec<Vec<f32>> = (0..3)
        .map(|c| (0..w * h).map(|i| f32::from(argb[i * 4 + c])).collect())
        .collect();
    let mut tmp = vec![0f32; w * h];
    for plane in &mut planes {
        for &r in &radii {
            box_pass(plane, &mut tmp, w, h, r, true);
            box_pass(&tmp, plane, w, h, r, false);
        }
    }
    let mut out = vec![255u8; w * h * 4];
    for i in 0..w * h {
        for c in 0..3 {
            out[i * 4 + c] = (planes[c][i] * brightness).round().clamp(0.0, 255.0) as u8;
        }
    }
    Some(out)
}

/// One box-blur pass of radius `r`, along rows (`horizontal`) or columns,
/// with edge pixels extended.
fn box_pass(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize, horizontal: bool) {
    let (lines, len) = if horizontal { (h, w) } else { (w, h) };
    let at = |line: usize, k: usize| {
        if horizontal {
            line * w + k
        } else {
            k * w + line
        }
    };
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let first = src[at(line, 0)];
        let last = src[at(line, len - 1)];
        let get = |k: isize| -> f32 {
            if k < 0 {
                first
            } else if k as usize >= len {
                last
            } else {
                src[at(line, k as usize)]
            }
        };
        let mut sum: f32 = (-(r as isize)..=r as isize).map(get).sum();
        for k in 0..len {
            dst[at(line, k)] = sum * norm;
            sum += get(k as isize + r as isize + 1) - get(k as isize - r as isize);
        }
    }
}

/// A drop shadow in CSS terms (`0 dy blur spread rgba(0, 0, 0, alpha)`).
#[derive(Debug, Clone, Copy)]
pub struct Shadow {
    pub dy: f32,
    pub blur: f32,
    pub spread: f32,
    pub alpha: f32,
}

/// A rendered overview card: premultiplied ARGB8888 (B, G, R, A in
/// memory), with `margin` pixels of shadow around the card itself.
#[derive(Debug, Clone)]
pub struct Card {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub margin: u32,
}

/// GNOME's overview workspace card (`.workspace-background`): the part
/// of the desktop at `crop` (x, y, w, h in `full`'s pixels) scaled to
/// `w` x `h`, its corners rounded to `radius`, over a soft shadow.
/// `full` is the output's wallpaper as ARGB8888 (`full_w` x `full_h`);
/// `None` fills the card with `color` (RGB), GNOME's primary-color.
#[allow(clippy::too_many_arguments)]
pub fn card(
    full: Option<(&[u8], u32, u32)>,
    crop: (u32, u32, u32, u32),
    color: [u8; 3],
    w: u32,
    h: u32,
    radius: f32,
    shadow: Shadow,
) -> Option<Card> {
    if w == 0 || h == 0 {
        return None;
    }
    // The card's own pixels, straight RGBA.
    let content: image::RgbaImage = match full {
        Some((argb, fw, fh)) if argb.len() >= (fw * fh * 4) as usize => {
            let (cx, cy, cw, ch) = crop;
            if cw == 0 || ch == 0 || cx + cw > fw || cy + ch > fh {
                return None;
            }
            let mut rgba = image::RgbaImage::new(cw, ch);
            for y in 0..ch {
                for x in 0..cw {
                    let i = (((cy + y) * fw + cx + x) * 4) as usize;
                    rgba.put_pixel(x, y, image::Rgba([argb[i + 2], argb[i + 1], argb[i], 255]));
                }
            }
            image::imageops::resize(&rgba, w, h, image::imageops::FilterType::Triangle)
        }
        _ => image::RgbaImage::from_pixel(w, h, image::Rgba([color[0], color[1], color[2], 255])),
    };
    let margin = (shadow.blur + shadow.spread + shadow.dy.abs()).ceil() as u32 + 1;
    let (cw, ch) = (w + 2 * margin, h + 2 * margin);
    // Shadow: the card's rounded shape grown by `spread`, moved by `dy`,
    // blurred (CSS blur radius is twice the Gaussian sigma).
    let mut mask = image::GrayImage::new(cw, ch);
    let grow = shadow.spread;
    for y in 0..ch {
        for x in 0..cw {
            let px = x as f32 + 0.5 - margin as f32;
            let py = y as f32 + 0.5 - margin as f32 - shadow.dy;
            let cov = coverage(
                px + grow,
                py + grow,
                w as f32 + 2.0 * grow,
                h as f32 + 2.0 * grow,
                radius + grow,
            );
            mask.put_pixel(x, y, image::Luma([(cov * 255.0).round() as u8]));
        }
    }
    if shadow.blur > 0.0 {
        mask = image::imageops::blur(&mask, shadow.blur / 2.0);
    }
    let mut out = vec![0u8; (cw * ch * 4) as usize];
    for y in 0..ch {
        for x in 0..cw {
            let sa = f32::from(mask.get_pixel(x, y)[0]) / 255.0 * shadow.alpha;
            let (cx, cy) = (x as i64 - margin as i64, y as i64 - margin as i64);
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, sa);
            if cx >= 0 && cy >= 0 && (cx as u32) < w && (cy as u32) < h {
                let cov = coverage(cx as f32 + 0.5, cy as f32 + 0.5, w as f32, h as f32, radius);
                if cov > 0.0 {
                    let p = content.get_pixel(cx as u32, cy as u32);
                    // Card over shadow, premultiplied.
                    r = f32::from(p[0]) / 255.0 * cov;
                    g = f32::from(p[1]) / 255.0 * cov;
                    b = f32::from(p[2]) / 255.0 * cov;
                    a = cov + sa * (1.0 - cov);
                }
            }
            let i = ((y * cw + x) * 4) as usize;
            out[i] = (b * 255.0).round() as u8;
            out[i + 1] = (g * 255.0).round() as u8;
            out[i + 2] = (r * 255.0).round() as u8;
            out[i + 3] = (a * 255.0).round() as u8;
        }
    }
    Some(Card {
        pixels: out,
        width: cw,
        height: ch,
        margin,
    })
}

/// How much of the pixel centered at (`x`, `y`) lies inside a `w` x `h`
/// rectangle at the origin with corners rounded to `r`: 0..1, with a
/// one-pixel antialiased edge.
fn coverage(x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    // Distance outside the rounded rectangle (negative inside).
    let qx = (x - w / 2.0).abs() - (w / 2.0 - r);
    let qy = (y - h / 2.0).abs() - (h / 2.0 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
    (0.5 - outside).clamp(0.0, 1.0)
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
    fn cards_round_their_corners_over_a_shadow() {
        let shadow = Shadow {
            dy: 4.0,
            blur: 16.0,
            spread: 4.0,
            alpha: 0.2,
        };
        let c = card(None, (0, 0, 0, 0), [10, 20, 30], 100, 60, 20.0, shadow).unwrap();
        assert_eq!((c.width, c.height), (100 + 2 * c.margin, 60 + 2 * c.margin));
        let px = |x: u32, y: u32| {
            let i = ((y * c.width + x) * 4) as usize;
            [
                c.pixels[i],
                c.pixels[i + 1],
                c.pixels[i + 2],
                c.pixels[i + 3],
            ]
        };
        let m = c.margin;
        // Center: the fill colour, opaque (BGRA in memory).
        assert_eq!(px(m + 50, m + 30), [30, 20, 10, 255]);
        // Card corner pixel: outside the 20px rounding, only shadow.
        let corner = px(m, m);
        assert!(corner[3] < 255 && corner[2] == 0, "{corner:?}");
        // Mid edge, just outside the card: shadow, darker below (dy).
        let above = px(m + 50, m - 2)[3];
        let below = px(m + 50, m + 60 + 1)[3];
        assert!(
            below > above && below > 0,
            "shadow {above} above, {below} below"
        );
    }

    #[test]
    fn cards_crop_the_desktop_they_show() {
        // A 4x4 wallpaper whose bottom half is white: cropping rows 2..4
        // gives an all-white card.
        let mut argb = vec![0u8; 4 * 4 * 4];
        for i in 8..16 {
            argb[i * 4..i * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
        let shadow = Shadow {
            dy: 0.0,
            blur: 0.0,
            spread: 0.0,
            alpha: 0.0,
        };
        let c = card(
            Some((&argb, 4, 4)),
            (0, 2, 4, 2),
            [0, 0, 0],
            4,
            2,
            0.0,
            shadow,
        )
        .unwrap();
        let i = ((c.margin * c.width + c.margin) * 4) as usize;
        assert_eq!(&c.pixels[i..i + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn lock_background_blurs_and_dims() {
        // Left half white, right half black.
        let (w, h) = (64u32, 8u32);
        let mut argb = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 255 } else { 0 };
                argb.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let out = blur_dim_argb(&argb, w, h, 4.0, 0.5).unwrap();
        let px = |x: u32| out[((3 * w + x) * 4) as usize];
        // Far from the edge: dimmed but unblurred.
        assert_eq!(px(0), 128);
        assert_eq!(px(w - 1), 0);
        // At the edge: a soft ramp, monotone.
        assert!(px(w / 2 - 1) > px(w / 2));
        assert!(px(w / 2 - 1) < 128 && px(w / 2) > 0);
        assert!((px(w / 2 - 1) as i32 + px(w / 2) as i32 - 128).abs() <= 2);
        assert_eq!(out[3], 255);
        assert!(blur_dim_argb(&argb, w, h + 1, 4.0, 0.5).is_none());
    }

    #[test]
    fn garbage_is_none() {
        assert!(decode_cover_argb(b"not an image", 8, 8).is_none());
        assert!(decode_cover_argb(&[0xff, 0x0a, 1, 2, 3], 8, 8).is_none());
    }
}
