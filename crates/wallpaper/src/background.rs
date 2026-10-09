//! GNOME static placement and color composition, independent of GPU rendering.
use image::RgbaImage;
use tuna_shell_control::background::{PictureSettings, Placement, Shading};

/// Actual physical output dimensions and logical layout, including gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Geometry {
    pub physical: [u32; 2],
    pub origin: [i32; 2],
    pub desktop: [i32; 4],
    pub scale_bits: u64,
}
impl Geometry {
    pub fn scale(&self) -> f64 {
        f64::from_bits(self.scale_bits)
    }
    pub fn valid(&self) -> bool {
        let [w, h] = self.physical;
        w > 0
            && h > 0
            && u64::from(w) * u64::from(h) <= 7680 * 4320
            && self.desktop[2] > 0
            && self.desktop[3] > 0
            && self.scale().is_finite()
            && self.scale() > 0.0
    }
}

/// Render an opaque, fully composed B,G,R,A output. Image transparency reveals
/// the selected gradient; `none` suppresses only the image, not that gradient.
pub fn render(
    image: Option<&RgbaImage>,
    settings: PictureSettings,
    g: Geometry,
) -> Option<Vec<u8>> {
    if !g.valid() {
        return None;
    }
    let [w, h] = g.physical;
    let image = image.filter(|im| im.width() > 0 && im.height() > 0);
    let mut result = vec![0; w as usize * h as usize * 4];
    for y in 0..h {
        for x in 0..w {
            // Mutter uses a two-texel LINEAR/CLAMP texture with normalized
            // coordinates: its first/last quarters retain the endpoint colors.
            let progress = match settings.shading {
                Shading::Solid => 0.0,
                Shading::Horizontal => {
                    (2.0 * (f64::from(x) + 0.5) / f64::from(w) - 0.5).clamp(0.0, 1.0)
                }
                Shading::Vertical => {
                    (2.0 * (f64::from(y) + 0.5) / f64::from(h) - 0.5).clamp(0.0, 1.0)
                }
            };
            let mut color = [0.0; 3];
            for (channel, value) in color.iter_mut().enumerate() {
                *value = f64::from(settings.primary[channel]) * (1.0 - progress)
                    + f64::from(settings.secondary[channel]) * progress;
            }
            if let Some(im) = image {
                if let Some((sx, sy, repeat)) =
                    source_coordinate(settings.placement, g, im.width(), im.height(), x, y)
                {
                    let sample = sample(im, sx, sy, repeat);
                    let alpha = sample[3] / 255.0;
                    for channel in 0..3 {
                        color[channel] = sample[channel] * alpha + color[channel] * (1.0 - alpha);
                    }
                }
            }
            let pos = (y as usize * w as usize + x as usize) * 4;
            result[pos..pos + 4].copy_from_slice(&[
                color[2].round().clamp(0.0, 255.0) as u8,
                color[1].round().clamp(0.0, 255.0) as u8,
                color[0].round().clamp(0.0, 255.0) as u8,
                255,
            ]);
        }
    }
    Some(result)
}

fn source_coordinate(
    p: Placement,
    g: Geometry,
    iw: u32,
    ih: u32,
    x: u32,
    y: u32,
) -> Option<(f64, f64, bool)> {
    let (w, h, iw, ih) = (
        f64::from(g.physical[0]),
        f64::from(g.physical[1]),
        f64::from(iw),
        f64::from(ih),
    );
    let (x, y) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
    let (left, top, dw, dh, repeat) = match p {
        Placement::None => return None,
        Placement::Centered => (
            (w / 2.0).trunc() - (iw / 2.0).trunc(),
            (h / 2.0).trunc() - (ih / 2.0).trunc(),
            iw,
            ih,
            false,
        ),
        Placement::Stretched => (0.0, 0.0, w, h, false),
        Placement::Scaled | Placement::Zoom => {
            let scale = if p == Placement::Scaled {
                (w / iw).min(h / ih)
            } else {
                (w / iw).max(h / ih)
            };
            let (dw, dh) = ((iw * scale).trunc().max(1.0), (ih * scale).trunc().max(1.0));
            (
                (w / 2.0).trunc() - (dw / 2.0).trunc(),
                (h / 2.0).trunc() - (dh / 2.0).trunc(),
                dw,
                dh,
                false,
            )
        }
        Placement::Spanned => {
            let scale = g.scale();
            (
                f64::from(g.desktop[0] - g.origin[0]) * scale,
                f64::from(g.desktop[1] - g.origin[1]) * scale,
                f64::from(g.desktop[2]) * scale,
                f64::from(g.desktop[3]) * scale,
                false,
            )
        }
        Placement::Wallpaper => {
            // Mutter's tiled texture uses logical layout for a global centered
            // tile phase, shared across monitors, instead of restarting at each.
            let scale = g.scale();
            let left = (f64::from(g.desktop[0]) + ((f64::from(g.desktop[2]) - iw) / 2.0).trunc()
                - f64::from(g.origin[0]))
                * scale;
            let top = (f64::from(g.desktop[1]) + ((f64::from(g.desktop[3]) - ih) / 2.0).trunc()
                - f64::from(g.origin[1]))
                * scale;
            (left, top, iw * scale, ih * scale, true)
        }
    };
    if !repeat && (x < left || y < top || x >= left + dw || y >= top + dh) {
        return None;
    }
    Some((
        (x - left) / dw * iw - 0.5,
        (y - top) / dh * ih - 0.5,
        repeat,
    ))
}

