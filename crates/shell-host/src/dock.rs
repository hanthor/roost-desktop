//! Bottom dock: pinned launches plus running windows (settings-compat M3).
//!
//! Like the panel popups this is wire-free at its core: [`dock_items`]
//! composes favorites, desktop entries, and model windows into items,
//! [`dock_press`] maps a button press to one [`DockAction`], and
//! [`paint_dock`] renders the strip. The panel owns the layer surface,
//! pointer routing, and running the actions (launch/pin locally,
//! switch/close through the control client).

use crate::apps::{entry_from_file, AppProvider};
use crate::icons::Artwork;
use crate::model::WindowEntry;
use crate::overview::{
    blit_glyph, glyph_index, put_pixel, ACCENT, BG, BYTES_PER_PIXEL, FONT_SCALE, GLYPH_ADVANCE,
};
use std::path::{Path, PathBuf};

/// Dock strip height; the surface is bottom-anchored and floats over
/// windows (overlay layer, no exclusive zone), like the switcher.
pub const DOCK_H: i32 = 56;
/// Icon slot width; icons center in a full-width bottom strip.
pub const DOCK_SLOT: i32 = 56;
/// Favorites prefix marking a folder stack (`stack:<dir>`): a
/// macOS-like grid of the directory's launchables above the dock.
pub const STACK_PREFIX: &str = "stack:";
/// Grid band height above the icon strip while a stack is open.
pub const STACK_GRID_H: i32 = 232;
/// Grid cell pitch (icon plus label).
pub const STACK_CELL: i32 = 64;
/// Grid rows that fit the band (padding included).
pub const STACK_GRID_ROWS: i32 = 3;

/// Left/middle/right mouse buttons (evdev).
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_MIDDLE: u32 = 0x112;
pub const BTN_RIGHT: u32 = 0x111;

/// One dock icon: a pinned app, a running app group, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockItem {
    /// Desktop-entry id (`org.gnome.Terminal.desktop`), or the
    /// `stack:<dir>` favorite id for a folder stack.
    pub app_id: String,
    /// Display name for paint and fallback matching.
    pub name: String,
    /// Running window ids in model order.
    pub windows: Vec<u64>,
    /// True when any window holds keyboard focus.
    pub active: bool,
    /// True when pinned in favorites.
    pub pinned: bool,
    /// Pinned folder backing this item, if it is a stack.
    pub stack_dir: Option<PathBuf>,
}

/// One stack cell: a launchable desktop file or a plain file
/// opened through the default handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackEntry {
    /// Display name (entry name, or the file stem).
    pub name: String,
    /// Backing file path.
    pub path: PathBuf,
    /// True for `.desktop` files launched directly; false for
    /// plain files opened with `xdg-open`.
    pub app: bool,
}

