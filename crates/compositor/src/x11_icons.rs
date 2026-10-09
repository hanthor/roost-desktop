//! X11 property IO stays on a worker; only compositor-mapped identities are queried.
use crate::window_icons::Raster;
use smithay::reexports::x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConnectionExt, ImageFormat, ImageOrder, VisualClass},
};
use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};
const MAX_WORDS: u32 = 1_048_576;
type Request = (u32, Vec<(u32, u64)>);
type Reply = (u32, Vec<(u32, u64, Option<Raster>)>);
pub struct Reader {
    requests: SyncSender<Request>,
    replies: Receiver<Reply>,
    last: Instant,
}
impl Reader {
    pub fn new() -> Self {
        let (requests, rx) = mpsc::sync_channel::<Request>(1);
        let (tx, replies) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("tuna-x11-icons".into())
            .spawn(move || {
                let mut connection = None;
                while let Ok((display, windows)) = rx.recv() {
                    if connection.as_ref().is_none_or(|(d, _, _)| *d != display) {
                        connection =
                            smithay::reexports::x11rb::connect(Some(&format!(":{display}")))
                                .ok()
                                .and_then(|(c, _)| {
                                    let atom = c
                                        .intern_atom(false, b"_NET_WM_ICON")
                                        .ok()?
                                        .reply()
                                        .ok()?
                                        .atom;
                                    Some((display, c, atom))
                                });
                    }
                    let Some((_, conn, atom)) = &connection else {
                        continue;
                    };
                    let result = windows
                        .into_iter()
                        .take(512)
                        .map(|(xid, id)| {
                            let raster = conn
                                .get_property(false, xid, *atom, AtomEnum::CARDINAL, 0, MAX_WORDS)
                                .ok()
                                .and_then(|c| c.reply().ok())
                                .filter(|p| p.bytes_after == 0 && p.format == 32)
                                .and_then(|p| parse(&p.value32()?.collect::<Vec<_>>()))
                                .or_else(|| wm_hints(conn, xid));
                            (xid, id, raster)
                        })
                        .collect();
                    let failed = conn.flush().is_err();
                    let _ = tx.try_send((display, result));
                    if failed {
                        connection = None;
                    }
                }
            })
            .expect("X11 icon worker");
        Self {
            requests,
            replies,
            last: Instant::now() - Duration::from_secs(1),
        }
    }
    pub fn poll(
        &mut self,
        display: u32,
        windows: Vec<(u32, u64)>,
    ) -> Vec<(u32, u64, Option<Raster>)> {
        let mut result = Vec::new();
        while let Ok((d, r)) = self.replies.try_recv() {
            if d == display {
                result = r;
            }
        }
        if self.last.elapsed() >= Duration::from_millis(500)
            && self.requests.try_send((display, windows)).is_ok()
        {
            self.last = Instant::now();
        }
        result
    }
}
fn wm_hints(conn: &impl Connection, xid: u32) -> Option<Raster> {
    let reply = conn
        .get_property(false, xid, AtomEnum::WM_HINTS, AtomEnum::WM_HINTS, 0, 9)
        .ok()?
        .reply()
        .ok()?;
    if reply.format != 32 || reply.bytes_after != 0 {
        return None;
    }
    let hints = reply.value32()?.collect::<Vec<_>>();
    if hints.len() < 9 || hints[0] & (1 << 2) == 0 || hints[3] == 0 {
        return None;
    }
    let geometry = conn.get_geometry(hints[3]).ok()?.reply().ok()?;
    let (w, h) = (u32::from(geometry.width), u32::from(geometry.height));
    if w == 0 || w != h || w > 1024 || !matches!(geometry.depth, 24 | 32) {
        return None;
    }
    let setup = conn.setup();
    let screen = setup.roots.iter().find(|r| r.root == geometry.root)?;
    let visual = screen
        .allowed_depths
        .iter()
        .find(|d| d.depth == geometry.depth)?
        .visuals
        .iter()
        .find(|v| {
            v.class == VisualClass::TRUE_COLOR
                && (v.visual_id == screen.root_visual || geometry.depth == 32)
        })?;
    let format = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == geometry.depth)?;
    if !matches!(format.bits_per_pixel, 24 | 32) || !matches!(format.scanline_pad, 8 | 16 | 32) {
        return None;
    }
    let masks = [visual.red_mask, visual.green_mask, visual.blue_mask];
    if masks.iter().any(|m| !mask_valid(*m))
        || masks[0] & masks[1] != 0
        || masks[0] & masks[2] != 0
        || masks[1] & masks[2] != 0
    {
        return None;
    }
    let image = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            hints[3],
            0,
            0,
            geometry.width,
            geometry.height,
            u32::MAX,
        )
        .ok()?
        .reply()
        .ok()?;
    if image.depth != geometry.depth {
        return None;
    }
    let stride = (w * u32::from(format.bits_per_pixel)).div_ceil(u32::from(format.scanline_pad))
        * u32::from(format.scanline_pad)
        / 8;
    if image.data.len() != stride as usize * h as usize {
        return None;
    }
    let mask = if hints[0] & (1 << 5) != 0 && hints[7] != 0 {
        let g = conn.get_geometry(hints[7]).ok()?.reply().ok()?;
        if g.width != geometry.width
            || g.height != geometry.height
            || g.depth != 1
            || g.root != geometry.root
        {
            return None;
        }
        let f = setup.pixmap_formats.iter().find(|f| f.depth == 1)?;
        if f.bits_per_pixel != 1 || !matches!(f.scanline_pad, 8 | 16 | 32) {
            return None;
        }
        let image = conn
            .get_image(ImageFormat::Z_PIXMAP, hints[7], 0, 0, g.width, g.height, 1)
            .ok()?
            .reply()
            .ok()?;
        let stride = w.div_ceil(u32::from(f.scanline_pad)) * u32::from(f.scanline_pad) / 8;
        if image.depth != 1 || image.data.len() != stride as usize * h as usize {
            return None;
        }
        Some((image.data, stride))
    } else {
        None
    };
    let width = w.min(256);
    let mut rgba = Vec::with_capacity(width as usize * width as usize * 4);
    for y in 0..width {
        for x in 0..width {
            let (sx, sy) = (x * w / width, y * h / width);
            let offset = (sy * stride + sx * u32::from(format.bits_per_pixel) / 8) as usize;
            let bytes = &image.data[offset..offset + usize::from(format.bits_per_pixel / 8)];
            let pixel = if setup.image_byte_order == ImageOrder::LSB_FIRST {
                bytes
                    .iter()
                    .enumerate()
                    .fold(0u32, |v, (i, b)| v | u32::from(*b) << (8 * i))
            } else {
                bytes.iter().fold(0u32, |v, b| (v << 8) | u32::from(*b))
            };
            for m in masks {
                rgba.push(channel(pixel, m));
            }
            let alpha = mask.as_ref().map_or(255, |(data, stride)| {
                let byte = data[(sy * stride + sx / 8) as usize];
                let bit = if setup.bitmap_format_bit_order == ImageOrder::LSB_FIRST {
                    sx % 8
                } else {
                    7 - sx % 8
                };
                if byte & (1 << bit) != 0 {
                    255
                } else {
                    0
                }
            });
            rgba.push(alpha);
        }
    }
    Some(Raster { width, rgba })
}
fn mask_valid(mask: u32) -> bool {
    if mask == 0 {
        return false;
    }
    let bits = mask >> mask.trailing_zeros();
    bits & bits.wrapping_add(1) == 0
}
fn channel(pixel: u32, mask: u32) -> u8 {
    let shift = mask.trailing_zeros();
    (((pixel & mask) >> shift) as u64 * 255 / u64::from(mask >> shift)) as u8
}

