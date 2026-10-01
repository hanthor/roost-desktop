//! AppIndicator host: StatusNotifierWatcher role plus item menus.
//!
//! The shell owns the `org.kde.StatusNotifierWatcher` name (GNOME
//! sessions run no watcher, so nothing fights for it) and tracks the
//! items that register. Icons paint into the bar's right edge beside
//! the service tiles; a press opens the item's dbusmenu as a popup.
//!
//! The core ([`IndicatorHost`], layout, paint) is wire-free. The
//! D-Bus edge ([`WatcherBus`]) polls on the panel's slow tick — the
//! same degradation shape as settings and tiles: no bus, or a taken
//! watcher name, reads as an empty host, never a hang.

use std::sync::{Arc, Mutex};

use crate::overview::{
    blit_glyph, glyph_index, put_pixel, ACCENT, BYTES_PER_PIXEL, DOT_GAP, DOT_MARGIN, DOT_SIZE,
    FONT_SCALE, GLYPH_ADVANCE,
};
use crate::popup::Rect;

/// Well-known watcher name and object path (kde StatusNotifier spec).
pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
/// Watcher object path.
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
/// Fallback item path when a registration carries no object path.
pub const FALLBACK_ITEM_PATH: &str = "/StatusNotifierItem";
/// Fallback menu path is per-item; empty means no menu.
pub const INDICATOR_CELL: i32 = 28;

/// One dbusmenu row, flattened one level (submenus arrive as their
/// parent row; nesting deeper is later work).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    /// dbusmenu id for the `Event` call.
    pub id: i32,
    /// Row label.
    pub label: String,
    /// False rows paint dimmed and never fire.
    pub enabled: bool,
}

/// An indicator icon: a theme name, or raw pixmap bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndicatorIcon {
    /// Icon theme name (painted as its initial, like dock icons).
    Named(String),
    /// `width` x `height` ARGB32 pixels, compositor byte order.
    Pixmap {
        /// Pixmap width.
        width: i32,
        /// Pixmap height.
        height: i32,
        /// Native `B,G,R,A` bytes ready for the shm buffer.
        argb: Vec<u8>,
    },
}

/// One hosted indicator: identity plus its current icon and menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndicatorItem {
    /// Registration string (`bus name` or `bus name/path`).
    pub service: String,
    /// Item title for fallback paint.
    pub title: String,
    /// Current icon.
    pub icon: IndicatorIcon,
    /// Current one-level menu.
    pub menu: Vec<MenuEntry>,
}

/// Raw StatusNotifier properties for one item: identity, status,
/// and both icon channels. Name resolution happens in the caller
/// (which owns the artwork cache), so this stays bus-shaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemInfo {
    /// Registration string (`bus name` or `bus name/path`).
    pub service: String,
    /// Item title for fallback paint.
    pub title: String,
    /// Item status (`Active`, `Passive`, `NeedsAttention`).
    pub status: String,
    /// Normal icon theme name.
    pub icon_name: String,
    /// Normal icon pixmaps (`a(iiay)` payload).
    pub icon_pixmap: Vec<(i32, i32, Vec<u8>)>,
    /// Attention icon theme name.
    pub attention_name: String,
    /// Attention icon pixmaps.
    pub attention_pixmap: Vec<(i32, i32, Vec<u8>)>,
}

impl ItemInfo {
    /// Largest normal pixmap as an icon, if any.
    pub fn pixmap_icon(&self) -> Option<IndicatorIcon> {
        pixmaps_icon(&self.icon_pixmap)
    }

    /// Largest attention pixmap as an icon, if any.
    pub fn attention_pixmap_icon(&self) -> Option<IndicatorIcon> {
        pixmaps_icon(&self.attention_pixmap)
    }

    /// True while the item requests attention.
    pub fn needs_attention(&self) -> bool {
        self.status == "NeedsAttention"
    }
}

/// Largest pixmap in a payload as an icon, if any is usable.
fn pixmaps_icon(pixmaps: &[(i32, i32, Vec<u8>)]) -> Option<IndicatorIcon> {
    let (w, h, bytes) = pick_pixmap(pixmaps.to_vec())?;
    Some(IndicatorIcon::Pixmap {
        width: w,
        height: h,
        argb: argb_to_shm(&bytes),
    })
}

/// Wire-free indicator set: register, refresh, remove by service.
#[derive(Debug, Default)]
pub struct IndicatorHost {
    items: Vec<IndicatorItem>,
}

impl IndicatorHost {
    /// Empty host.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current items in registration order.
    pub fn items(&self) -> &[IndicatorItem] {
        &self.items
    }

    /// Item for `service`, if hosted.
    pub fn get(&self, service: &str) -> Option<&IndicatorItem> {
        self.items.iter().find(|item| item.service == service)
    }

    /// Insert or replace the item for `service`.
    pub fn upsert(&mut self, item: IndicatorItem) {
        if let Some(slot) = self
            .items
            .iter_mut()
            .find(|slot| slot.service == item.service)
        {
            *slot = item;
        } else {
            self.items.push(item);
        }
    }

    /// Remove the item for `service`. Returns whether one was hosted.
    pub fn remove(&mut self, service: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.service != service);
        self.items.len() != before
    }

    /// Drop services no longer registered (vanished clients).
    pub fn retain_registered(&mut self, registered: &[String]) {
        self.items.retain(|item| registered.contains(&item.service));
    }
}