/// Read a stack directory into sorted cells: launchable desktop
/// files first by the same filter discovery uses, then plain
/// files by stem. Subdirectories are skipped (nesting stacks is
/// later work), and an unreadable directory reads as empty rather
/// than failing the paint path.
pub fn read_stack(dir: &Path) -> Vec<StackEntry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for child in read.flatten() {
        let Ok(kind) = child.file_type() else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let path = child.path();
        if path.extension().is_some_and(|ext| ext == "desktop") {
            if let Some(entry) = entry_from_file(&path) {
                entries.push(StackEntry {
                    name: entry.name,
                    path,
                    app: true,
                });
            }
        } else {
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.is_empty())
                .unwrap_or("file")
                .to_owned();
            entries.push(StackEntry {
                name,
                path,
                app: false,
            });
        }
    }
    entries.sort_by(|a, b| {
        b.app
            .cmp(&a.app)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

impl DockItem {
    /// True while at least one window is running.
    pub fn running(&self) -> bool {
        !self.windows.is_empty()
    }

    /// Window a left press switches to: the focused one, else first.
    pub fn switch_target(&self, focused: &[u64]) -> Option<u64> {
        self.windows
            .iter()
            .find(|id| focused.contains(id))
            .or(self.windows.first())
            .copied()
    }
}

/// Strip a `.desktop` suffix for app-id comparison: clients report
/// `org.gnome.Terminal` while entries are filed as
/// `org.gnome.Terminal.desktop`.
fn normalize_app_id(id: &str) -> &str {
    id.strip_suffix(".desktop").unwrap_or(id)
}

/// True when a window belongs to an app: normalized app-id match
/// first, then an exact (case-insensitive) title-to-name match for
/// clients that report no usable id.
fn window_belongs(window: &WindowEntry, app_id: &str, name: &str) -> bool {
    if let Some(reported) = window.app_id.as_deref() {
        if normalize_app_id(reported) == normalize_app_id(app_id) {
            return true;
        }
    }
    window.title.eq_ignore_ascii_case(name)
}

/// Compose dock items: pinned favorites in favorites order, then
/// running-but-unpinned app groups in window order.
pub fn dock_items(
    favorites: &[String],
    apps: &AppProvider,
    windows: &[WindowEntry],
) -> Vec<DockItem> {
    let mut items = Vec::new();
    let mut claimed: Vec<u64> = Vec::new();
    for fav_id in favorites {
        // Folder stacks pin as `stack:<dir>` and never claim
        // windows: their children launch through xdg-open and are
        // not tracked launches.
        if let Some(dir) = fav_id.strip_prefix(STACK_PREFIX) {
            let dir = PathBuf::from(dir);
            if dir.is_dir() {
                let name = dir
                    .file_name()
                    .and_then(|stem| stem.to_str())
                    .filter(|stem| !stem.is_empty())
                    .unwrap_or(dir.as_os_str().to_str().unwrap_or("stack"))
                    .to_owned();
                items.push(DockItem {
                    app_id: fav_id.clone(),
                    name,
                    windows: Vec::new(),
                    active: false,
                    pinned: true,
                    stack_dir: Some(dir),
                });
                continue;
            }
        }
        let name = apps
            .entry(fav_id)
            .map(|entry| entry.name.clone())
            .unwrap_or_else(|| {
                fav_id
                    .rsplit('.')
                    .next()
                    .unwrap_or(fav_id)
                    .trim_end_matches(".desktop")
                    .to_owned()
            });
        let mut item_windows = Vec::new();
        let mut active = false;
        for window in windows {
            if claimed.contains(&window.id) {
                continue;
            }
            if window_belongs(window, fav_id, &name) {
                claimed.push(window.id);
                item_windows.push(window.id);
                active |= window.active;
            }
        }
        items.push(DockItem {
            app_id: fav_id.clone(),
            name,
            windows: item_windows,
            active,
            pinned: true,
            stack_dir: None,
        });
    }
    // Running windows no favorite claimed, grouped by reported app id
    // (or title when the client reports none).
    let mut groups: Vec<(String, String, Vec<u64>, bool)> = Vec::new();
    for window in windows {
        if claimed.contains(&window.id) {
            continue;
        }
        let key = window
            .app_id
            .clone()
            .unwrap_or_else(|| window.title.clone());
        if let Some(group) = groups.iter_mut().find(|(id, name, _, _)| {
            normalize_app_id(id) == normalize_app_id(&key) || *name == window.title
        }) {
            group.2.push(window.id);
            group.3 |= window.active;
        } else {
            let name = window
                .app_id
                .as_deref()
                .and_then(|id| apps.entry(id))
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| window.title.clone());
            groups.push((key, name, vec![window.id], window.active));
        }
    }
    for (app_id, name, item_windows, active) in groups {
        // Resolve to the desktop-entry id when one is known: pins
        // and launches downstream need the exact filed id, not the
        // client's suffix-less report.
        let app_id = apps
            .apps()
            .iter()
            .find(|entry| normalize_app_id(&entry.app_id) == normalize_app_id(&app_id))
            .map(|entry| entry.app_id.clone())
            .unwrap_or(app_id);
        items.push(DockItem {
            app_id,
            name,
            windows: item_windows,
            active,
            pinned: false,
            stack_dir: None,
        });
    }
    items
}

/// One press outcome. Launch and pin run locally on the host;
/// switch and close need the control client and travel through the
/// run loop's pending queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockAction {
    /// Launch a pinned app that is not running.
    Launch(String),
    /// Focus and raise this window.
    Switch(u64),
    /// Politely close this window.
    Close(u64),
    /// Pin (unpinned) or unpin (pinned) this app id.
    TogglePin(String),
    /// Open (or re-target) the folder-stack grid for item `index`.
    OpenStack(usize),
}

