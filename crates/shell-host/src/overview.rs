//! Minimal overview rendering: solid blocks, no text (002 journey).
//!
//! The shell has no toolkit yet (GTK verdict pending the 000 spike),
//! so the overview renders window placeholders as solid rectangles on
//! a shared-memory canvas: one block per window in grid order, a
//! bright border on the selected window, small squares for pinned
//! favorites. This proves the real pipeline — layer surface,
//! shm buffer attach, compositor scanout, visible Super reaction —
//! while text, icons, and search-field rendering wait for the toolkit.
//! Pure drawing lives in [`OverviewCanvas`] (unit-tested pixels);
//! [`OverviewSurface`] owns the Wayland objects.

use crate::model::ShellModel;
use crate::notifications::Urgency;
use crate::tiles::{Tile, TileState};

/// Canvas backing pixel format (matches `wl_shm` `Argb8888`).
pub const BYTES_PER_PIXEL: usize = 4;
/// Backdrop color (opaque dark).
const BG: [u8; 4] = [0x1e, 0x1a, 0x1a, 0xff];
/// Window block fill.
const BLOCK: [u8; 4] = [0x42, 0x3a, 0x3a, 0xff];
/// Selected window block fill.
const BLOCK_SELECTED: [u8; 4] = [0x54, 0x4a, 0x4a, 0xff];
/// Selection border + favorite squares.
const ACCENT: [u8; 4] = [0xe8, 0xe8, 0xe8, 0xff];

const BLOCK_W: i32 = 280;
const BLOCK_H: i32 = 180;
const GAP: i32 = 24;
const ORIGIN_X: i32 = 96;
const ORIGIN_Y: i32 = 120;
const COLS: i32 = 4;
/// How many window blocks are ever drawn (grid overflow is clipped,
/// never allocated).
pub const MAX_DRAWN_WINDOWS: usize = 16;
const FAV_SIZE: i32 = 40;
const FAV_GAP: i32 = 12;
const FAV_Y: i32 = 640;
/// How many favorite squares are ever drawn.
pub const MAX_DRAWN_FAVORITES: usize = 12;

/// Pixel canvas plus the layout math, with no Wayland dependency.
#[derive(Debug, Default)]
pub struct OverviewCanvas {
    width: i32,
    height: i32,
    pixels: Vec<u8>,
}

impl OverviewCanvas {
    /// Blank canvas; zero-size canvases hold no pixels and draw nothing.
    pub fn new(width: i32, height: i32) -> Self {
        let len = (width.max(0) as usize)
            .saturating_mul(height.max(0) as usize)
            .saturating_mul(BYTES_PER_PIXEL);
        let mut pixels = vec![0u8; len];
        let (chunks, _) = pixels.as_chunks_mut::<BYTES_PER_PIXEL>();
        for px in chunks {
            px.copy_from_slice(&BG);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Canvas width in pixels.
    pub fn width(&self) -> i32 {
        self.width
    }

    /// Canvas height in pixels.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Raw `Argb8888` bytes, row-major.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    fn put(&mut self, x: i32, y: i32, color: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let at = (y as usize * self.width as usize + x as usize) * BYTES_PER_PIXEL;
        if let Some(slot) = self.pixels.get_mut(at..at + BYTES_PER_PIXEL) {
            slot.copy_from_slice(&color);
        }
    }

    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: [u8; 4]) {
        for dy in 0..h {
            for dx in 0..w {
                self.put(x + dx, y + dy, color);
            }
        }
    }

    fn border(&mut self, x: i32, y: i32, w: i32, h: i32, thick: i32, color: [u8; 4]) {
        self.rect(x, y, w, thick, color);
        self.rect(x, y + h - thick, w, thick, color);
        self.rect(x, y, thick, h, color);
        self.rect(x + w - thick, y, thick, h, color);
    }