/// Cells for `count` indicators extending left from `right_x` (the
/// tile row's left edge) in a `strip_h`-high strip.
pub fn indicator_cells(right_x: i32, strip_h: i32, count: usize) -> Vec<Rect> {
    (0..count)
        .map(|index| {
            let x = right_x - (index as i32 + 1) * INDICATOR_CELL;
            Rect {
                x,
                y: (strip_h - INDICATOR_CELL) / 2,
                w: INDICATOR_CELL,
                h: INDICATOR_CELL,
            }
        })
        .collect()
}

/// Left edge a `count`-cell indicator row starts from, given the
/// surface `width`: mirror of the tile packing in
/// [`crate::popup::panel_layout`] (three dots from the right
/// margin), so cells sit exactly left of the sound tile.
pub fn indicator_right_x(width: i32) -> i32 {
    width - DOT_MARGIN - 3 * DOT_SIZE - 2 * DOT_GAP - 4 - 4
}

/// Cell index under the strip point, if any.
pub fn indicator_at(cells: &[Rect], x: i32, y: i32) -> Option<usize> {
    cells.iter().position(|cell| cell.contains(x, y))
}

/// Paint indicator cells: pixmaps blit centered and clipped, named
/// icons paint their initial like dock icons.
pub fn paint_indicators(pixels: &mut [u8], width: i32, cells: &[Rect], items: &[IndicatorItem]) {
    let stride = width as usize * BYTES_PER_PIXEL;
    for (cell, item) in cells.iter().zip(items.iter()) {
        // Cell frame so the indicator reads as a tile.
        for dx in 0..cell.w {
            put_pixel(pixels, stride, cell.x + dx, cell.y, ACCENT);
            put_pixel(pixels, stride, cell.x + dx, cell.y + cell.h - 1, ACCENT);
        }
        match &item.icon {
            IndicatorIcon::Pixmap {
                width,
                height,
                argb,
            } => {
                let w = (*width).min(cell.w - 4).max(1);
                let h = (*height).min(cell.h - 4).max(1);
                let ox = cell.x + (cell.w - w) / 2;
                let oy = cell.y + (cell.h - h) / 2;
                // Nearest-sample the pixmap into the cell.
                for dy in 0..h {
                    for dx in 0..w {
                        let sx = (dx * *width / w).clamp(0, *width - 1) as usize;
                        let sy = (dy * *height / h).clamp(0, *height - 1) as usize;
                        let from = (sy * *width as usize + sx) * BYTES_PER_PIXEL;
                        let into =
                            ((oy + dy) as usize * stride) + ((ox + dx) as usize * BYTES_PER_PIXEL);
                        if let (Some(src), Some(dst)) = (
                            argb.get(from..from + BYTES_PER_PIXEL),
                            pixels.get_mut(into..into + BYTES_PER_PIXEL),
                        ) {
                            dst.copy_from_slice(src);
                        }
                    }
                }
            }
            IndicatorIcon::Named(name) => {
                let face = name.chars().next().unwrap_or('?');
                if let Some(glyph) = glyph_index(face) {
                    let gx = cell.x + (cell.w - 4 * FONT_SCALE) / 2;
                    let gy = cell.y + (cell.h - 5 * FONT_SCALE) / 2;
                    blit_glyph(pixels, stride, gx, gy, glyph, ACCENT);
                }
            }
        }
    }
}

/// Menu rows for an indicator popup: labels truncated to the box.
pub fn paint_indicator_menu(pixels: &mut [u8], stride: usize, rect: &Rect, item: &IndicatorItem) {
    const DIM: [u8; 4] = [0x4a, 0x44, 0x44, 0xff];
    const ROW_H: i32 = 24;
    let max_chars = ((rect.w - 24) / GLYPH_ADVANCE).max(1) as usize;
    for (row, entry) in item.menu.iter().enumerate() {
        let gy = rect.y + 10 + row as i32 * ROW_H;
        if gy + ROW_H > rect.y + rect.h {
            break;
        }
        let color = if entry.enabled { ACCENT } else { DIM };
        let mut gx = rect.x + 12;
        for ch in entry.label.chars().take(max_chars) {
            if ch == ' ' {
                gx += GLYPH_ADVANCE;
                continue;
            }
            if let Some(glyph) = glyph_index(ch) {
                blit_glyph(pixels, stride, gx, gy, glyph, color);
            }
            gx += GLYPH_ADVANCE;
        }
    }
}

/// Menu row under the popup point, if any.
pub fn menu_row_at(rect: &Rect, y: i32, count: usize) -> Option<usize> {
    const ROW_H: i32 = 24;
    if count == 0 || y < rect.y + 10 {
        return None;
    }
    let row = ((y - rect.y - 10) / ROW_H) as usize;
    (row < count).then_some(row)
}

/// Paint box for indicator-menu row `index`: the inverse of
/// [`menu_row_at`], so the keyboard focus ring lands where presses
/// do. Slightly inset from the menu box edges.
pub fn menu_row_rect(rect: &Rect, index: usize, count: usize) -> Option<Rect> {
    const ROW_H: i32 = 24;
    if index >= count {
        return None;
    }
    Some(Rect {
        x: rect.x + 4,
        y: rect.y + 10 + index as i32 * ROW_H,
        w: (rect.w - 8).max(0),
        h: ROW_H,
    })
}

/// Split a registration string into bus name and object path.
pub fn split_service(service: &str) -> (String, String) {
    match service.split_once('/') {
        Some((bus, path)) => (bus.to_owned(), format!("/{path}")),
        None => (service.to_owned(), FALLBACK_ITEM_PATH.to_owned()),
    }
}

