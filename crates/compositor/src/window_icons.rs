//! Client icons are copied into a bounded, compositor-owned PNG cache.
use crate::State;
use smithay::reexports::wayland_server::{
    protocol::{wl_buffer::WlBuffer, wl_shm, wl_surface::WlSurface},
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use std::{collections::HashMap, sync::Mutex};
use wayland_protocols::xdg::toplevel_icon::v1::server::{
    xdg_toplevel_icon_manager_v1::{self as manager, XdgToplevelIconManagerV1},
    xdg_toplevel_icon_v1::{self as icon, XdgToplevelIconV1},
};
const EDGE: u32 = 256;
const MAX_EDGE: i32 = 1024;
const MAX_ICONS: usize = 512;
#[derive(Clone, PartialEq)]
pub(crate) struct Raster {
    pub width: u32,
    pub rgba: Vec<u8>,
}
#[derive(Default)]
pub struct IconData(Mutex<Contents>);
#[derive(Default)]
struct Contents {
    immutable: bool,
    name: Option<String>,
    raster: Option<Raster>,
    buffers: Vec<WlBuffer>,
}
#[derive(Clone)]
struct Assigned {
    name: Option<String>,
    raster: Option<Raster>,
}
pub struct WindowIcons {
    directory: tempfile::TempDir,
    pending: HashMap<WlSurface, Option<Assigned>>,
    committed: HashMap<WlSurface, Option<String>>,
    resources: Vec<XdgToplevelIconV1>,
    serial: u64,
    files: HashMap<String, (Raster, Vec<std::path::PathBuf>)>,
}
impl WindowIcons {
    pub fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, XdgToplevelIconManagerV1, _>(1, ());
        Self {
            directory: tempfile::Builder::new()
                .prefix("roost-icons-")
                .tempdir()
                .expect("private icon cache"),
            pending: HashMap::new(),
            committed: HashMap::new(),
            resources: Vec::new(),
            serial: 0,
            files: HashMap::new(),
        }
    }
    pub(crate) fn cache(&mut self, key: String, raster: Raster) -> Option<String> {
        if let Some((old, paths)) = self.files.get(&key) {
            if old == &raster {
                return paths.last().map(|p| p.to_string_lossy().into_owned());
            }
        }
        if !self.files.contains_key(&key) && self.files.len() >= MAX_ICONS {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        let path = self.directory.path().join(format!("{}.png", self.serial));
        image::save_buffer_with_format(
            &path,
            &raster.rgba,
            raster.width,
            raster.width,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .ok()?;
        let entry = self
            .files
            .entry(key)
            .or_insert_with(|| (raster.clone(), Vec::new()));
        entry.0 = raster;
        entry.1.push(path.clone());
        while entry.1.len() > 2 {
            let _ = std::fs::remove_file(entry.1.remove(0));
        }
        Some(path.to_string_lossy().into_owned())
    }
    pub(crate) fn commit(&mut self, surface: &WlSurface) {
        if let Some(value) = self.pending.remove(surface) {
            if !self.committed.contains_key(surface) && self.committed.len() >= MAX_ICONS {
                return;
            }
            let value = value.and_then(|assigned| {
                assigned
                    .raster
                    .and_then(|r| self.cache(format!("native:{:?}", surface.id()), r))
                    .or(assigned.name)
            });
            self.committed.insert(surface.clone(), value);
        }
    }
    pub(crate) fn get(&self, surface: &WlSurface) -> Option<String> {
        self.committed.get(surface).cloned().flatten()
    }
    pub(crate) fn remove(&mut self, surface: &WlSurface) {
        self.pending.remove(surface);
        self.committed.remove(surface);
        self.remove_cache(&format!("native:{:?}", surface.id()));
    }
    pub(crate) fn remove_cache(&mut self, key: &str) {
        if let Some((_, paths)) = self.files.remove(key) {
            for p in paths {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    pub(crate) fn buffer_destroyed(&mut self, buffer: &WlBuffer) {
        self.resources.retain(Resource::is_alive);
        for resource in &self.resources {
            if resource
                .data::<IconData>()
                .unwrap()
                .0
                .lock()
                .unwrap()
                .buffers
                .contains(buffer)
            {
                resource.post_error(icon::Error::NoBuffer, "icon buffer destroyed before icon");
            }
        }
    }
}
fn name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name.contains('/')
        && !name.contains('\\')
        && name != "."
        && name != ".."
        && !name.chars().any(char::is_control)
}
fn copy_shm(buffer: &WlBuffer, scale: i32) -> Option<Raster> {
    if scale <= 0 {
        return None;
    }
    smithay::wayland::shm::with_buffer_contents(buffer, |ptr, len, data| {
        if data.width <= 0
            || data.width != data.height
            || data.width > MAX_EDGE
            || data.offset < 0
            || data.stride < data.width.checked_mul(4)?
            || !matches!(
                data.format,
                wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888
            )
        {
            return None;
        }
        let end = (data.offset as usize)
            .checked_add((data.height as usize - 1).checked_mul(data.stride as usize)?)?
            .checked_add(data.width as usize * 4)?;
        if end > len {
            return None;
        }
        let width = (data.width as u32).min(EDGE);
        let mut rgba = Vec::with_capacity(width as usize * width as usize * 4);
        for y in 0..width {
            for x in 0..width {
                let source_x = x * data.width as u32 / width;
                let source_y = y * data.height as u32 / width;
                let offset = data.offset as usize
                    + source_y as usize * data.stride as usize
                    + source_x as usize * 4;
                // Checked above: each four-byte pixel lies inside the mapped SHM pool.
                let argb = unsafe { std::ptr::read_unaligned(ptr.add(offset).cast::<u32>()) };
                let a = if data.format == wl_shm::Format::Xrgb8888 {
                    255
                } else {
                    (argb >> 24) as u8
                };
                for shift in [16, 8, 0] {
                    let c = (argb >> shift) as u8;
                    rgba.push(if a == 0 {
                        0
                    } else {
                        ((u32::from(c) * 255 / u32::from(a)).min(255)) as u8
                    });
                }
                rgba.push(a);
            }
        }
        Some(Raster { width, rgba })
    })
    .ok()
    .flatten()
}
impl GlobalDispatch<XdgToplevelIconManagerV1, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<XdgToplevelIconManagerV1>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        let resource = init.init(resource, ());
        resource.icon_size(64);
        resource.icon_size(128);
        resource.done();
    }
}
impl Dispatch<XdgToplevelIconManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &XdgToplevelIconManagerV1,
        request: manager::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            manager::Request::CreateIcon { id } => {
                state.window_icons.resources.retain(Resource::is_alive);
                let icon = init.init(id, IconData::default());
                if state.window_icons.resources.len() >= MAX_ICONS {
                    icon.post_error(icon::Error::InvalidBuffer, "too many icon objects");
                } else {
                    state.window_icons.resources.push(icon);
                }
            }
            manager::Request::SetIcon { toplevel, icon } => {
                let Some(surface) = state
                    .xdg_shell_state
                    .toplevel_surfaces()
                    .iter()
                    .find(|s| s.xdg_toplevel() == &toplevel)
                    .map(|s| s.wl_surface().clone())
                else {
                    return;
                };
                let assigned = icon.as_ref().and_then(|resource| {
                    if resource.client().map(|c| c.id()) != toplevel.client().map(|c| c.id()) {
                        return None;
                    }
                    let mut data = resource.data::<IconData>()?.0.lock().ok()?;
                    data.immutable = true;
                    Some(Assigned {
                        name: data.name.clone(),
                        raster: data.raster.clone(),
                    })
                });
                if state.window_icons.pending.contains_key(&surface)
                    || state.window_icons.pending.len() < MAX_ICONS
                {
                    state.window_icons.pending.insert(surface, assigned);
                }
            }
            _ => {}
        }
    }
}
impl Dispatch<XdgToplevelIconV1, IconData> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        resource: &XdgToplevelIconV1,
        request: icon::Request,
        data: &IconData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let mut data = data.0.lock().unwrap();
        if !matches!(request, icon::Request::Destroy) && data.immutable {
            resource.post_error(icon::Error::Immutable, "assigned icon is immutable");
            return;
        }
        match request {
            icon::Request::SetName { icon_name } => {
                data.name = name_valid(&icon_name).then_some(icon_name);
            }
            icon::Request::AddBuffer { buffer, scale } => {
                if data.buffers.len() >= 32 {
                    resource.post_error(icon::Error::InvalidBuffer, "too many icon buffers");
                    return;
                }
                if let Some(raster) = copy_shm(&buffer, scale) {
                    if data.raster.as_ref().is_none_or(|r| raster.width >= r.width) {
                        data.raster = Some(raster);
                    }
                    data.buffers.push(buffer);
                } else {
                    resource.post_error(
                        icon::Error::InvalidBuffer,
                        "icon must be bounded square ARGB/XRGB SHM",
                    );
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn theme_names_cannot_escape_cache_or_load_client_paths() {
        for name in ["/etc/passwd", "../icon", "a/b", "..", "a\n"] {
            assert!(!name_valid(name));
        }
        assert!(name_valid("utilities-terminal"));
    }
    #[test]
    fn cache_reuses_content_and_bounds_versions() {
        let display = smithay::reexports::wayland_server::Display::<State>::new().unwrap();
        let mut cache = WindowIcons::new(&display.handle());
        let raster = Raster {
            width: 1,
            rgba: vec![0, 0, 255, 255],
        };
        let path = cache.cache("test".into(), raster.clone()).unwrap();
        assert_eq!(cache.cache("test".into(), raster), Some(path.clone()));
        let pixels = image::open(&path).unwrap().to_rgba8();
        assert_eq!(pixels.as_raw(), &[0, 0, 255, 255]);
        for n in 1..4 {
            cache.cache(
                "test".into(),
                Raster {
                    width: 1,
                    rgba: vec![n, 0, 0, 255],
                },
            );
        }
        assert!(!std::path::Path::new(&path).exists());
        assert_eq!(
            std::fs::read_dir(cache.directory.path()).unwrap().count(),
            2
        );
        cache.remove_cache("test");
        assert_eq!(
            std::fs::read_dir(cache.directory.path()).unwrap().count(),
            0
        );
    }
}
