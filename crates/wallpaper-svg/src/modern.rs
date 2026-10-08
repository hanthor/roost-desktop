//! Actual libglycin-2 C API used by GNOME Shell 51. Dynamic discovery keeps
//! old GdkPixbuf test environments usable without substituting modern loaders.
use glib::{object::ObjectType, translate::*};
use std::{ffi::CStr, ptr};

pub struct Decoded {
    pub pixbuf: gdk_pixbuf::Pixbuf,
    // Keep the real loader/image/frame alive through the backend receipt. Their
    // owned Glycin subprocess remains identifiable while its resources hash.
    pub _objects: Vec<glib::Object>,
}

pub fn decode(bytes: &[u8]) -> Result<Option<Decoded>, Box<dyn std::error::Error>> {
    type New = unsafe extern "C" fn(*mut gio::ffi::GInputStream) -> *mut glib::gobject_ffi::GObject;
    type Formats = unsafe extern "C" fn(*mut glib::gobject_ffi::GObject, u32);
    type Load = unsafe extern "C" fn(
        *mut glib::gobject_ffi::GObject,
        *mut *mut glib::ffi::GError,
    ) -> *mut glib::gobject_ffi::GObject;
    type Dimension = unsafe extern "C" fn(*mut glib::gobject_ffi::GObject) -> u32;
    type Format = unsafe extern "C" fn(*mut glib::gobject_ffi::GObject) -> i32;
    type Bytes = unsafe extern "C" fn(*mut glib::gobject_ffi::GObject) -> *mut glib::ffi::GBytes;
    type Mime = unsafe extern "C" fn(*mut glib::gobject_ffi::GObject) -> *const libc::c_char;
    // SAFETY: symbols have the public libglycin-2 ABI, verified against its
    // actual primary C implementation. The library stays mapped until this
    // single-use process exits; all returned GObjects/bytes retain ownership.
    unsafe {
        let library = libc::dlopen(
            c"libglycin-2.so.0".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        );
        if library.is_null() {
            // A modern GdkPixbuf must not silently switch to the old module.
            let minor =
                libc::dlsym(libc::RTLD_DEFAULT, c"gdk_pixbuf_minor_version".as_ptr()).cast::<u32>();
            if minor.is_null() || *minor >= 44 {
                return Err("actual modern Glycin library missing".into());
            }
            return Ok(None);
        }
        macro_rules! symbol {
            ($name:literal, $ty:ty) => {{
                let pointer = libc::dlsym(
                    library,
                    CStr::from_bytes_with_nul(concat!($name, "\0").as_bytes())?.as_ptr(),
                );
                if pointer.is_null() {
                    return Err(concat!("actual Glycin API missing: ", $name).into());
                }
                std::mem::transmute::<*mut libc::c_void, $ty>(pointer)
            }};
        }
        let new = symbol!("gly_loader_new_for_stream", New);
        let formats = symbol!("gly_loader_set_accepted_memory_formats", Formats);
        let load = symbol!("gly_loader_load", Load);
        let next = symbol!("gly_image_next_frame", Load);
        let image_width = symbol!("gly_image_get_width", Dimension);
        let image_height = symbol!("gly_image_get_height", Dimension);
        let frame_width = symbol!("gly_frame_get_width", Dimension);
        let frame_height = symbol!("gly_frame_get_height", Dimension);
        let stride = symbol!("gly_frame_get_stride", Dimension);
        let format = symbol!("gly_frame_get_memory_format", Format);
        let buffer = symbol!("gly_frame_get_buf_bytes", Bytes);
        let mime = symbol!("gly_image_get_mime_type", Mime);
        let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes.to_vec()));
        let loader = object(new(stream.as_ptr().cast()), ptr::null_mut())?;
        // Public MemoryFormatSelection.R8G8B8A8 bit5; Glycin performs the
        // actual native conversion from librsvg's premultiplied Cairo pixels.
        formats(loader.as_ptr(), 1 << 5);
        let mut error = ptr::null_mut();
        let raw = load(loader.as_ptr(), &mut error);
        let image = object(raw, error)?;
        let mime = mime(image.as_ptr());
        if mime.is_null()
            || !matches!(
                CStr::from_ptr(mime).to_bytes(),
                b"image/svg+xml" | b"image/svg+xml-compressed"
            )
        {
            return Err("actual input is not SVG".into());
        }
        let w = image_width(image.as_ptr());
        let h = image_height(image.as_ptr());
        if w == 0 || h == 0 || w as u64 * h as u64 > super::MAX_PIXELS {
            return Err("intrinsic dimension bound".into());
        }
        let mut error = ptr::null_mut();
        let raw = next(image.as_ptr(), &mut error);
        let frame = object(raw, error)?;
        let rowstride = stride(frame.as_ptr());
        if frame_width(frame.as_ptr()) != w
            || frame_height(frame.as_ptr()) != h
            || format(frame.as_ptr()) != 5
            || rowstride < w.checked_mul(4).ok_or("stride overflow")?
            || rowstride > i32::MAX as u32
        {
            return Err("invalid actual Glycin RGBA layout".into());
        }
        let raw = buffer(frame.as_ptr());
        if raw.is_null() {
            return Err("missing actual Glycin pixels".into());
        }
        let bytes: glib::Bytes = from_glib_none(raw);
        let end = (h as u64 - 1) * rowstride as u64 + w as u64 * 4;
        if end > bytes.len() as u64 {
            return Err("short actual Glycin pixel buffer".into());
        }
        let pixbuf = gdk_pixbuf::Pixbuf::from_bytes(
            &bytes,
            gdk_pixbuf::Colorspace::Rgb,
            true,
            8,
            w as i32,
            h as i32,
            rowstride as i32,
        );
        Ok(Some(Decoded {
            pixbuf,
            _objects: vec![loader, image, frame],
        }))
    }
}

unsafe fn object(
    raw: *mut glib::gobject_ffi::GObject,
    error: *mut glib::ffi::GError,
) -> Result<glib::Object, Box<dyn std::error::Error>> {
    if !error.is_null() {
        return Err(unsafe { glib::Error::from_glib_full(error) }.into());
    }
    if raw.is_null() {
        return Err("actual Glycin returned no object".into());
    }
    Ok(unsafe { glib::Object::from_glib_full(raw) })
}
