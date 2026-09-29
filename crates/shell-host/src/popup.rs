//! Panel popups: calendar and service menus (settings-compat M2).
//!
//! All state and paint here is wire-free: pointer coordinates enter
//! through [`PopupState::press`], and the panel surface grows to fit
//! the popup band. Hit areas mirror `overview::paint_panel` exactly —
//! the clock rect centers like the painted text, tile rects sit on the
//! painted dots — so presses land where the pixels are.

use crate::overview::{
    blit_glyph, glyph_index, put_pixel, ACCENT, BG, BYTES_PER_PIXEL, DOT_GAP, DOT_MARGIN, DOT_SIZE,
    FONT_SCALE, GLYPH_ADVANCE,
};
use crate::tiles::{Tile, TileState};

/// Popup band height below the strip; the panel surface grows by this
/// much while a popup is open (exclusive zone stays at strip height).
pub const POPUP_HEIGHT: i32 = 216;

/// Integer rectangle for hit-testing painted regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge, surface coordinates.
    pub x: i32,
    /// Top edge, surface coordinates.
    pub y: i32,
    /// Width in pixels.
    pub w: i32,
    /// Height in pixels.
    pub h: i32,
}

impl Rect {
    /// True when the surface point falls inside (right/bottom exclusive).
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Strip hit areas: clock plus the three service tiles in strip order
/// (network, power, sound), matching the painted dots right to left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelLayout {
    /// Surface width the layout was computed for.
    pub width: i32,
    /// Strip height (popup band excluded).
    pub strip_h: i32,
    /// Clock text area (padded for touch).
    pub clock: Rect,
    /// Tile areas in strip order.
    pub tiles: [Rect; 3],
}

impl PanelLayout {
    /// Tile index under the point, if any.
    pub fn tile_at(&self, x: i32, y: i32) -> Option<usize> {
        self.tiles.iter().position(|rect| rect.contains(x, y))
    }
}

/// Hit areas for a strip of `width` carrying `clock`, mirroring
/// `overview::paint_panel`: centered clock text, dots packed from the
/// right margin.
pub fn panel_layout(width: i32, strip_h: i32, clock: &str) -> PanelLayout {
    let text_w = clock.chars().count() as i32 * GLYPH_ADVANCE - (FONT_SCALE - 1);
    let cx = (width - text_w) / 2;
    let cy = (strip_h - 5 * FONT_SCALE) / 2;
    let clock = Rect {
        x: cx - 6,
        y: cy - 4,
        w: text_w + 12,
        h: 5 * FONT_SCALE + 8,
    };
    let mut dx = width - DOT_MARGIN - DOT_SIZE;
    let mut tiles = [Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    }; 3];
    for tile in tiles.iter_mut() {
        *tile = Rect {
            x: dx - 4,
            y: 0,
            w: DOT_SIZE + 8,
            h: strip_h,
        };
        dx -= DOT_SIZE + DOT_GAP;
    }
    PanelLayout {
        width,
        strip_h,
        clock,
        tiles,
    }
}

/// Which popup is open. Calendar belongs to the clock; each menu
/// belongs to a tile by strip index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupBody {
    /// Month calendar under the clock.
    Calendar,
    /// Service menu under tile `usize` (strip order).
    Menu(usize),
    /// Indicator menu for hosted item `usize` (registration order).
    IndicatorMenu(usize),
}

/// Open-popup state. Pressing the owning region toggles, pressing the
/// other region switches, pressing anywhere else dismisses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PopupState {
    open: Option<PopupBody>,
}

impl PopupState {
    /// Currently open popup, if any.
    pub fn body(&self) -> Option<PopupBody> {
        self.open
    }

    /// True while any popup is open.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Close whatever is open.
    pub fn dismiss(&mut self) {
        self.open = None;
    }

    /// Open `body` directly. Indicator cells live outside the strip
    /// layout `press` understands, so the panel opens their menus
    /// without a layout hit.
    pub fn open(&mut self, body: PopupBody) {
        self.open = Some(body);
    }

    /// Left press at surface coordinates against the strip layout.
    /// `popup_box` (below) describes the open popup's band area:
    /// presses inside it change nothing, presses outside the strip
    /// and the box dismiss.
    pub fn press(&mut self, layout: &PanelLayout, popup: Option<Rect>, x: i32, y: i32) {
        if layout.clock.contains(x, y) {
            self.toggle(PopupBody::Calendar);
        } else if let Some(index) = layout.tile_at(x, y) {
            self.toggle(PopupBody::Menu(index));
        } else if popup.is_some_and(|rect| rect.contains(x, y)) {
            // Inside the open popup: keep it open.
        } else {
            self.dismiss();
        }
    }

    fn toggle(&mut self, body: PopupBody) {
        if self.open == Some(body) {
            self.open = None;
        } else {
            self.open = Some(body);
        }
    }
}