    /// Paint the model: backdrop, one block per window in list order
    /// (clipped to [`MAX_DRAWN_WINDOWS`]), accent border on the
    /// selected window, `fav_count` squares (clipped to
    /// [`MAX_DRAWN_FAVORITES`]).
    pub fn render(&mut self, model: &ShellModel, fav_count: usize) {
        self.rect(0, 0, self.width, self.height, BG);
        let selected = model.selected();
        let active = model.active_workspace();
        let windows: Vec<&crate::model::WindowEntry> = model
            .windows()
            .iter()
            .filter(|w| w.workspace == active)
            .take(MAX_DRAWN_WINDOWS)
            .collect();
        for (i, window) in windows.iter().enumerate() {
            let col = i as i32 % COLS;
            let row = i as i32 / COLS;
            let x = ORIGIN_X + col * (BLOCK_W + GAP);
            let y = ORIGIN_Y + row * (BLOCK_H + GAP);
            let is_selected = selected == Some(window.id);
            self.rect(
                x,
                y,
                BLOCK_W,
                BLOCK_H,
                if is_selected { BLOCK_SELECTED } else { BLOCK },
            );
            if is_selected {
                self.border(x - 4, y - 4, BLOCK_W + 8, BLOCK_H + 8, 4, ACCENT);
            }
        }
        for i in 0..fav_count.min(MAX_DRAWN_FAVORITES) {
            self.rect(
                ORIGIN_X + i as i32 * (FAV_SIZE + FAV_GAP),
                FAV_Y,
                FAV_SIZE,
                FAV_SIZE,
                ACCENT,
            );
        }
    }

    /// Search box row with the live query in micro-glyphs plus a
    /// caret block. Called after [`render`](Self::render); unknown
    /// chars (and spaces) leave gaps. Clipped, never panics.
    pub fn draw_query(&mut self, query: &str) {
        const PAD: i32 = 8;
        const QUERY_Y: i32 = 32;
        const MAX_QUERY_CHARS: usize = 48;
        let shown: String = query.chars().take(MAX_QUERY_CHARS).collect();
        let glyphs = shown.chars().count() as i32;
        let box_w = (glyphs + 1) * GLYPH_ADVANCE + PAD * 2;
        let box_h = 5 * FONT_SCALE + PAD * 2;
        self.rect(ORIGIN_X, QUERY_Y, box_w, box_h, BLOCK);
        self.border(ORIGIN_X, QUERY_Y, box_w, box_h, 2, ACCENT);
        let stride = self.width as usize * BYTES_PER_PIXEL;
        let mut gx = ORIGIN_X + PAD;
        let gy = QUERY_Y + PAD;
        for ch in shown.chars() {
            if ch == ' ' {
                gx += GLYPH_ADVANCE;
                continue;
            }
            if let Some(glyph) = glyph_index(ch) {
                blit_glyph(&mut self.pixels, stride, gx, gy, glyph, ACCENT);
            }
            gx += GLYPH_ADVANCE;
        }
        self.rect(gx, gy, FONT_SCALE, 5 * FONT_SCALE, ACCENT);
    }
}

/// Paint the panel strip: backdrop plus a lighter Activities corner.
/// Error-indicator color (opaque brick).
const WARN: [u8; 4] = [0xc0, 0x40, 0x30, 0xff];