/// Pick the largest pixmap from an `IconPixmap` (`a(iiay)`) payload.
pub fn pick_pixmap(pixmaps: Vec<(i32, i32, Vec<u8>)>) -> Option<(i32, i32, Vec<u8>)> {
    pixmaps
        .into_iter()
        .filter(|(w, h, bytes)| {
            *w > 0 && *h > 0 && bytes.len() == *w as usize * *h as usize * BYTES_PER_PIXEL
        })
        .max_by_key(|(w, h, _)| *w as i64 * *h as i64)
}

/// Convert SNI pixmap bytes (big-endian ARGB32 words) to shm
/// `B,G,R,A` order.
pub fn argb_to_shm(argb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(argb.len());
    let (chunks, _) = argb.as_chunks::<4>();
    for px in chunks {
        out.push(px[2]);
        out.push(px[1]);
        out.push(px[0]);
        out.push(px[3]);
    }
    out
}

/// Parse one dbusmenu layout struct into one-level rows: the root's
/// children with labels, skipping invisible rows and separators.
pub fn parse_menu_layout(layout: &zbus::zvariant::OwnedValue) -> Vec<MenuEntry> {
    use zbus::zvariant::Value;
    let Ok(Value::Structure(root)) = Value::try_from(layout) else {
        return Vec::new();
    };
    let fields = root.fields();
    let Some(Value::Array(children)) = fields.get(2) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for index in 0..children.len() {
        let Ok(Some(child)) = children.get::<zbus::zvariant::OwnedValue>(index) else {
            continue;
        };
        if let Some(entry) = parse_menu_node(&child) {
            rows.push(entry);
        }
    }
    rows
}

/// One layout node struct `(id, properties, children)` to a row.
fn parse_menu_node(node: &zbus::zvariant::OwnedValue) -> Option<MenuEntry> {
    use zbus::zvariant::Value;
    let Ok(Value::Structure(node)) = Value::try_from(node) else {
        return None;
    };
    let fields = node.fields();
    let Value::I32(id) = fields.first()? else {
        return None;
    };
    let Value::Dict(props) = fields.get(1)? else {
        return None;
    };
    let visible_key = zbus::zvariant::Str::from("visible");
    let visible: bool = props.get(&visible_key).ok()?.unwrap_or(true);
    if !visible {
        return None;
    }
    let type_key = zbus::zvariant::Str::from("type");
    let kind: String = props.get(&type_key).ok()?.unwrap_or_default();
    if kind == "separator" {
        return None;
    }
    let label_key = zbus::zvariant::Str::from("label");
    let label: String = props.get(&label_key).ok()?.unwrap_or_default();
    if label.is_empty() {
        return None;
    }
    let enabled_key = zbus::zvariant::Str::from("enabled");
    let enabled: bool = props.get(&enabled_key).ok()?.unwrap_or(true);
    Some(MenuEntry {
        id: *id,
        label,
        enabled,
    })
}

/// Shared watcher registrations (the interface object and the poll
/// path read the same state).
#[derive(Debug, Default)]
struct WatcherState {
    /// Registered item services, in registration order.
    items: Vec<String>,
    /// Registered host services.
    hosts: Vec<String>,
}

/// The `org.kde.StatusNotifierWatcher` object: registration methods
/// plus the three spec properties. Signal emission is unwired in v1
/// (single-host session; our host reads this same state directly).
#[derive(Debug)]
struct Watcher {
    state: Arc<Mutex<WatcherState>>,
}

#[zbus::interface(interface = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    /// Register an item service (`bus name` or `bus name/path`).
    fn register_status_notifier_item(&mut self, service: String) {
        if let Ok(mut state) = self.state.lock() {
            if !state.items.contains(&service) {
                state.items.push(service);
            }
        }
    }

    /// Register a host service.
    fn register_status_notifier_host(&mut self, service: String) {
        if let Ok(mut state) = self.state.lock() {
            if !state.hosts.contains(&service) {
                state.hosts.push(service);
            }
        }
    }

    /// Registered item services.
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.items.clone())
            .unwrap_or_default()
    }

    /// Whether any host registered.
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        self.state
            .lock()
            .map(|state| !state.hosts.is_empty())
            .unwrap_or_default()
    }

    /// Watcher protocol version (always zero per the spec).
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}

/// D-Bus edge behind the host: owns the watcher role when the name
/// is free, fetches item properties on the slow tick.
pub struct WatcherBus {
    conn: Option<zbus::blocking::Connection>,
    state: Arc<Mutex<WatcherState>>,
}

impl Default for WatcherBus {
    /// Disconnected edge, like [`WatcherBus::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl WatcherBus {
    /// Disconnected edge; [`WatcherBus::ensure`] connects.
    pub fn new() -> Self {
        Self {
            conn: None,
            state: Arc::new(Mutex::new(WatcherState::default())),
        }
    }

