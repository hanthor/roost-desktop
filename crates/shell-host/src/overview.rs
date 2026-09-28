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
}

/// Paint the panel strip: backdrop plus a lighter Activities corner.
pub fn paint_panel(pixels: &mut [u8], width: i32, height: i32) {
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
        assert!(canvas.pixels().is_empty());
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
}