/// Map a press on item `index` to its action. Left launches,
/// switches, or opens a stack grid; middle closes a running window
/// (browser-tab convention); right toggles the pin. Middle on a
/// stopped app or a stack does nothing.
pub fn dock_press(
    items: &[DockItem],
    focused: &[u64],
    index: usize,
    button: u32,
) -> Option<DockAction> {
    let item = items.get(index)?;
    if item.stack_dir.is_some() {
        return match button {
            BTN_LEFT => Some(DockAction::OpenStack(index)),
            BTN_RIGHT => Some(DockAction::TogglePin(item.app_id.clone())),
            _ => None,
        };
    }
    match button {
        BTN_LEFT => {
            if item.running() {
                item.switch_target(focused).map(DockAction::Switch)
            } else {
                Some(DockAction::Launch(item.app_id.clone()))
            }
        }
        BTN_MIDDLE => item.switch_target(focused).map(DockAction::Close),
        BTN_RIGHT => Some(DockAction::TogglePin(item.app_id.clone())),
        _ => None,
    }
}

/// Slot rect for item `index` in a `width`-wide strip.
pub fn dock_slot(width: i32, index: usize, count: usize) -> (i32, i32, i32, i32) {
    dock_slot_at(width, index, count, 0)
}

/// Slot rect with the icon strip shifted down by `y_origin` (the
/// stack grid band occupies the top of a grown dock surface).
pub fn dock_slot_at(width: i32, index: usize, count: usize, y_origin: i32) -> (i32, i32, i32, i32) {
    let total = count as i32 * DOCK_SLOT;
    let x0 = (width - total) / 2 + index as i32 * DOCK_SLOT;
    (x0, y_origin, DOCK_SLOT, DOCK_H)
}

/// Grid columns that fit a `width`-wide band (8px margins).
pub fn stack_cols(width: i32) -> i32 {
    ((width - 16) / STACK_CELL).max(1)
}

/// Cells the band can show: full rows only, capped by
/// [`STACK_GRID_ROWS`].
pub fn stack_shown(width: i32, count: usize) -> usize {
    ((stack_cols(width) * STACK_GRID_ROWS) as usize).min(count)
}

/// Cell index at grid-band coordinates (`y` below the band is not a
/// cell). Cells lay out left-aligned from an 8px margin, row-major.
pub fn stack_cell_at(width: i32, x: i32, y: i32, shown: usize) -> Option<usize> {
    if !(0..STACK_GRID_H).contains(&y) {
        return None;
    }
    let col = (x - 8) / STACK_CELL;
    let row = (y - 8) / STACK_CELL;
    if col < 0 || row < 0 || col >= stack_cols(width) {
        return None;
    }
    let index = (row * stack_cols(width) + col) as usize;
    (index < shown).then_some(index)
}

/// Top-left corner of cell `index` in the grid band.
pub fn stack_cell_origin(width: i32, index: usize) -> (i32, i32) {
    let cols = stack_cols(width);
    (
        8 + (index as i32 % cols) * STACK_CELL,
        8 + (index as i32 / cols) * STACK_CELL,
    )
}

/// Edge length of dock artwork in pixels: the icon-square
/// interior (`DOCK_SLOT` minus the frame padding on both sides).
pub const DOCK_ICON_PX: u32 = 40;

/// Paint the dock strip: icon squares with the app's initial, an
/// accent frame plus underline while running.
pub fn paint_dock(pixels: &mut [u8], width: i32, height: i32, items: &[DockItem]) {
    paint_dock_stacked(pixels, width, height, items, None);
}

/// Blit square `art` centered on (`cx`, `cy`) into a `w` x `h` box,
/// nearest-sampling. Out-of-buffer pixels are clipped.
pub fn blit_artwork(
    pixels: &mut [u8],
    stride: usize,
    cx: i32,
    cy: i32,
    w: i32,
    h: i32,
    art: &Artwork,
) {
    if w <= 0 || h <= 0 || art.size == 0 {
        return;
    }
    let size = art.size as i32;
    for dy in 0..h {
        for dx in 0..w {
            let sx = (dx * size / w).clamp(0, size - 1) as usize;
            let sy = (dy * size / h).clamp(0, size - 1) as usize;
            let from = (sy * art.size as usize + sx) * BYTES_PER_PIXEL;
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 {
                continue;
            }
            let into = (y as usize * stride) + (x as usize * BYTES_PER_PIXEL);
            if let (Some(src), Some(dst)) = (
                art.argb.get(from..from + BYTES_PER_PIXEL),
                pixels.get_mut(into..into + BYTES_PER_PIXEL),
            ) {
                dst.copy_from_slice(src);
            }
        }
    }
}