    /// Connect the session bus, serve the watcher object, and take
    /// the watcher name without queueing. Returns true while we own
    /// the role. A taken name (or no bus) reads as no host: the bar
    /// keeps its tiles and nothing else changes.
    pub fn ensure(&mut self) -> bool {
        if self.conn.is_some() {
            return true;
        }
        let conn = match zbus::blocking::connection::Builder::session() {
            Ok(builder) => builder,
            Err(e) => {
                eprintln!("roost-shell-host: indicator bus unavailable: {e}");
                return false;
            }
        };
        let watcher = Watcher {
            state: self.state.clone(),
        };
        let conn = match conn.serve_at(WATCHER_PATH, watcher) {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("roost-shell-host: indicator serve failed: {e}");
                return false;
            }
        };
        let conn = match conn.build() {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("roost-shell-host: indicator connect failed: {e}");
                return false;
            }
        };
        let reply = conn
            .request_name_with_flags(WATCHER_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into());
        match reply {
            Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                self.conn = Some(conn);
                // Our own host registration, in-process.
                if let Ok(mut state) = self.state.lock() {
                    state.hosts.push("roost-shell-host".to_owned());
                }
                true
            }
            Ok(other) => {
                eprintln!("roost-shell-host: watcher name taken ({other:?}), indicators off");
                false
            }
            Err(e) => {
                eprintln!("roost-shell-host: watcher name request failed: {e}");
                false
            }
        }
    }

    /// Services registered since the edge started.
    pub fn registered(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.items.clone())
            .unwrap_or_default()
    }

    /// Raw item properties for icon selection. The caller decides
    /// which name to resolve (attention vs normal) and decodes
    /// through its own artwork cache; this edge stays paint-free.
    /// `None` when the item vanished or its properties unreadable.
    pub fn fetch_info(&self, service: &str) -> Option<ItemInfo> {
        let conn = self.conn.as_ref()?;
        let (bus, path) = split_service(service);
        let item = zbus::blocking::Proxy::new(
            conn,
            bus.as_str(),
            path.as_str(),
            "org.kde.StatusNotifierItem",
        )
        .ok()?;
        let title: String = item
            .get_property("Title")
            .ok()
            .filter(|t: &String| !t.is_empty())
            .unwrap_or_else(|| service.to_owned());
        let status: String = item.get_property("Status").unwrap_or_default();
        let icon_name: String = item.get_property("IconName").unwrap_or_default();
        let icon_pixmap: Vec<(i32, i32, Vec<u8>)> =
            item.get_property("IconPixmap").unwrap_or_default();
        let attention_name: String = item.get_property("AttentionIconName").unwrap_or_default();
        let attention_pixmap: Vec<(i32, i32, Vec<u8>)> =
            item.get_property("AttentionIconPixmap").unwrap_or_default();
        Some(ItemInfo {
            service: service.to_owned(),
            title,
            status,
            icon_name,
            icon_pixmap,
            attention_name,
            attention_pixmap,
        })
    }

    /// Fetch one item's live properties into an [`IndicatorItem`].
    /// `None` when the item vanished or its properties unreadable.
    /// Theme names stay unresolved here: the host resolves them
    /// through its artwork cache after this returns.
    pub fn fetch_item(&self, service: &str) -> Option<IndicatorItem> {
        let info = self.fetch_info(service)?;
        let icon = info.pixmap_icon().unwrap_or_else(|| {
            if info.icon_name.is_empty() {
                IndicatorIcon::Named(info.service.clone())
            } else {
                IndicatorIcon::Named(info.icon_name.clone())
            }
        });
        Some(IndicatorItem {
            service: info.service,
            title: info.title,
            icon,
            menu: Vec::new(),
        })
    }

    /// Fetch one item's menu rows (one level). Empty when the item
    /// has no menu or the layout is unreadable.
    pub fn fetch_menu(&self, service: &str) -> Vec<MenuEntry> {
        let Some(conn) = self.conn.as_ref() else {
            return Vec::new();
        };
        let (bus, path) = split_service(service);
        let item = match zbus::blocking::Proxy::new(
            conn,
            bus.as_str(),
            path.as_str(),
            "org.kde.StatusNotifierItem",
        ) {
            Ok(item) => item,
            Err(_) => return Vec::new(),
        };
        let menu_path: String = match item.get_property("Menu") {
            Ok(path) => path,
            Err(_) => return Vec::new(),
        };
        let menu = match zbus::blocking::Proxy::new(
            conn,
            bus.as_str(),
            menu_path.as_str(),
            "com.canonical.dbusmenu",
        ) {
            Ok(menu) => menu,
            Err(_) => return Vec::new(),
        };
        let props = vec![
            "label".to_owned(),
            "enabled".to_owned(),
            "visible".to_owned(),
            "type".to_owned(),
        ];
        let reply: Result<(u32, zbus::zvariant::OwnedValue), zbus::Error> =
            menu.call("GetLayout", &(0i32, 1i32, props));
        match reply {
            Ok((_, layout)) => parse_menu_layout(&layout),
            Err(_) => Vec::new(),
        }
    }

    /// Send `Activate` to an item (empty menus activate directly).
    pub fn activate(&self, service: &str) -> bool {
        let Some(conn) = self.conn.as_ref() else {
            return false;
        };
        let (bus, path) = split_service(service);
        let Ok(item) = zbus::blocking::Proxy::new(
            conn,
            bus.as_str(),
            path.as_str(),
            "org.kde.StatusNotifierItem",
        ) else {
            return false;
        };
        item.call::<&str, _, ()>("Activate", &(0i32, 0i32)).is_ok()
    }

    /// Send a dbusmenu `Event` for a row press.
    pub fn fire_menu(&self, service: &str, id: i32) -> bool {
        let Some(conn) = self.conn.as_ref() else {
            return false;
        };
        let (bus, path) = split_service(service);
        let Ok(item) = zbus::blocking::Proxy::new(
            conn,
            bus.as_str(),
            path.as_str(),
            "org.kde.StatusNotifierItem",
        ) else {
            return false;
        };
        let menu_path: String = match item.get_property("Menu") {
            Ok(path) => path,
            Err(_) => return false,
        };
        let Ok(menu) = zbus::blocking::Proxy::new(
            conn,
            bus.as_str(),
            menu_path.as_str(),
            "com.canonical.dbusmenu",
        ) else {
            return false;
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0);
        menu.call::<&str, _, ()>(
            "Event",
            &(id, "clicked", zbus::zvariant::Value::U32(0), stamp),
        )
        .is_ok()
    }
}