/// Popup band box for an open popup: calendar centers on the clock,
/// menus right-align under the tiles. Clamped inside the surface with
/// an 8px margin.
pub fn popup_box(layout: &PanelLayout, body: PopupBody) -> Rect {
    const BOX_W: i32 = 300;
    const BOX_H: i32 = POPUP_HEIGHT - 16;
    let y = layout.strip_h + 8;
    let x = match body {
        PopupBody::Calendar => {
            let center = layout.clock.x + layout.clock.w / 2;
            (center - BOX_W / 2).clamp(8, (layout.width - 8 - BOX_W).max(8))
        }
        PopupBody::Menu(_) | PopupBody::IndicatorMenu(_) => (layout.width - 8 - BOX_W).max(8),
    };
    Rect {
        x,
        y,
        w: BOX_W.min(layout.width - 16).max(0),
        h: BOX_H,
    }
}

/// One calendar cell: day number, whether it falls in the shown
/// month, and whether it is today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalCell {
    /// Day of month (this or the abutting month).
    pub day: u8,
    /// False for leading/trailing filler days.
    pub in_month: bool,
    /// True for today's cell.
    pub today: bool,
}

/// Monday-first month grid (42 cells) for `year`/`month`, with `today`
/// highlighted when it falls inside. Pure date math, no clock reads.
pub fn month_cells(year: i32, month: u8, today: Option<u8>) -> [CalCell; 42] {
    let first =
        jiff::civil::Date::new(year as i16, month as i8, 1).expect("caller passes a valid month");
    let leading = match first.weekday() {
        jiff::civil::Weekday::Monday => 0,
        jiff::civil::Weekday::Tuesday => 1,
        jiff::civil::Weekday::Wednesday => 2,
        jiff::civil::Weekday::Thursday => 3,
        jiff::civil::Weekday::Friday => 4,
        jiff::civil::Weekday::Saturday => 5,
        jiff::civil::Weekday::Sunday => 6,
    };
    let days_in_month = first.days_in_month() as usize;
    let prev_days = if month == 1 {
        jiff::civil::Date::new((year - 1) as i16, 12, 1)
            .expect("year-1 valid")
            .days_in_month() as usize
    } else {
        jiff::civil::Date::new(year as i16, (month - 1) as i8, 1)
            .expect("month-1 valid")
            .days_in_month() as usize
    };
    let mut cells = [CalCell {
        day: 0,
        in_month: false,
        today: false,
    }; 42];
    for (index, cell) in cells.iter_mut().enumerate() {
        if index < leading {
            cell.day = (prev_days - leading + 1 + index) as u8;
        } else if index < leading + days_in_month {
            let day = (index - leading + 1) as u8;
            cell.day = day;
            cell.in_month = true;
            cell.today = today == Some(day);
        } else {
            cell.day = (index - leading - days_in_month + 1) as u8;
        }
    }
    cells
}

/// Lowercase month names (the micro-glyphs fold case anyway).
const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// Blit one lowercase text line; unknown glyphs become gaps.
pub(crate) fn blit_text(
    pixels: &mut [u8],
    stride: usize,
    x: i32,
    y: i32,
    text: &str,
    color: [u8; 4],
) {
    let mut cx = x;
    for ch in text.chars() {
        if ch == ' ' {
            cx += GLYPH_ADVANCE;
            continue;
        }
        if let Some(glyph) = glyph_index(ch) {
            blit_glyph(pixels, stride, cx, y, glyph, color);
        }
        cx += GLYPH_ADVANCE;
    }
}

/// Popup band and box face.
const FACE: [u8; 4] = [0x2a, 0x26, 0x26, 0xff];

/// Box frame: filled face with a one-pixel accent border.
fn paint_box(pixels: &mut [u8], stride: usize, rect: &Rect) {
    for y in rect.y..rect.y + rect.h {
        for x in rect.x..rect.x + rect.w {
            let edge =
                x == rect.x || y == rect.y || x == rect.x + rect.w - 1 || y == rect.y + rect.h - 1;
            put_pixel(pixels, stride, x, y, if edge { ACCENT } else { FACE });
        }
    }
}