/// 3x5 micro-glyphs (`0-9`, `:`, then `a-z`), rows top to bottom,
/// low three bits left to right. Enough for the panel clock and the
/// overview search box at strip scale; fuller text waits for the
/// toolkit.
const GLYPHS: [[u8; 5]; 37] = [
    [0b111, 0b101, 0b101, 0b101, 0b111], // 0
    [0b010, 0b110, 0b010, 0b010, 0b111], // 1
    [0b111, 0b001, 0b111, 0b100, 0b111], // 2
    [0b111, 0b001, 0b111, 0b001, 0b111], // 3
    [0b101, 0b101, 0b111, 0b001, 0b001], // 4
    [0b111, 0b100, 0b111, 0b001, 0b111], // 5
    [0b111, 0b100, 0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b010, 0b010, 0b010], // 7
    [0b111, 0b101, 0b111, 0b101, 0b111], // 8
    [0b111, 0b101, 0b111, 0b001, 0b111], // 9
    [0b000, 0b010, 0b000, 0b010, 0b000], // :
    [0b010, 0b000, 0b010, 0b101, 0b011], // a
    [0b100, 0b100, 0b110, 0b101, 0b110], // b
    [0b000, 0b011, 0b100, 0b100, 0b011], // c
    [0b001, 0b001, 0b011, 0b101, 0b011], // d
    [0b000, 0b010, 0b101, 0b110, 0b010], // e
    [0b001, 0b001, 0b111, 0b001, 0b001], // f
    [0b000, 0b011, 0b101, 0b011, 0b001], // g (descender clipped)
    [0b100, 0b100, 0b110, 0b101, 0b101], // h
    [0b010, 0b000, 0b010, 0b010, 0b010], // i
    [0b001, 0b000, 0b001, 0b101, 0b010], // j
    [0b100, 0b101, 0b110, 0b101, 0b101], // k
    [0b010, 0b010, 0b010, 0b010, 0b010], // l
    [0b000, 0b101, 0b111, 0b101, 0b101], // m
    [0b000, 0b110, 0b101, 0b101, 0b101], // n
    [0b000, 0b010, 0b101, 0b101, 0b010], // o
    [0b000, 0b110, 0b101, 0b110, 0b100], // p
    [0b000, 0b011, 0b101, 0b011, 0b001], // q
    [0b000, 0b101, 0b110, 0b100, 0b100], // r
    [0b011, 0b100, 0b010, 0b001, 0b110], // s
    [0b010, 0b111, 0b010, 0b010, 0b010], // t
    [0b000, 0b101, 0b101, 0b101, 0b111], // u
    [0b000, 0b101, 0b101, 0b101, 0b010], // v
    [0b000, 0b101, 0b101, 0b111, 0b101], // w
    [0b000, 0b101, 0b010, 0b101, 0b101], // x
    [0b000, 0b101, 0b101, 0b011, 0b001], // y (descender clipped)
    [0b000, 0b111, 0b001, 0b010, 0b111], // z
];
/// Glyph advance (3px glyph + 1px tracking) times the strip scale.
const FONT_SCALE: i32 = 3;
const GLYPH_ADVANCE: i32 = 4 * FONT_SCALE;
/// Indicator square size and spacing at the strip's right edge.
const DOT_SIZE: i32 = 14;
const DOT_GAP: i32 = 8;
const DOT_MARGIN: i32 = 10;

fn put_pixel(pixels: &mut [u8], stride: usize, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 {
        return;
    }
    let at = y as usize * stride + x as usize * BYTES_PER_PIXEL;
    if let Some(slot) = pixels.get_mut(at..at + BYTES_PER_PIXEL) {
        slot.copy_from_slice(&color);
    }
}

fn blit_glyph(pixels: &mut [u8], stride: usize, x: i32, y: i32, glyph: u8, color: [u8; 4]) {
    for row in 0..5 {
        for col in 0..3 {
            if GLYPHS[glyph as usize][row as usize] & (1 << (2 - col)) != 0 {
                for dy in 0..FONT_SCALE {
                    for dx in 0..FONT_SCALE {
                        put_pixel(
                            pixels,
                            stride,
                            x + col * FONT_SCALE + dx,
                            y + row * FONT_SCALE + dy,
                            color,
                        );
                    }
                }
            }
        }
    }
}

/// Micro-glyph index, or `None` for a gap. ASCII uppercase folds
/// to lowercase: 3x5 cells cannot distinguish case, and matching is
/// case-insensitive anyway.
fn glyph_index(ch: char) -> Option<u8> {
    match ch {
        '0'..='9' => Some(ch as u8 - b'0'),
        ':' => Some(10),
        'a'..='z' => Some(11 + (ch as u8 - b'a')),
        'A'..='Z' => Some(11 + (ch as u8 - b'A')),
        _ => None,
    }
}