/// Paint the dock strip with an optional open folder-stack grid in
/// the band above the icons. A stack item paints with a double
/// frame so the grid's source stays visible. Items without artwork
/// fall back to the app's initial.
pub fn paint_dock_stacked(
    pixels: &mut [u8],
    width: i32,
    height: i32,
    items: &[DockItem],
    grid: Option<&[StackEntry]>,
) {
    let blank = vec![None; items.len()];
    paint_dock_stacked_with_icons(pixels, width, height, items, grid, &blank);
}

/// Paint the dock strip with per-item theme artwork: `icons[i]`
/// replaces item `i`'s initial when `Some`, resolved by the caller
/// through the host icon cache. `None` keeps the initial fallback.
pub fn paint_dock_stacked_with_icons(
    pixels: &mut [u8],
    width: i32,
    height: i32,
    items: &[DockItem],
    grid: Option<&[StackEntry]>,
    icons: &[Option<Artwork>],
) {
    const DIM: [u8; 4] = [0x4a, 0x44, 0x44, 0xff];
    let stride = width as usize * BYTES_PER_PIXEL;
    for (i, byte) in pixels.iter_mut().enumerate() {
        *byte = BG[i % BYTES_PER_PIXEL];
    }
    let y_origin = if grid.is_some() { STACK_GRID_H } else { 0 };
    if let Some(entries) = grid {
        paint_stack_grid(pixels, stride, width, entries);
    }
    for (index, item) in items.iter().enumerate() {
        let (x0, _, _, _) = dock_slot_at(width, index, items.len(), y_origin);
        let pad = 8;
        let frame = if item.running() { ACCENT } else { DIM };
        // Icon square outline (double frame for stacks).
        let frames = if item.stack_dir.is_some() {
            [pad - 3, pad]
        } else {
            [pad, pad]
        };
        for frame_pad in frames {
            for dx in 0..DOCK_SLOT - 2 * frame_pad {
                for dy in 0..DOCK_SLOT - 2 * frame_pad {
                    let edge = dx == 0
                        || dy == 0
                        || dx == DOCK_SLOT - 2 * frame_pad - 1
                        || dy == DOCK_SLOT - 2 * frame_pad - 1;
                    if edge {
                        put_pixel(
                            pixels,
                            stride,
                            x0 + frame_pad + dx,
                            y_origin + frame_pad + dy,
                            frame,
                        );
                    }
                }
            }
        }
        // Theme artwork replaces the initial; without it the
        // app's initial paints centered as before.
        match icons.get(index).and_then(|slot| slot.as_ref()) {
            Some(art) => {
                let side = (art.size as i32).min(DOCK_SLOT - 2 * pad).max(1);
                let ox = x0 + (DOCK_SLOT - side) / 2;
                let oy = y_origin + (DOCK_H - side) / 2;
                blit_artwork(pixels, stride, ox, oy, side, side, art);
            }
            None => {
                if let Some(initial) = item.name.chars().next() {
                    if let Some(glyph) = glyph_index(initial) {
                        let gx = x0 + (DOCK_SLOT - 4 * FONT_SCALE) / 2;
                        let gy = y_origin + (DOCK_H - 5 * FONT_SCALE) / 2 - 2;
                        blit_glyph(pixels, stride, gx, gy, glyph, ACCENT);
                    }
                }
            }
        }
        // Running underline.
        if item.running() {
            let underline = if item.active { ACCENT } else { DIM };
            for dx in pad..DOCK_SLOT - pad {
                put_pixel(pixels, stride, x0 + dx, height - 5, underline);
            }
        }
    }
}