/// Paint the open popup band below the strip. `today` is the date the
/// calendar highlights; `tiles` feeds the menu rows. No-ops on
/// degenerate sizes.
pub fn paint_popup(
    pixels: &mut [u8],
    width: i32,
    layout: &PanelLayout,
    body: PopupBody,
    today: jiff::civil::Date,
    tiles: &[Tile; 3],
    indicators: &[crate::watcher::IndicatorItem],
) {
    let rect = popup_box(layout, body);
    if rect.w <= 0 || rect.h <= 0 {
        return;
    }
    let stride = width as usize * BYTES_PER_PIXEL;
    // Own the whole band below the strip so the box floats on a clean
    // face instead of stretched strip pixels.
    for y in layout.strip_h..layout.strip_h + POPUP_HEIGHT {
        for x in 0..width {
            put_pixel(pixels, stride, x, y, FACE);
        }
    }
    paint_box(pixels, stride, &rect);
    match body {
        PopupBody::Calendar => paint_calendar(pixels, stride, &rect, today),
        PopupBody::Menu(index) => {
            let tile = tiles.get(index).copied().unwrap_or(Tile {
                kind: crate::tiles::ServiceKind::Network,
                state: TileState::Disconnected,
                level: None,
            });
            paint_menu(pixels, stride, &rect, tile);
        }
        PopupBody::IndicatorMenu(index) => {
            if let Some(item) = indicators.get(index) {
                crate::watcher::paint_indicator_menu(pixels, stride, &rect, item);
            }
        }
    }
}

/// Month title, weekday header, and day grid with today inverted.
fn paint_calendar(pixels: &mut [u8], stride: usize, rect: &Rect, today: jiff::civil::Date) {
    const CELL_W: i32 = 36;
    const CELL_H: i32 = 22;
    let title = format!("{} {}", MONTHS[today.month() as usize - 1], today.year());
    blit_text(pixels, stride, rect.x + 12, rect.y + 10, &title, ACCENT);
    let grid_y = rect.y + 10 + 5 * FONT_SCALE + 12;
    for (index, day) in ["mo", "tu", "we", "th", "fr", "sa", "su"]
        .iter()
        .enumerate()
    {
        blit_text(
            pixels,
            stride,
            rect.x + 12 + index as i32 * CELL_W,
            grid_y,
            day,
            ACCENT,
        );
    }
    let cells = month_cells(
        today.year() as i32,
        today.month() as u8,
        Some(today.day() as u8),
    );
    for (index, cell) in cells.iter().enumerate() {
        let col = index as i32 % 7;
        let row = index as i32 / 7;
        let x = rect.x + 12 + col * CELL_W;
        let y = grid_y + 5 * FONT_SCALE + 8 + row * CELL_H;
        if cell.today {
            for dy in 0..CELL_H - 4 {
                for dx in 0..CELL_W - 8 {
                    put_pixel(pixels, stride, x + dx - 2, y + dy - 2, ACCENT);
                }
            }
        }
        let digits = format!("{:>2}", cell.day);
        blit_text(
            pixels,
            stride,
            x,
            y,
            &digits,
            if cell.today {
                BG
            } else if cell.in_month {
                ACCENT
            } else {
                WARN_DIM
            },
        );
    }
}

/// Dim brick for out-of-month filler days.
const WARN_DIM: [u8; 4] = [0x60, 0x38, 0x30, 0xff];