pub fn paint_panel(pixels: &mut [u8], width: i32, height: i32, clock: &str, tiles: &[Tile]) {
    let stride = width as usize * BYTES_PER_PIXEL;
    for (i, byte) in pixels.iter_mut().enumerate() {
        let channel = i % BYTES_PER_PIXEL;
        *byte = BG[channel];
    }
    // Activities corner block, 96xheight, slightly lifted.
    let corner: [u8; 4] = [0x3a, 0x34, 0x34, 0xff];
    for y in 0..height {
        for x in 0..96.min(width) {
            let at = y as usize * stride + x as usize * BYTES_PER_PIXEL;
            if let Some(slot) = pixels.get_mut(at..at + BYTES_PER_PIXEL) {
                slot.copy_from_slice(&corner);
            }
        }
    }
    // Centered clock (`HH:MM` only; anything else is skipped glyph by
    // glyph so a malformed string degrades to gaps, never garbage).
    let text_w = clock.chars().count() as i32 * GLYPH_ADVANCE - (FONT_SCALE - 1);
    let mut cx = (width - text_w) / 2;
    let cy = (height - 5 * FONT_SCALE) / 2;
    for ch in clock.chars() {
        if let Some(glyph) = glyph_index(ch) {
            blit_glyph(pixels, stride, cx, cy, glyph, ACCENT);
        }
        cx += GLYPH_ADVANCE;
    }
    // Right-edge indicators in strip order: filled when ready, hollow
    // when loading/disconnected, brick when error. The power tile
    // fills from the bottom by battery percent when known.
    let mut dx = width - DOT_MARGIN - DOT_SIZE;
    for tile in tiles {
        let y0 = (height - DOT_SIZE) / 2;
        let (fill, frame) = match tile.state {
            TileState::Ready => (ACCENT, ACCENT),
            TileState::Error => (WARN, WARN),
            TileState::Loading | TileState::Disconnected => (BG, ACCENT),
        };
        for y in 0..DOT_SIZE {
            for x in 0..DOT_SIZE {
                let edge = x == 0 || y == 0 || x == DOT_SIZE - 1 || y == DOT_SIZE - 1;
                let level_ok = tile
                    .level
                    .is_some_and(|level| y >= DOT_SIZE - DOT_SIZE * level as i32 / 100);
                let color = if edge {
                    frame
                } else if tile.level.is_some() {
                    if level_ok {
                        fill
                    } else {
                        BG
                    }
                } else {
                    fill
                };
                put_pixel(pixels, stride, dx + x, y0 + y, color);
            }
        }
        dx -= DOT_SIZE + DOT_GAP;
    }
}

/// Alt-Tab switcher strip: MRU-ordered blocks with the selection
/// highlighted, centered in a bottom-anchored strip surface.
const SWITCHER_SLOT_W: i32 = 160;
const SWITCHER_SLOT_H: i32 = 100;
const SWITCHER_GAP: i32 = 16;
const SWITCHER_PAD: i32 = 24;
/// Fixed strip height the panel requests from the compositor.
pub const SWITCHER_STRIP_H: i32 = SWITCHER_PAD * 2 + SWITCHER_SLOT_H;
/// How many MRU entries are ever drawn (older ones stay reachable by
/// stepping; the strip never grows unbounded).
pub const MAX_DRAWN_SWITCHER: usize = 10;

/// Pixel canvas for one switcher frame, with no Wayland dependency.
#[derive(Debug, Default)]
pub struct SwitcherCanvas {
    width: i32,
    height: i32,
    pixels: Vec<u8>,
}