fn sample(im: &RgbaImage, x: f64, y: f64, repeat: bool) -> [f64; 4] {
    let (ix, iy) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let coord = |value: i64, size: u32| {
        if repeat {
            value.rem_euclid(i64::from(size)) as u32
        } else {
            value.clamp(0, i64::from(size) - 1) as u32
        }
    };
    let mut result = [0.0; 4];
    // Interpolate premultiplied channels so transparent RGB cannot leak halos.
    for (dx, dy, weight) in [
        (0, 0, (1.0 - fx) * (1.0 - fy)),
        (1, 0, fx * (1.0 - fy)),
        (0, 1, (1.0 - fx) * fy),
        (1, 1, fx * fy),
    ] {
        let pixel = im.get_pixel(coord(ix + dx, im.width()), coord(iy + dy, im.height()));
        let alpha = f64::from(pixel[3]) / 255.0;
        for channel in 0..3 {
            result[channel] += f64::from(pixel[channel]) * alpha * weight;
        }
        result[3] += f64::from(pixel[3]) * weight;
    }
    if result[3] > 0.0 {
        let alpha = result[3] / 255.0;
        for value in &mut result[..3] {
            *value /= alpha;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn geometry(w: u32, h: u32) -> Geometry {
        Geometry {
            physical: [w, h],
            origin: [0, 0],
            desktop: [0, 0, w as i32, h as i32],
            scale_bits: 1.0f64.to_bits(),
        }
    }
    fn pixel(bytes: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
        bytes[(y * width + x) * 4..(y * width + x + 1) * 4]
            .try_into()
            .unwrap()
    }
    fn settings(placement: Placement) -> PictureSettings {
        PictureSettings {
            placement,
            primary: [0, 255, 0],
            secondary: [255, 0, 0],
            shading: Shading::Solid,
        }
    }
    #[test]
    fn static_modes_reveal_distinct_real_image_regions() {
        let mut im = RgbaImage::from_pixel(4, 2, image::Rgba([255, 0, 0, 255]));
        for y in 0..2 {
            for x in 2..4 {
                im.put_pixel(x, y, image::Rgba([0, 0, 255, 255]));
            }
        }
        let g = geometry(8, 8);
        let none = render(Some(&im), settings(Placement::None), g).unwrap();
        assert_eq!(pixel(&none, 8, 4, 4), [0, 255, 0, 255]);
        let centered = render(Some(&im), settings(Placement::Centered), g).unwrap();
        assert_eq!(pixel(&centered, 8, 0, 0), [0, 255, 0, 255]);
        assert_eq!(pixel(&centered, 8, 2, 3), [0, 0, 255, 255]);
        assert_eq!(pixel(&centered, 8, 5, 4), [255, 0, 0, 255]);
        let scaled = render(Some(&im), settings(Placement::Scaled), g).unwrap();
        assert_eq!(pixel(&scaled, 8, 0, 0), [0, 255, 0, 255]);
        assert_eq!(pixel(&scaled, 8, 0, 2), [0, 0, 255, 255]);
        assert_eq!(pixel(&scaled, 8, 7, 5), [255, 0, 0, 255]);
        for mode in [Placement::Stretched, Placement::Zoom] {
            let rendered = render(Some(&im), settings(mode), g).unwrap();
            assert_eq!(pixel(&rendered, 8, 0, 0), [0, 0, 255, 255]);
            assert_eq!(pixel(&rendered, 8, 7, 7), [255, 0, 0, 255]);
        }
    }
    #[test]
    fn gradients_and_transparency_compose_under_none_and_centered() {
        let mut s = settings(Placement::None);
        s.primary = [0, 0, 0];
        s.secondary = [200, 0, 0];
        s.shading = Shading::Horizontal;
        let horizontal = render(None, s, geometry(4, 2)).unwrap();
        assert_eq!(pixel(&horizontal, 4, 0, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&horizontal, 4, 3, 1), [0, 0, 200, 255]);
        s.shading = Shading::Vertical;
        let vertical = render(None, s, geometry(4, 2)).unwrap();
        assert_eq!(pixel(&vertical, 4, 0, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&vertical, 4, 3, 1), [0, 0, 200, 255]);
        let im = RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 255, 128]));
        s.placement = Placement::Centered;
        s.shading = Shading::Solid;
        s.primary = [200, 0, 0];
        let blended = render(Some(&im), s, geometry(1, 1)).unwrap();
        assert_eq!(pixel(&blended, 1, 0, 0), [128, 0, 100, 255]);
    }
    #[test]
    fn spanned_maps_real_global_union_instead_of_repeating_each_monitor() {
        let mut im = RgbaImage::from_pixel(8, 2, image::Rgba([255, 0, 0, 255]));
        for y in 0..2 {
            for x in 4..8 {
                im.put_pixel(x, y, image::Rgba([0, 0, 255, 255]));
            }
        }
        let mut left = geometry(4, 2);
        left.origin = [-4, 0];
        left.desktop = [-4, 0, 8, 2];
        let mut right = left;
        right.origin = [0, 0];
        let a = render(Some(&im), settings(Placement::Spanned), left).unwrap();
        let b = render(Some(&im), settings(Placement::Spanned), right).unwrap();
        assert_eq!(pixel(&a, 4, 3, 1), [0, 0, 255, 255]);
        assert_eq!(pixel(&b, 4, 0, 1), [255, 0, 0, 255]);
        right.physical = [8, 4];
        right.scale_bits = 2.0f64.to_bits();
        let b = render(Some(&im), settings(Placement::Spanned), right).unwrap();
        assert_eq!(pixel(&b, 8, 7, 3), [255, 0, 0, 255]);
    }
    #[test]
    fn tiles_share_global_phase_across_monitor_origins() {
        let mut im = RgbaImage::from_pixel(2, 1, image::Rgba([255, 0, 0, 255]));
        im.put_pixel(1, 0, image::Rgba([0, 0, 255, 255]));
        let mut left = geometry(3, 1);
        left.desktop = [0, 0, 6, 1];
        let mut right = left;
        right.origin = [3, 0];
        let a = render(Some(&im), settings(Placement::Wallpaper), left).unwrap();
        let b = render(Some(&im), settings(Placement::Wallpaper), right).unwrap();
        assert_eq!(pixel(&a, 3, 0, 0), [0, 0, 255, 255]);
        assert_eq!(pixel(&b, 3, 0, 0), [255, 0, 0, 255]);
    }
    #[test]
    fn invalid_geometry_is_rejected_before_allocation() {
        let mut g = geometry(8, 8);
        g.scale_bits = f64::NAN.to_bits();
        assert!(render(None, settings(Placement::None), g).is_none());
        g = geometry(u32::MAX, u32::MAX);
        assert!(render(None, settings(Placement::None), g).is_none());
    }
    #[test]
    fn gradient_matches_actual_two_texel_gl_linear_clamp_sampling() {
        let mut s = settings(Placement::None);
        s.primary = [0, 200, 0];
        s.secondary = [0, 0, 200];
        s.shading = Shading::Horizontal;
        let actual = render(None, s, geometry(8, 1)).unwrap();
        // Retained Mesa EGL sampler operation; not a GNOME desktop capture.
        let expected = [
            [0, 200, 0, 255],
            [0, 200, 0, 255],
            [25, 175, 0, 255],
            [75, 125, 0, 255],
            [125, 75, 0, 255],
            [175, 25, 0, 255],
            [200, 0, 0, 255],
            [200, 0, 0, 255],
        ];
        for (index, expected) in expected.into_iter().enumerate() {
            assert_eq!(pixel(&actual, 8, index, 0), expected);
        }
    }
}