/// Service name, state word, and optional level row.
fn paint_menu(pixels: &mut [u8], stride: usize, rect: &Rect, tile: Tile) {
    use crate::tiles::ServiceKind;
    let name = match tile.kind {
        ServiceKind::Clock => "clock",
        ServiceKind::Network => "network",
        ServiceKind::Power => "power",
        ServiceKind::Sound => "sound",
    };
    let state = match tile.state {
        TileState::Ready => "ready",
        TileState::Disconnected => "no link",
        TileState::Error => "error",
        TileState::Loading => "waiting",
    };
    blit_text(pixels, stride, rect.x + 12, rect.y + 10, name, ACCENT);
    blit_text(
        pixels,
        stride,
        rect.x + 12,
        rect.y + 10 + 5 * FONT_SCALE + 10,
        state,
        ACCENT,
    );
    if let Some(level) = tile.level {
        blit_text(
            pixels,
            stride,
            rect.x + 12,
            rect.y + 10 + 2 * (5 * FONT_SCALE + 10),
            &format!("{level} pct"),
            ACCENT,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::ServiceKind;

    fn layout() -> PanelLayout {
        panel_layout(1280, 32, "12:34")
    }

    #[test]
    fn clock_rect_centers_on_painted_text() {
        let layout = layout();
        // "12:34" is 5 glyphs: 5*12-2 = 58px wide, centered in 1280.
        assert_eq!(layout.clock.w, 58 + 12);
        assert_eq!(layout.clock.x, (1280 - 58) / 2 - 6);
        assert!(layout.clock.contains(640, 16));
        assert!(!layout.clock.contains(0, 16));
    }

    #[test]
    fn tile_rects_sit_on_the_right_edge_in_order() {
        let layout = layout();
        // First tile (network) is rightmost.
        assert!(layout.tiles[0].x > layout.tiles[1].x);
        assert!(layout.tiles[1].x > layout.tiles[2].x);
        let right = 1280 - DOT_MARGIN;
        assert!(layout.tiles[0].x + layout.tiles[0].w >= right - 8);
        assert_eq!(layout.tile_at(right - DOT_SIZE, 16), Some(0));
        assert_eq!(layout.tile_at(640, 16), None);
    }

    #[test]
    fn press_toggles_switches_and_dismisses() {
        let layout = layout();
        let mut popup = PopupState::default();
        assert!(!popup.is_open());
        // Clock press opens the calendar; pressing again closes it.
        popup.press(&layout, None, 640, 16);
        assert_eq!(popup.body(), Some(PopupBody::Calendar));
        popup.press(&layout, None, 640, 16);
        assert!(!popup.is_open());
        // Tile press opens that tile's menu; clock press switches.
        let tile_x = layout.tiles[1].x + 2;
        popup.press(&layout, None, tile_x, 16);
        assert_eq!(popup.body(), Some(PopupBody::Menu(1)));
        popup.press(&layout, None, 640, 16);
        assert_eq!(popup.body(), Some(PopupBody::Calendar));
        // Presses inside the open box keep it; outside dismisses.
        let rect = popup_box(&layout, PopupBody::Calendar);
        popup.press(&layout, Some(rect), rect.x + 4, rect.y + 4);
        assert!(popup.is_open());
        popup.press(&layout, Some(rect), 4, 100);
        assert!(!popup.is_open());
    }

    #[test]
    fn escape_and_repress_paths_dismiss() {
        let mut popup = PopupState::default();
        popup.press(&layout(), None, 640, 16);
        assert!(popup.is_open());
        popup.dismiss();
        assert!(!popup.is_open());
    }

    #[test]
    fn september_2026_grid_starts_tuesday_with_30_days() {
        // September 1st 2026 is a Tuesday: one Monday filler, then 1..=30.
        let cells = month_cells(2026, 9, None);
        assert_eq!(cells.len(), 42);
        assert!(!cells[0].in_month);
        assert_eq!(cells[0].day, 31);
        assert!(cells[1].in_month);
        assert_eq!(cells[1].day, 1);
        assert_eq!(cells[30].day, 30);
        assert!(cells[30].in_month);
        assert!(!cells[31].in_month);
        assert_eq!(cells[31].day, 1);
    }

    #[test]
    fn today_flag_lands_on_the_right_cell() {
        let cells = month_cells(2026, 9, Some(15));
        let flagged: Vec<u8> = cells
            .iter()
            .filter(|cell| cell.today)
            .map(|cell| cell.day)
            .collect();
        assert_eq!(flagged, vec![15]);
        let none = month_cells(2026, 9, Some(31));
        assert!(none.iter().all(|cell| !cell.today));
    }

    #[test]
    fn popup_box_stays_inside_narrow_surfaces() {
        let small = panel_layout(200, 32, "12:34");
        let rect = popup_box(&small, PopupBody::Calendar);
        assert!(rect.x >= 0);
        assert!(rect.x + rect.w <= 200);
    }

    fn sample_tiles() -> [Tile; 3] {
        [
            Tile {
                kind: ServiceKind::Network,
                state: crate::tiles::TileState::Ready,
                level: None,
            },
            Tile {
                kind: ServiceKind::Power,
                state: crate::tiles::TileState::Ready,
                level: Some(73),
            },
            Tile {
                kind: ServiceKind::Sound,
                state: crate::tiles::TileState::Disconnected,
                level: None,
            },
        ]
    }

    #[test]
    fn popup_paint_differs_from_strip_and_covers_expected_area() {
        let layout = layout();
        let today = jiff::civil::Date::new(2026, 9, 15).unwrap();
        let mut plain = vec![0u8; 1280 * (32 + POPUP_HEIGHT) as usize * BYTES_PER_PIXEL];
        crate::overview::paint_panel(
            &mut plain,
            1280,
            32 + POPUP_HEIGHT,
            32,
            "12:34",
            &sample_tiles(),
            0,
        );
        let mut with_popup = plain.clone();
        paint_popup(
            &mut with_popup,
            1280,
            &layout,
            PopupBody::Calendar,
            today,
            &sample_tiles(),
            &[],
        );
        // Popup paints below the strip and leaves the strip itself alone.
        assert_eq!(
            plain[..1280 * 32 * BYTES_PER_PIXEL],
            with_popup[..1280 * 32 * BYTES_PER_PIXEL]
        );
        assert_ne!(
            plain[1280 * 32 * BYTES_PER_PIXEL..],
            with_popup[1280 * 32 * BYTES_PER_PIXEL..]
        );
        let mut menu = plain.clone();
        paint_popup(
            &mut menu,
            1280,
            &layout,
            PopupBody::Menu(1),
            today,
            &sample_tiles(),
            &[],
        );
        assert_ne!(
            menu[1280 * 32 * BYTES_PER_PIXEL..],
            with_popup[1280 * 32 * BYTES_PER_PIXEL..]
        );
    }
}