impl SwitcherCanvas {
    /// Blank canvas; zero-size canvases hold no pixels and draw nothing.
    pub fn new(width: i32, height: i32) -> Self {
        let len = (width.max(0) as usize)
            .saturating_mul(height.max(0) as usize)
            .saturating_mul(BYTES_PER_PIXEL);
        let mut pixels = vec![0u8; len];
        let (chunks, _) = pixels.as_chunks_mut::<BYTES_PER_PIXEL>();
        for px in chunks {
            px.copy_from_slice(&BG);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Raw `Argb8888` bytes, row-major.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Draw `order` (MRU, front = most recent) centered, highlighting
    /// `selected`. Pure backdrop when empty.
    pub fn render(&mut self, order: &[u64], selected: Option<u64>) {
        let shown = order.len().min(MAX_DRAWN_SWITCHER);
        if shown == 0 || self.width <= 0 || self.height <= 0 {
            return;
        }
        let row_w = shown as i32 * SWITCHER_SLOT_W + (shown as i32 - 1) * SWITCHER_GAP;
        let mut x = (self.width - row_w) / 2;
        let y = (self.height - SWITCHER_SLOT_H) / 2;
        for id in order.iter().take(shown) {
            let is_selected = selected == Some(*id);
            self.rect(
                x,
                y,
                SWITCHER_SLOT_W,
                SWITCHER_SLOT_H,
                if is_selected { BLOCK_SELECTED } else { BLOCK },
            );
            if is_selected {
                self.border(
                    x - 4,
                    y - 4,
                    SWITCHER_SLOT_W + 8,
                    SWITCHER_SLOT_H + 8,
                    4,
                    ACCENT,
                );
            }
            x += SWITCHER_SLOT_W + SWITCHER_GAP;
        }
    }

    fn put(&mut self, x: i32, y: i32, color: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let at = (y as usize * self.width as usize + x as usize) * BYTES_PER_PIXEL;
        if let Some(slot) = self.pixels.get_mut(at..at + BYTES_PER_PIXEL) {
            slot.copy_from_slice(&color);
        }
    }

    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: [u8; 4]) {
        for dy in 0..h {
            for dx in 0..w {
                self.put(x + dx, y + dy, color);
            }
        }
    }

    fn border(&mut self, x: i32, y: i32, w: i32, h: i32, thick: i32, color: [u8; 4]) {
        self.rect(x, y, w, thick, color);
        self.rect(x, y + h - thick, w, thick, color);
        self.rect(x, y, thick, h, color);
        self.rect(x + w - thick, y, thick, h, color);
    }
}

/// Notification banner stack: one row per banner (002 notifications).
const BANNER_PAD: i32 = 12;
const BANNER_GAP: i32 = 8;
const BANNER_ROW_H: i32 = 56;
const BANNER_ROW_EXPANDED_H: i32 = 96;
/// Fixed strip width the panel requests from the compositor.
pub const BANNER_STRIP_W: i32 = 360;
/// How many banners are ever drawn (the center caps the queue here).
pub const MAX_DRAWN_BANNERS: usize = 3;

/// Strip height for banners with these expanded flags.
pub fn banner_strip_height(expanded: &[bool]) -> i32 {
    let mut height = BANNER_PAD * 2;
    for (i, expanded) in expanded.iter().enumerate() {
        if i > 0 {
            height += BANNER_GAP;
        }
        height += if *expanded {
            BANNER_ROW_EXPANDED_H
        } else {
            BANNER_ROW_H
        };
    }
    height
}

/// Pixel canvas for one banner frame, with no Wayland dependency.
/// Rows render oldest-first: plain fill normally, highlighted fill
/// with an accent border for critical urgency, taller rows expanded.
#[derive(Debug, Default)]
pub struct BannerCanvas {
    width: i32,
    height: i32,
    pixels: Vec<u8>,
}

impl BannerCanvas {
    /// Blank canvas; zero-size canvases hold no pixels and draw nothing.
    pub fn new(width: i32, height: i32) -> Self {
        let len = (width.max(0) as usize)
            .saturating_mul(height.max(0) as usize)
            .saturating_mul(BYTES_PER_PIXEL);
        let mut pixels = vec![0u8; len];
        let (chunks, _) = pixels.as_chunks_mut::<BYTES_PER_PIXEL>();
        for px in chunks {
            px.copy_from_slice(&BG);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Raw `Argb8888` bytes, row-major.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Draw `(urgency, expanded)` rows top to bottom.
    pub fn render(&mut self, rows: &[(Urgency, bool)]) {
        if self.width <= 0 || self.height <= 0 {
            return;
        }
        let mut y = BANNER_PAD;
        for (urgency, expanded) in rows.iter().take(MAX_DRAWN_BANNERS) {
            let h = if *expanded {
                BANNER_ROW_EXPANDED_H
            } else {
                BANNER_ROW_H
            };
            let critical = *urgency == Urgency::Critical;
            self.rect(
                BANNER_PAD,
                y,
                self.width - BANNER_PAD * 2,
                h,
                if critical { BLOCK_SELECTED } else { BLOCK },
            );
            if critical {
                self.border(
                    BANNER_PAD - 3,
                    y - 3,
                    self.width - BANNER_PAD * 2 + 6,
                    h + 6,
                    3,
                    ACCENT,
                );
            }
            y += h + BANNER_GAP;
        }
    }

    fn put(&mut self, x: i32, y: i32, color: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let at = (y as usize * self.width as usize + x as usize) * BYTES_PER_PIXEL;
        if let Some(slot) = self.pixels.get_mut(at..at + BYTES_PER_PIXEL) {
            slot.copy_from_slice(&color);
        }
    }

    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: [u8; 4]) {
        for dy in 0..h {
            for dx in 0..w {
                self.put(x + dx, y + dy, color);
            }
        }
    }

    fn border(&mut self, x: i32, y: i32, w: i32, h: i32, thick: i32, color: [u8; 4]) {
        self.rect(x, y, w, thick, color);
        self.rect(x, y + h - thick, w, thick, color);
        self.rect(x, y, thick, h, color);
        self.rect(x + w - thick, y, thick, h, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WindowEntry;

    fn two_windows() -> ShellModel {
        let mut model = ShellModel::new();
        model.apply_window_list(
            vec![
                WindowEntry::new(1, "alpha", false),
                WindowEntry::new(2, "beta", true),
            ],
            vec![0],
        );
        model
    }

    fn pixel(canvas: &OverviewCanvas, x: i32, y: i32) -> [u8; 4] {
        let at = (y as usize * canvas.width() as usize + x as usize) * BYTES_PER_PIXEL;
        canvas.pixels()[at..at + BYTES_PER_PIXEL]
            .try_into()
            .unwrap()
    }

    #[test]
    fn blocks_follow_list_order_with_selected_border() {
        let mut canvas = OverviewCanvas::new(1280, 800);
        canvas.render(&two_windows(), 0);
        // First block interior is the plain fill.
        assert_eq!(pixel(&canvas, ORIGIN_X + 10, ORIGIN_Y + 10), BLOCK);
        // Second block is selected: brighter fill plus accent border.
        let sx = ORIGIN_X + BLOCK_W + GAP;
        assert_eq!(pixel(&canvas, sx + 20, ORIGIN_Y + 20), BLOCK_SELECTED);
        assert_eq!(pixel(&canvas, sx - 2, ORIGIN_Y - 2), ACCENT);
        // Far corner stays backdrop.
        assert_eq!(pixel(&canvas, 1270, 790), BG);
    }

    #[test]
    fn empty_model_is_pure_backdrop() {
        let mut canvas = OverviewCanvas::new(640, 480);
        canvas.render(&ShellModel::new(), 0);
        let (chunks, _) = canvas.pixels().as_chunks::<4>();
        assert!(chunks.iter().all(|px| *px == BG));
    }

    #[test]
    fn favorites_draw_as_squares() {
        let mut canvas = OverviewCanvas::new(1280, 800);
        canvas.render(&ShellModel::new(), 3);
        assert_eq!(pixel(&canvas, ORIGIN_X + 5, FAV_Y + 5), ACCENT);
        assert_eq!(
            pixel(&canvas, ORIGIN_X + 2 * (FAV_SIZE + FAV_GAP) + 5, FAV_Y + 5),
            ACCENT
        );
        assert_eq!(pixel(&canvas, ORIGIN_X, FAV_Y - 10), BG);
    }

    #[test]
    fn render_shows_only_active_workspace() {
        use crate::model::SnapshotView;
        let mut model = ShellModel::new();
        model.apply_snapshot_view(SnapshotView {
            windows: vec![
                WindowEntry::new(1, "alpha", false).with_workspace(0),
                WindowEntry::new(2, "beta", true).with_workspace(1),
            ],
            workspaces: vec![0, 1],
            active_workspace: 1,
        });
        // Only beta (ws 1) draws: first block is the selected fill.
        let mut canvas = OverviewCanvas::new(1280, 800);
        canvas.render(&model, 0);
        assert_eq!(pixel(&canvas, ORIGIN_X + 10, ORIGIN_Y + 10), BLOCK_SELECTED);
        // Flip to workspace 0: the same slot now shows plain alpha.
        model.set_active_workspace(0);
        let mut canvas = OverviewCanvas::new(1280, 800);
        canvas.render(&model, 0);
        assert_eq!(pixel(&canvas, ORIGIN_X + 10, ORIGIN_Y + 10), BLOCK);
    }

    #[test]
    fn zero_size_canvas_draws_nothing() {
        let mut canvas = OverviewCanvas::new(0, 0);
        canvas.render(&two_windows(), 5);
        canvas.draw_query("term");
        assert!(canvas.pixels().is_empty());
    }

    #[test]
    fn query_row_shows_box_glyphs_and_caret() {
        let mut canvas = OverviewCanvas::new(1280, 800);
        canvas.render(&ShellModel::new(), 0);
        canvas.draw_query("ab");
        // Box border is accent, interior carries glyph pixels: the
        // `a` cell's top row (010) has its middle pixel set.
        assert_eq!(pixel(&canvas, ORIGIN_X, 32), ACCENT);
        let gx = ORIGIN_X + 8;
        let gy = 32 + 8;
        assert_eq!(pixel(&canvas, gx + 3, gy), ACCENT);
        assert_eq!(pixel(&canvas, gx, gy), BLOCK);
        // Caret block sits one advance past the last glyph.
        let caret_x = gx + 2 * GLYPH_ADVANCE;
        assert_eq!(pixel(&canvas, caret_x + 1, gy + 2), ACCENT);
        // Unknown chars leave gaps without panicking.
        canvas.draw_query("a%b");
        // Far corner stays backdrop.
        assert_eq!(pixel(&canvas, 1270, 790), BG);
    }

    #[test]
    fn switcher_renders_mru_order_with_selection() {
        let mut canvas = SwitcherCanvas::new(1280, SWITCHER_STRIP_H);
        canvas.render(&[3, 1, 2], Some(1));
        // Three slots, centered: slot 0 (id 3) plain, slot 1 (id 1)
        // selected, slot 2 (id 2) plain.
        let row_w = 3 * SWITCHER_SLOT_W + 2 * SWITCHER_GAP;
        let x0 = (1280 - row_w) / 2;
        let y = (SWITCHER_STRIP_H - SWITCHER_SLOT_H) / 2;
        assert_eq!(pixel_at(&canvas, x0 + 10, y + 10), BLOCK);
        assert_eq!(
            pixel_at(&canvas, x0 + SWITCHER_SLOT_W + SWITCHER_GAP + 10, y + 10),
            BLOCK_SELECTED
        );
        assert_eq!(
            pixel_at(&canvas, x0 + SWITCHER_SLOT_W + SWITCHER_GAP - 2, y - 2),
            ACCENT
        );
        assert_eq!(
            pixel_at(
                &canvas,
                x0 + 2 * (SWITCHER_SLOT_W + SWITCHER_GAP) + 10,
                y + 10
            ),
            BLOCK
        );
        // Strip padding stays backdrop.
        assert_eq!(pixel_at(&canvas, 5, 5), BG);
    }

    #[test]
    fn switcher_empty_is_pure_backdrop() {
        let mut canvas = SwitcherCanvas::new(1280, SWITCHER_STRIP_H);
        canvas.render(&[], None);
        let (chunks, _) = canvas.pixels().as_chunks::<4>();
        assert!(chunks.iter().all(|px| *px == BG));
        let mut tiny = SwitcherCanvas::new(0, 0);
        tiny.render(&[1], Some(1));
        assert!(tiny.pixels().is_empty());
    }

    fn pixel_at(canvas: &SwitcherCanvas, x: i32, y: i32) -> [u8; 4] {
        let at = (y as usize * 1280 + x as usize) * BYTES_PER_PIXEL;
        canvas.pixels()[at..at + BYTES_PER_PIXEL]
            .try_into()
            .unwrap()
    }

    fn strip_pixel(pixels: &[u8], width: i32, x: i32, y: i32) -> [u8; 4] {
        let at = (y as usize * width as usize + x as usize) * BYTES_PER_PIXEL;
        pixels[at..at + BYTES_PER_PIXEL].try_into().unwrap()
    }

    #[test]
    fn panel_paints_clock_and_honest_tiles() {
        use crate::tiles::{ServiceKind, Tile};
        let tiles = [
            Tile {
                kind: ServiceKind::Network,
                state: TileState::Ready,
                level: None,
            },
            Tile {
                kind: ServiceKind::Power,
                state: TileState::Ready,
                level: Some(50),
            },
            Tile {
                kind: ServiceKind::Sound,
                state: TileState::Error,
                level: None,
            },
        ];
        let width = 1280;
        let mut pixels = vec![0u8; width as usize * 32 * BYTES_PER_PIXEL];
        paint_panel(&mut pixels, width, 32, "12:34", &tiles);
        // Clock "12:34" centered: digit 1 lights, colon lights.
        let cx = (width - (5 * GLYPH_ADVANCE - (FONT_SCALE - 1))) / 2;
        let cy = (32 - 5 * FONT_SCALE) / 2;
        assert_eq!(strip_pixel(&pixels, width, cx + 4, cy + 1), ACCENT);
        assert_eq!(
            strip_pixel(&pixels, width, cx + 2 * GLYPH_ADVANCE + 4, cy + 4),
            ACCENT
        );
        // Backdrop between furniture stays backdrop.
        assert_eq!(strip_pixel(&pixels, width, 500, 16), BG);
        // Network dot (rightmost) filled: ready.
        assert_eq!(strip_pixel(&pixels, width, 1256 + 7, 9 + 7), ACCENT);
        // Power dot half-filled from the bottom by its level.
        assert_eq!(strip_pixel(&pixels, width, 1234 + 7, 9 + 12), ACCENT);
        assert_eq!(strip_pixel(&pixels, width, 1234 + 7, 9 + 2), BG);
        // Sound dot brick: error.
        assert_eq!(strip_pixel(&pixels, width, 1212 + 7, 9 + 7), WARN);
    }

    #[test]
    fn banner_rows_highlight_critical_and_expand() {
        // One normal row, one expanded critical row.
        let rows = [(Urgency::Normal, false), (Urgency::Critical, true)];
        let height = banner_strip_height(&[false, true]);
        assert_eq!(
            height,
            12 * 2 + BANNER_ROW_H + BANNER_GAP + BANNER_ROW_EXPANDED_H
        );
        let mut canvas = BannerCanvas::new(BANNER_STRIP_W, height);
        canvas.render(&rows);
        // First row plain fill.
        let at = |x: i32, y: i32| {
            let at = (y as usize * BANNER_STRIP_W as usize + x as usize) * BYTES_PER_PIXEL;
            <[u8; 4]>::try_from(&canvas.pixels()[at..at + BYTES_PER_PIXEL]).unwrap()
        };
        assert_eq!(at(20, 12 + 10), BLOCK);
        // Second row highlighted with an accent border.
        let y1 = 12 + BANNER_ROW_H + BANNER_GAP;
        assert_eq!(at(20, y1 + 10), BLOCK_SELECTED);
        assert_eq!(at(12 - 2, y1 - 2), ACCENT);
        // Padding stays backdrop.
        assert_eq!(at(5, 5), BG);
        // Empty draws nothing but backdrop.
        let mut empty = BannerCanvas::new(BANNER_STRIP_W, height);
        empty.render(&[]);
        let (chunks, _) = empty.pixels().as_chunks::<4>();
        assert!(chunks.iter().all(|px| *px == BG));
    }

    #[test]
    fn panel_hollow_dots_when_disconnected() {
        use crate::tiles::{ServiceKind, Tile};
        let tiles = [Tile {
            kind: ServiceKind::Network,
            state: TileState::Disconnected,
            level: None,
        }];
        let width = 1280;
        let mut pixels = vec![0u8; width as usize * 32 * BYTES_PER_PIXEL];
        paint_panel(&mut pixels, width, 32, "", &tiles);
        // Hollow: accent frame, backdrop interior.
        assert_eq!(strip_pixel(&pixels, width, 1256, 9), ACCENT);
        assert_eq!(strip_pixel(&pixels, width, 1256 + 7, 9 + 7), BG);
    }
}