impl WatcherBus {
    /// Drop vanished clients: services whose bus name lost its
    /// owner. Without a bus this is a no-op (nothing was ever
    /// registered).
    pub fn prune_vanished(&self, services: &[String]) -> Vec<String> {
        let Some(conn) = self.conn.as_ref() else {
            return services.to_owned();
        };
        services
            .iter()
            .filter(|service| {
                let (bus, _) = split_service(service);
                zbus::blocking::Proxy::new(
                    conn,
                    "org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                )
                .and_then(|dbus| dbus.call::<&str, _, (String,)>("GetNameOwner", &(bus,)))
                .is_ok()
            })
            .cloned()
            .collect()
    }

    /// Forget services no longer alive, so re-polls stop refetching
    /// ghosts the bus already dropped.
    pub fn forget(&self, gone: &[String]) {
        if let Ok(mut state) = self.state.lock() {
            state.items.retain(|item| !gone.contains(item));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overview::{BG, BYTES_PER_PIXEL};

    fn item(service: &str, icon: IndicatorIcon) -> IndicatorItem {
        IndicatorItem {
            service: service.to_owned(),
            title: format!("{service} title"),
            icon,
            menu: Vec::new(),
        }
    }

    #[test]
    fn host_upsert_replaces_remove_drops_and_retain_prunes() {
        let mut host = IndicatorHost::new();
        assert!(host.items().is_empty());
        host.upsert(item("a.service", IndicatorIcon::Named("a".to_owned())));
        host.upsert(item("b.service", IndicatorIcon::Named("b".to_owned())));
        assert_eq!(host.items().len(), 2);
        assert_eq!(host.get("a.service").unwrap().title, "a.service title");
        // Same service replaces in place: no duplicate, new icon wins.
        host.upsert(item(
            "a.service",
            IndicatorIcon::Pixmap {
                width: 1,
                height: 1,
                argb: vec![0xff, 0xff, 0xff, 0xff],
            },
        ));
        assert_eq!(host.items().len(), 2);
        assert!(matches!(
            host.get("a.service").unwrap().icon,
            IndicatorIcon::Pixmap { .. }
        ));
        assert!(host.get("missing.service").is_none());
        // Registration order is stable.
        assert_eq!(host.items()[0].service, "a.service");
        assert_eq!(host.items()[1].service, "b.service");
        assert!(host.remove("a.service"));
        assert!(!host.remove("a.service"));
        assert_eq!(host.items().len(), 1);
        host.retain_registered(&["b.service".to_owned(), "c.service".to_owned()]);
        assert_eq!(host.items().len(), 1);
        host.retain_registered(&["gone.service".to_owned()]);
        assert!(host.items().is_empty());
    }

    #[test]
    fn split_service_defaults_to_fallback_item_path() {
        assert_eq!(
            split_service("org.example.Item"),
            ("org.example.Item".to_owned(), FALLBACK_ITEM_PATH.to_owned())
        );
        assert_eq!(
            split_service("org.example.Item/StatusNotifierItem"),
            (
                "org.example.Item".to_owned(),
                "/StatusNotifierItem".to_owned()
            )
        );
    }

    #[test]
    fn pick_pixmap_takes_largest_valid_entry() {
        assert!(pick_pixmap(Vec::new()).is_none());
        let valid = |w: i32, h: i32| {
            (
                w,
                h,
                vec![0x11u8; w as usize * h as usize * BYTES_PER_PIXEL],
            )
        };
        // The 32x32 entry is corrupt (short bytes): skipped, not picked.
        let picked = pick_pixmap(vec![valid(8, 8), (32, 32, vec![0u8; 7]), valid(16, 16)]);
        assert_eq!(picked.map(|(w, h, _)| (w, h)), Some((16, 16)));
        assert!(pick_pixmap(vec![(0, 8, vec![]), (-1, 4, vec![1, 2, 3])]).is_none());
    }

    #[test]
    fn argb_to_shm_reorders_one_known_pixel() {
        // Four input bytes [b0, b1, b2, b3] map to [b2, b1, b0, b3].
        assert_eq!(
            argb_to_shm(&[0xaa, 0x11, 0x22, 0x33]),
            vec![0x22, 0x11, 0xaa, 0x33]
        );
        assert_eq!(
            argb_to_shm(&[0xaa, 0x11, 0x22, 0x33, 0xff, 0x44, 0x55, 0x66]),
            vec![0x22, 0x11, 0xaa, 0x33, 0x55, 0x44, 0xff, 0x66]
        );
        // A trailing partial word has no full pixel and is dropped.
        assert_eq!(argb_to_shm(&[0xaa, 0x11, 0x22, 0x33, 0x00]).len(), 4);
        assert!(argb_to_shm(&[]).is_empty());
    }

    fn item_info(status: &str) -> ItemInfo {
        ItemInfo {
            service: "test.service".to_owned(),
            title: "Test".to_owned(),
            status: status.to_owned(),
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
            attention_name: String::new(),
            attention_pixmap: Vec::new(),
        }
    }

    /// SNI (`a(iiay)`) payload bytes: `w` x `h` words of filler.
    fn pixmap_bytes(w: i32, h: i32, seed: u8) -> Vec<u8> {
        vec![seed; w as usize * h as usize * BYTES_PER_PIXEL]
    }

    #[test]
    fn item_info_needs_attention_only_for_needs_attention() {
        assert!(item_info("NeedsAttention").needs_attention());
        for status in ["Active", "Passive", "", "needsattention", "NeedsAttention "] {
            assert!(
                !item_info(status).needs_attention(),
                "status {status:?} must not read as attention"
            );
        }
    }

    #[test]
    fn item_info_pixmap_icon_picks_largest_valid() {
        // Same selection semantics as `pick_pixmap`: corrupt and
        // empty entries lose to the largest well-formed payload.
        let small = pixmap_bytes(2, 2, 0x11);
        let large = pixmap_bytes(4, 4, 0x22);
        let mut info = item_info("Active");
        info.icon_pixmap = vec![(2, 2, small), (8, 8, vec![0u8; 7]), (4, 4, large.clone())];
        assert_eq!(
            info.pixmap_icon(),
            Some(IndicatorIcon::Pixmap {
                width: 4,
                height: 4,
                argb: argb_to_shm(&large),
            })
        );
        // No usable entry means no icon.
        info.icon_pixmap = vec![(8, 8, vec![0u8; 7])];
        assert_eq!(info.pixmap_icon(), None);
    }

    #[test]
    fn item_info_attention_pixmap_icon_none_when_empty() {
        assert_eq!(item_info("NeedsAttention").attention_pixmap_icon(), None);
        let mut info = item_info("NeedsAttention");
        info.attention_pixmap = vec![(0, 8, Vec::new())];
        assert_eq!(info.attention_pixmap_icon(), None);
    }

    /// Test-only dbusmenu layout builder: mirrors the wire shape the
    /// parser reads (root node struct with id, `a{sv}` props, and an
    /// `av` children array), built from [`zbus::zvariant::Value`].
    mod layout_fixture {
        use zbus::zvariant::{Array, Dict, OwnedValue, Signature, Str, StructureBuilder, Value};

        fn str_value(text: &'static str) -> Value<'static> {
            Value::Str(Str::from(text))
        }

        /// One `(id, props, children)` node struct value.
        pub fn node(
            id: i32,
            props: Vec<(&'static str, Value<'static>)>,
            children: Vec<Value<'static>>,
        ) -> Value<'static> {
            let mut dict = Dict::new(&Signature::Str, &Signature::Variant);
            for (key, value) in props {
                dict.append(Value::Str(Str::from(key)), Value::Value(Box::new(value)))
                    .expect("test props fit a{sv}");
            }
            let mut kids = Array::new(&Signature::Variant);
            for child in children {
                kids.append(Value::Value(Box::new(child)))
                    .expect("test children fit av");
            }
            Value::Structure(
                StructureBuilder::new()
                    .append_field(Value::I32(id))
                    .append_field(Value::Dict(dict))
                    .append_field(Value::Array(kids))
                    .build()
                    .expect("test node builds"),
            )
        }

        /// Root layout: one kept row, one disabled row, an empty
        /// label, a separator, an invisible row, and a row with a
        /// nested child (one level only: the grandchild must vanish).
        pub fn layout() -> OwnedValue {
            let grandchild = node(61, vec![("label", str_value("Sub"))], Vec::new());
            let root = node(
                0,
                Vec::new(),
                vec![
                    node(
                        1,
                        vec![
                            ("label", str_value("Open")),
                            ("enabled", Value::Bool(true)),
                            ("visible", Value::Bool(true)),
                            ("type", str_value("standard")),
                        ],
                        Vec::new(),
                    ),
                    node(
                        2,
                        vec![
                            ("label", str_value("Quit")),
                            ("enabled", Value::Bool(false)),
                        ],
                        Vec::new(),
                    ),
                    node(3, vec![("label", str_value(""))], Vec::new()),
                    node(
                        4,
                        vec![
                            ("label", str_value("sep")),
                            ("type", str_value("separator")),
                        ],
                        Vec::new(),
                    ),
                    node(
                        5,
                        vec![
                            ("label", str_value("Hidden")),
                            ("visible", Value::Bool(false)),
                        ],
                        Vec::new(),
                    ),
                    // Missing enabled/visible/type read as enabled,
                    // visible, non-separator.
                    node(6, vec![("label", str_value("More"))], vec![grandchild]),
                ],
            );
            OwnedValue::try_from(root).expect("test layout converts")
        }
    }

    #[test]
    fn menu_layout_keeps_labeled_visible_rows_one_level() {
        let rows = parse_menu_layout(&layout_fixture::layout());
        assert_eq!(
            rows,
            vec![
                MenuEntry {
                    id: 1,
                    label: "Open".to_owned(),
                    enabled: true,
                },
                MenuEntry {
                    id: 2,
                    label: "Quit".to_owned(),
                    enabled: false,
                },
                MenuEntry {
                    id: 6,
                    label: "More".to_owned(),
                    enabled: true,
                },
            ]
        );
        assert!(
            !rows.iter().any(|row| row.label == "Sub"),
            "nested children stay one level deep"
        );
    }

    #[test]
    fn menu_layout_rejects_non_structures() {
        let garbage =
            zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::U32(7)).expect("converts");
        assert!(parse_menu_layout(&garbage).is_empty());
    }

    #[test]
    fn indicator_cells_round_trip_through_hit_test() {
        let cells = indicator_cells(1000, 32, 3);
        assert_eq!(cells.len(), 3);
        for (index, cell) in cells.iter().enumerate() {
            assert_eq!((cell.w, cell.h), (INDICATOR_CELL, INDICATOR_CELL));
            let cx = cell.x + cell.w / 2;
            let cy = cell.y + cell.h / 2;
            assert_eq!(indicator_at(&cells, cx, cy), Some(index));
        }
        // Cells extend left from the right edge in order.
        assert!(cells[0].x > cells[1].x);
        assert!(indicator_at(&cells, 0, 0).is_none());
        assert!(indicator_at(&cells, cells[0].x + 1, 1000).is_none());
    }

    fn non_bg_count(pixels: &[u8]) -> usize {
        let (chunks, _) = pixels.as_chunks::<BYTES_PER_PIXEL>();
        chunks.iter().filter(|pixel| **pixel != BG).count()
    }

    #[test]
    fn paint_indicators_and_menu_leave_non_bg_pixels() {
        let named = item("named.service", IndicatorIcon::Named("test".to_owned()));
        let pixmap = item(
            "pixmap.service",
            IndicatorIcon::Pixmap {
                width: 2,
                height: 2,
                argb: vec![0xff; 2 * 2 * BYTES_PER_PIXEL],
            },
        );
        let items = [named, pixmap];
        let cells = indicator_cells(400, 32, items.len());
        let mut pixels = vec![0u8; 400 * 32 * BYTES_PER_PIXEL];
        for (i, byte) in pixels.iter_mut().enumerate() {
            *byte = BG[i % BYTES_PER_PIXEL];
        }
        paint_indicators(&mut pixels, 400, &cells, &items);
        assert!(
            non_bg_count(&pixels) > 0,
            "named and pixmap icons must paint over the strip face"
        );

        let rect = crate::popup::Rect {
            x: 8,
            y: 40,
            w: 300,
            h: 200,
        };
        let stride = 320 * BYTES_PER_PIXEL;
        let with_rows = IndicatorItem {
            menu: vec![MenuEntry {
                id: 1,
                label: "quit".to_owned(),
                enabled: true,
            }],
            ..item("menu.service", IndicatorIcon::Named("m".to_owned()))
        };
        let mut menu_pixels = vec![0u8; 320 * 260 * BYTES_PER_PIXEL];
        for (i, byte) in menu_pixels.iter_mut().enumerate() {
            *byte = BG[i % BYTES_PER_PIXEL];
        }
        paint_indicator_menu(&mut menu_pixels, stride, &rect, &with_rows);
        assert!(
            non_bg_count(&menu_pixels) > 0,
            "a labeled row must paint glyph pixels"
        );
        let empty_rows = IndicatorItem {
            menu: Vec::new(),
            ..item("menu.service", IndicatorIcon::Named("m".to_owned()))
        };
        let mut empty_pixels = vec![0u8; 320 * 260 * BYTES_PER_PIXEL];
        for (i, byte) in empty_pixels.iter_mut().enumerate() {
            *byte = BG[i % BYTES_PER_PIXEL];
        }
        paint_indicator_menu(&mut empty_pixels, stride, &rect, &empty_rows);
        assert_eq!(non_bg_count(&empty_pixels), 0);
    }

    #[test]
    fn menu_row_at_respects_top_pad_and_count() {
        let rect = crate::popup::Rect {
            x: 8,
            y: 40,
            w: 300,
            h: 200,
        };
        assert_eq!(menu_row_at(&rect, 49, 2), None);
        assert_eq!(menu_row_at(&rect, 50, 2), Some(0));
        assert_eq!(menu_row_at(&rect, 73, 2), Some(0));
        assert_eq!(menu_row_at(&rect, 74, 2), Some(1));
        assert_eq!(menu_row_at(&rect, 10_000, 2), None);
        assert_eq!(menu_row_at(&rect, 60, 0), None);
    }

    /// Live stub over a private session bus: a stub
    /// StatusNotifierItem plus its dbusmenu, served in-process, drive
    /// the real [`WatcherBus`] edge. Skips (loudly) when no bus
    /// daemon or connection is available.
    mod live_bus {
        use std::io::BufRead;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use super::layout_fixture;
        use super::{
            argb_to_shm, IndicatorIcon, IndicatorItem, MenuEntry, WatcherBus, FALLBACK_ITEM_PATH,
        };

        const STUB_NAME: &str = "org.test.RoostIndicatorStub";
        const ITEM_PATH: &str = "/StatusNotifierItem";
        const MENU_PATH: &str = "/Menu";

        /// 2x2 SNI pixmap words in A,R,G,B order.
        const PIXMAP: [u8; 16] = [
            0xff, 0x10, 0x20, 0x30, 0xff, 0x40, 0x50, 0x60, 0xff, 0x70, 0x80, 0x90, 0xff, 0xa0,
            0xb0, 0xc0,
        ];

        /// 1x1 attention pixmap, distinct from the normal icon.
        const ATTENTION_PIXMAP: [u8; 4] = [0xee, 0x11, 0x22, 0x33];

        struct StubItem {
            activated: Arc<AtomicBool>,
        }

        #[zbus::interface(name = "org.kde.StatusNotifierItem")]
        impl StubItem {
            #[zbus(property)]
            fn title(&self) -> String {
                "StubIndicator".to_owned()
            }

            #[zbus(property)]
            fn status(&self) -> String {
                "NeedsAttention".to_owned()
            }

            #[zbus(property)]
            fn icon_name(&self) -> String {
                String::new()
            }

            #[zbus(property)]
            fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
                vec![(2, 2, PIXMAP.to_vec())]
            }

            #[zbus(property)]
            fn attention_icon_name(&self) -> String {
                "stub-attention".to_owned()
            }

            #[zbus(property)]
            fn attention_icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
                vec![(1, 1, ATTENTION_PIXMAP.to_vec())]
            }

            #[zbus(property)]
            fn menu(&self) -> String {
                MENU_PATH.to_owned()
            }

            fn activate(&self, _x: i32, _y: i32) {
                self.activated.store(true, Ordering::SeqCst);
            }
        }

        struct StubMenu {
            events: Arc<Mutex<Vec<i32>>>,
        }

        #[zbus::interface(name = "com.canonical.dbusmenu")]
        impl StubMenu {
            fn get_layout(
                &self,
                _parent: i32,
                _depth: i32,
                _names: Vec<String>,
            ) -> (u32, zbus::zvariant::OwnedValue) {
                (1, layout_fixture::layout())
            }

            fn event(
                &self,
                id: i32,
                _kind: String,
                _data: zbus::zvariant::OwnedValue,
                _stamp: u32,
            ) {
                self.events.lock().expect("test lock").push(id);
            }
        }

        /// Process-global session-bus address while a live test owns
        /// the bus; restores the prior value on drop.
        struct BusAddressGuard {
            prior: Option<String>,
        }

        impl BusAddressGuard {
            fn point_at(address: &str) -> Self {
                let prior = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
                std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address);
                Self { prior }
            }
        }