fn parse(words: &[u32]) -> Option<Raster> {
    let mut remaining = words;
    let mut best = None;
    while remaining.len() >= 2 {
        let (w, h) = (remaining[0], remaining[1]);
        let count = (w as usize).checked_mul(h as usize)?;
        if w == 0 || h == 0 || count > remaining.len() - 2 {
            return None;
        }
        let pixels = &remaining[2..2 + count];
        if w == h && w <= 1024 && best.as_ref().is_none_or(|r: &Raster| w.min(256) >= r.width) {
            let width = w.min(256);
            let mut rgba = Vec::with_capacity(width as usize * width as usize * 4);
            for y in 0..width {
                for x in 0..width {
                    let p = pixels[(y * w / width * w + x * w / width) as usize];
                    rgba.extend_from_slice(&[
                        (p >> 16) as u8,
                        (p >> 8) as u8,
                        p as u8,
                        (p >> 24) as u8,
                    ]);
                }
            }
            best = Some(Raster { width, rgba });
        }
        remaining = &remaining[2 + count..];
    }
    if !remaining.is_empty() {
        return None;
    }
    best
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn true_color_masks_are_contiguous_and_scaled() {
        assert!(mask_valid(0xff0000));
        assert!(mask_valid(0x3ff00000));
        assert!(!mask_valid(0));
        assert!(!mask_valid(0x101));
        assert_eq!(channel(0x3ff00000, 0x3ff00000), 255);
        assert_eq!(channel(0x00008000, 0x0000ff00), 128);
    }
    #[test]
    fn rejects_truncated_and_normalizes_argb() {
        assert!(parse(&[u32::MAX, u32::MAX]).is_none());
        assert!(parse(&[2, 2, 0]).is_none());
        let r = parse(&[1, 1, 0x80402010]).unwrap();
        assert_eq!(r.rgba, [64, 32, 16, 128]);
    }
}