/// Paint the stack grid band: one cell per entry (icon frame plus
/// truncated label), apps in accent and plain files dimmed.
fn paint_stack_grid(pixels: &mut [u8], stride: usize, width: i32, entries: &[StackEntry]) {
    const DIM: [u8; 4] = [0x4a, 0x44, 0x44, 0xff];
    let shown = stack_shown(width, entries.len());
    let max_chars = ((STACK_CELL - 8) / GLYPH_ADVANCE).max(1) as usize;
    for (index, entry) in entries.iter().enumerate().take(shown) {
        let (x0, y0) = stack_cell_origin(width, index);
        let frame = if entry.app { ACCENT } else { DIM };
        // Icon frame, 40x32.
        for dx in 0..40 {
            for dy in 0..32 {
                let edge = dx == 0 || dy == 0 || dx == 39 || dy == 31;
                if edge {
                    put_pixel(pixels, stride, x0 + 4 + dx, y0 + dy, frame);
                }
            }
        }
        if let Some(initial) = entry.name.chars().next() {
            if let Some(glyph) = glyph_index(initial) {
                blit_glyph(pixels, stride, x0 + 22, y0 + 6, glyph, frame);
            }
        }
        // Label, truncated to the cell.
        let mut gx = x0 + 4;
        let gy = y0 + 36;
        for ch in entry.name.chars().take(max_chars) {
            if ch == ' ' {
                gx += GLYPH_ADVANCE;
                continue;
            }
            if let Some(glyph) = glyph_index(ch) {
                blit_glyph(pixels, stride, gx, gy, glyph, frame);
            }
            gx += GLYPH_ADVANCE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::AppEntry;
    use crate::model::WindowEntry;

    fn entry(app_id: &str, name: &str) -> AppEntry {
        AppEntry {
            app_id: app_id.to_owned(),
            name: name.to_owned(),
            generic_name: None,
            keywords: Vec::new(),
            argv: Vec::new(),
            icon: None,
        }
    }

    fn provider() -> AppProvider {
        AppProvider::new(vec![
            entry("org.gnome.Terminal.desktop", "Terminal"),
            entry("org.gnome.Nautilus.desktop", "Files"),
        ])
    }

    fn window(id: u64, title: &str, app_id: Option<&str>, active: bool) -> WindowEntry {
        WindowEntry::new(id, title, active).with_app_id(app_id.map(str::to_owned))
    }

    #[test]
    fn pinned_first_then_running_unpinned() {
        let apps = provider();
        let windows = vec![
            window(1, "Terminal", Some("org.gnome.Terminal"), true),
            window(2, "Vlc", Some("org.videolan.VLC"), false),
        ];
        let items = dock_items(&["org.gnome.Terminal.desktop".to_owned()], &apps, &windows);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].app_id, "org.gnome.Terminal.desktop");
        assert!(items[0].pinned && items[0].running() && items[0].active);
        assert_eq!(items[0].windows, vec![1]);
        assert_eq!(items[1].app_id, "org.videolan.VLC");
        assert!(!items[1].pinned && items[1].running());
    }

    #[test]
    fn stopped_favorite_keeps_slot_without_windows() {
        let apps = provider();
        let items = dock_items(&["org.gnome.Nautilus.desktop".to_owned()], &apps, &[]);
        assert_eq!(items.len(), 1);
        assert!(items[0].pinned && !items[0].running());
        assert_eq!(items[0].name, "Files");
    }

    #[test]
    fn running_group_resolves_to_filed_desktop_id() {
        let apps = provider();
        let windows = vec![window(5, "Terminal", Some("org.gnome.Terminal"), false)];
        let items = dock_items(&[], &apps, &windows);
        assert_eq!(items.len(), 1);
        // Pins and launches need the exact filed id.
        assert_eq!(items[0].app_id, "org.gnome.Terminal.desktop");
        assert_eq!(items[0].name, "Terminal");
    }

    #[test]
    fn title_fallback_matches_id_less_clients() {
        let apps = provider();
        let windows = vec![window(3, "Terminal", None, false)];
        let items = dock_items(&["org.gnome.Terminal.desktop".to_owned()], &apps, &windows);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].windows, vec![3]);
    }

    #[test]
    fn same_app_windows_group_into_one_item() {
        let apps = provider();
        let windows = vec![
            window(1, "t1", Some("org.videolan.VLC"), false),
            window(2, "t2", Some("org.videolan.VLC"), true),
        ];
        let items = dock_items(&[], &apps, &windows);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].windows, vec![1, 2]);
        assert!(items[0].active);
    }

    #[test]
    fn press_matrix_covers_lifecycle() {
        let apps = provider();
        let windows = vec![window(1, "Terminal", Some("org.gnome.Terminal"), true)];
        let items = dock_items(
            &[
                "org.gnome.Terminal.desktop".to_owned(),
                "org.gnome.Nautilus.desktop".to_owned(),
            ],
            &apps,
            &windows,
        );
        // Left on running switches to the focused window.
        assert_eq!(
            dock_press(&items, &[1], 0, BTN_LEFT),
            Some(DockAction::Switch(1))
        );
        // Left on stopped launches.
        assert_eq!(
            dock_press(&items, &[1], 1, BTN_LEFT),
            Some(DockAction::Launch("org.gnome.Nautilus.desktop".to_owned()))
        );
        // Middle closes the running window; nothing on stopped.
        assert_eq!(
            dock_press(&items, &[1], 0, BTN_MIDDLE),
            Some(DockAction::Close(1))
        );
        assert_eq!(dock_press(&items, &[1], 1, BTN_MIDDLE), None);
        // Right toggles pins either way.
        assert_eq!(
            dock_press(&items, &[1], 0, BTN_RIGHT),
            Some(DockAction::TogglePin(
                "org.gnome.Terminal.desktop".to_owned()
            ))
        );
        // Unknown button and out-of-range index do nothing.
        assert_eq!(dock_press(&items, &[1], 0, 0x113), None);
        assert_eq!(dock_press(&items, &[1], 9, BTN_LEFT), None);
    }

    #[test]
    fn switch_prefers_focused_window() {
        let apps = provider();
        let windows = vec![
            window(1, "t1", Some("org.videolan.VLC"), false),
            window(2, "t2", Some("org.videolan.VLC"), true),
        ];
        let items = dock_items(&[], &apps, &windows);
        assert_eq!(
            dock_press(&items, &[2], 0, BTN_LEFT),
            Some(DockAction::Switch(2))
        );
        assert_eq!(
            dock_press(&items, &[], 0, BTN_LEFT),
            Some(DockAction::Switch(1))
        );
    }

    #[test]
    fn paint_marks_running_and_leaves_stopped_plain() {
        let apps = provider();
        let windows = vec![window(1, "Terminal", Some("org.gnome.Terminal"), true)];
        let items = dock_items(
            &[
                "org.gnome.Terminal.desktop".to_owned(),
                "org.gnome.Nautilus.desktop".to_owned(),
            ],
            &apps,
            &windows,
        );
        let width = 1280;
        let mut pixels = vec![0u8; width as usize * DOCK_H as usize * BYTES_PER_PIXEL];
        paint_dock(&mut pixels, width, DOCK_H, &items);
        // Something painted: not all backdrop.
        let (chunks, _) = pixels.as_chunks::<4>();
        assert!(chunks.iter().any(|px| *px != BG));
        // Running underline row differs between the two icons' slots.
        let stride = width as usize * BYTES_PER_PIXEL;
        let row = |slot: usize| {
            let (x0, _, _, _) = dock_slot(width, slot, items.len());
            (8..DOCK_SLOT - 8)
                .map(|dx| {
                    let at =
                        ((DOCK_H - 5) as usize * stride) + ((x0 + dx) as usize * BYTES_PER_PIXEL);
                    pixels[at..at + 4].to_vec()
                })
                .collect::<Vec<_>>()
        };
        assert_ne!(row(0), row(1));
    }

    #[test]
    fn paint_with_artwork_blits_theme_pixels_over_initial() {
        let apps = provider();
        let items = dock_items(&["org.gnome.Terminal.desktop".to_owned()], &apps, &[]);
        assert_eq!(items.len(), 1);
        let width = 1280;
        // Solid artwork in a color the fallback never paints.
        let art = crate::icons::Artwork {
            size: DOCK_ICON_PX,
            argb: vec![0x12; DOCK_ICON_PX as usize * DOCK_ICON_PX as usize * BYTES_PER_PIXEL],
        };
        let mut art_pixels = vec![0u8; width as usize * DOCK_H as usize * BYTES_PER_PIXEL];
        paint_dock_stacked_with_icons(&mut art_pixels, width, DOCK_H, &items, None, &[Some(art)]);
        // Slot center carries the artwork color...
        let stride = width as usize * BYTES_PER_PIXEL;
        let (x0, _, _, _) = dock_slot(width, 0, items.len());
        let at =
            ((DOCK_H / 2) as usize * stride) + ((x0 + DOCK_SLOT / 2) as usize * BYTES_PER_PIXEL);
        assert_eq!(&art_pixels[at..at + 4], &[0x12, 0x12, 0x12, 0x12]);
        // ...while the fallback paints no such pixel anywhere.
        let mut plain = vec![0u8; width as usize * DOCK_H as usize * BYTES_PER_PIXEL];
        paint_dock(&mut plain, width, DOCK_H, &items);
        assert_ne!(art_pixels, plain);
        let (chunks, _) = plain.as_chunks::<4>();
        assert!(
            chunks.iter().all(|px| *px != [0x12, 0x12, 0x12, 0x12]),
            "fallback must not paint the artwork color"
        );
    }

    fn stack_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "roost-stack-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("stack fixture dir");
        std::fs::write(dir.join("notes.txt"), "plain file sorts after launchables")
            .expect("stack plain file");
        std::fs::write(
            dir.join("tool.desktop"),
            "[Desktop Entry]\nName=Tool\nExec=/bin/true\nType=Application\n",
        )
        .expect("stack desktop file");
        std::fs::write(
            dir.join("hidden.desktop"),
            "[Desktop Entry]\nName=Hidden\nExec=/bin/true\nType=Application\nNoDisplay=true\n",
        )
        .expect("stack hidden desktop file");
        std::fs::create_dir_all(dir.join("subdir")).expect("stack subdir");
        dir
    }

    #[test]
    fn stack_read_sorts_apps_first_and_skips_unusable() {
        let dir = stack_dir();
        let entries = read_stack(&dir);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].app);
        assert_eq!(entries[0].name, "Tool");
        assert!(!entries[1].app);
        assert_eq!(entries[1].name, "notes");
        let _ = std::fs::remove_dir_all(&dir);
        // Missing directories read as empty, never an error.
        assert!(read_stack(&dir).is_empty());
    }

    #[test]
    fn stack_favorite_composes_item_without_claiming_windows() {
        let dir = stack_dir();
        let fav = format!("{STACK_PREFIX}{}", dir.display());
        let apps = provider();
        let windows = vec![window(
            1,
            dir.file_name().unwrap().to_str().unwrap(),
            None,
            false,
        )];
        let items = dock_items(std::slice::from_ref(&fav), &apps, &windows);
        // The stack claims nothing: the same-titled window stands as
        // its own unpinned group beside the stack item.
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].app_id, fav);
        assert!(items[0].pinned && !items[0].running());
        assert_eq!(items[0].stack_dir.as_deref(), Some(dir.as_path()));
        assert!(!items[1].pinned && items[1].running());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stack_press_opens_and_cell_layout_round_trips() {
        let dir = stack_dir();
        let fav = format!("{STACK_PREFIX}{}", dir.display());
        let items = dock_items(&[fav], &provider(), &[]);
        assert_eq!(
            dock_press(&items, &[], 0, BTN_LEFT),
            Some(DockAction::OpenStack(0))
        );
        assert_eq!(dock_press(&items, &[], 0, BTN_MIDDLE), None);
        let width = 1280;
        let entries = read_stack(&dir);
        let shown = stack_shown(width, entries.len());
        assert_eq!(shown, entries.len());
        // First cell hit-tests back to index zero.
        let (cx, cy) = stack_cell_origin(width, 0);
        assert_eq!(stack_cell_at(width, cx + 4, cy + 4, shown), Some(0));
        // Below the band is not a cell.
        assert_eq!(stack_cell_at(width, cx + 4, STACK_GRID_H + 1, shown), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stack_paint_grows_band_and_marks_cells() {
        let dir = stack_dir();
        let fav = format!("{STACK_PREFIX}{}", dir.display());
        let items = dock_items(&[fav], &provider(), &[]);
        let entries = read_stack(&dir);
        let width = 640;
        let height = STACK_GRID_H + DOCK_H;
        let mut pixels = vec![0u8; width as usize * height as usize * BYTES_PER_PIXEL];
        paint_dock_stacked(&mut pixels, width, height, &items, Some(&entries));
        let (chunks, _) = pixels.as_chunks::<4>();
        assert!(chunks.iter().any(|px| *px != BG));
        // First cell's icon frame painted inside the band.
        let (cx, cy) = stack_cell_origin(width, 0);
        let stride = width as usize * BYTES_PER_PIXEL;
        let at = (cy as usize * stride) + ((cx + 4) as usize * BYTES_PER_PIXEL);
        assert_ne!(&pixels[at..at + 4], &BG);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