        impl Drop for BusAddressGuard {
            fn drop(&mut self) {
                match &self.prior {
                    Some(value) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", value),
                    None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
                }
            }
        }

        /// Private bus daemon plus its address, or `None` when no
        /// daemon is usable (the caller skips the test).
        fn private_bus() -> Option<(String, std::process::Child)> {
            let mut child = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let stdout = child.stdout.take()?;
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(stdout);
                let mut line = String::new();
                let line = reader.read_line(&mut line).ok().map(|_| line);
                let _ = tx.send(line);
            });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Some(line)) if !line.trim().is_empty() => Some((line.trim().to_owned(), child)),
                _ => {
                    let _ = child.kill();
                    None
                }
            }
        }

        #[test]
        fn stub_item_serves_icon_and_menu_over_private_bus() {
            let Some((address, mut daemon)) = private_bus() else {
                eprintln!("SKIP live indicator test: no usable dbus-daemon on PATH");
                return;
            };
            let _bus_address = BusAddressGuard::point_at(&address);

            let activated = Arc::new(AtomicBool::new(false));
            let events = Arc::new(Mutex::new(Vec::new()));
            let mut stub: Option<zbus::blocking::Connection> = None;
            for _ in 0..50 {
                match zbus::blocking::connection::Builder::address(address.as_str()) {
                    Ok(builder) => {
                        match builder
                            .serve_at(
                                ITEM_PATH,
                                StubItem {
                                    activated: activated.clone(),
                                },
                            )
                            .and_then(|builder| {
                                builder.serve_at(
                                    MENU_PATH,
                                    StubMenu {
                                        events: events.clone(),
                                    },
                                )
                            })
                            .and_then(|builder| builder.name(STUB_NAME))
                            .and_then(|builder| builder.build())
                        {
                            Ok(conn) => {
                                stub = Some(conn);
                                break;
                            }
                            Err(_) => std::thread::sleep(Duration::from_millis(100)),
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            let Some(_stub) = stub else {
                let _ = daemon.kill();
                eprintln!("SKIP live indicator test: private bus refused connections");
                return;
            };

            let service = format!("{STUB_NAME}{ITEM_PATH}");
            assert_eq!(
                super::super::split_service(&service),
                (STUB_NAME.to_owned(), ITEM_PATH.to_owned())
            );
            assert_eq!(ITEM_PATH, FALLBACK_ITEM_PATH);

            let mut bus = WatcherBus::new();
            let mut owned = false;
            for _ in 0..100 {
                if bus.ensure() {
                    owned = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(owned, "host takes the watcher role on a private bus");

            let item: IndicatorItem = bus.fetch_item(&service).expect("stub item fetches");
            assert_eq!(item.service, service);
            assert_eq!(item.title, "StubIndicator");
            assert_eq!(
                item.icon,
                IndicatorIcon::Pixmap {
                    width: 2,
                    height: 2,
                    argb: argb_to_shm(&PIXMAP),
                }
            );

            let info = bus.fetch_info(&service).expect("stub info fetches");
            assert_eq!(info.service, service);
            assert_eq!(info.title, "StubIndicator");
            assert_eq!(info.status, "NeedsAttention");
            assert!(info.needs_attention());
            assert_eq!(info.icon_name, String::new());
            assert_eq!(info.icon_pixmap, vec![(2, 2, PIXMAP.to_vec())]);
            assert_eq!(info.attention_name, "stub-attention");
            assert_eq!(
                info.attention_pixmap,
                vec![(1, 1, ATTENTION_PIXMAP.to_vec())]
            );
            assert_eq!(
                info.attention_pixmap_icon(),
                Some(IndicatorIcon::Pixmap {
                    width: 1,
                    height: 1,
                    argb: argb_to_shm(&ATTENTION_PIXMAP),
                })
            );

            let menu = bus.fetch_menu(&service);
            assert_eq!(
                menu,
                vec![
                    MenuEntry {
                        id: 1,
                        label: "Open".to_owned(),
                        enabled: true,
                    },
                    MenuEntry {
                        id: 2,
                        label: "Quit".to_owned(),
                        enabled: false,
                    },
                    MenuEntry {
                        id: 6,
                        label: "More".to_owned(),
                        enabled: true,
                    },
                ]
            );

            assert!(bus.activate(&service));
            assert!(activated.load(Ordering::SeqCst));
            assert!(bus.fire_menu(&service, 2));
            assert_eq!(*events.lock().expect("test lock"), vec![2]);

            // Our stub owns its name; the ghost never existed.
            assert_eq!(
                bus.prune_vanished(&[service, "org.test.RoostGhost/Item".to_owned()]),
                vec![format!("{STUB_NAME}{ITEM_PATH}")]
            );

            let _ = daemon.kill();
        }
    }
}
