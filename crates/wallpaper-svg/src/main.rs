//! The compositor never initializes GdkPixbuf, librsvg or FontConfig. This
//! single-use process decodes a stream without a base URI, like GNOME's loader.
mod modern;
mod provenance;

use gdk_pixbuf::prelude::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

const MAX_INPUT: u64 = 64 * 1024 * 1024;
const MAX_PIXELS: u64 = 7680 * 4320;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!(
            "roost-wallpaper-svg {}",
            option_env!("ROOST_VERSION")
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .trim_start_matches('v')
        );
        return;
    }
    if let Err(error) = run() {
        eprintln!("roost-wallpaper-svg: {error}");
        std::process::exit(1);
    }
}

fn limits() -> Result<(), Box<dyn std::error::Error>> {
    // Bind the helper to its caller. Actual Glycin/bwrap binds its monitor
    // and PID-namespace child with --die-with-parent; other native paths use
    // PDEATHSIG. Killing our helper tears down those owned descendants too.
    let parent = unsafe { libc::getppid() };
    if parent == 1
        || unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0
        || unsafe { libc::getppid() } != parent
    {
        return Err("caller lifecycle unavailable".into());
    }
    for (resource, soft, hard) in [
        (libc::RLIMIT_AS, 1024 * 1024 * 1024, 1024 * 1024 * 1024),
        (libc::RLIMIT_CPU, 4, 5),
        (libc::RLIMIT_CORE, 0, 0),
        (libc::RLIMIT_NOFILE, 128, 128),
    ] {
        let limit = libc::rlimit {
            rlim_cur: soft,
            rlim_max: hard,
        };
        // SAFETY: valid resource and initialized rlimit; only this process.
        if unsafe { libc::setrlimit(resource, &limit) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    limits()?;
    let argument = std::env::args().nth(1);
    let identity = matches!(argument.as_deref(), Some("--identity" | "--receipt"));
    let mut bytes = Vec::new();
    if identity {
        // Exercise the actual font/text path so the receipt includes its loaded
        // libraries. No base URI is supplied, even for the identity probe.
        bytes.extend_from_slice(br#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><text x="1" y="24">M</text></svg>"#);
    } else {
        std::io::stdin()
            .take(MAX_INPUT + 1)
            .read_to_end(&mut bytes)?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_INPUT {
            return Err("input bound".into());
        }
    }
    let decoded = modern::decode(&bytes)?;
    let pixbuf = if let Some(decoded) = decoded.as_ref() {
        decoded.pixbuf.clone()
    } else {
        let loader = gdk_pixbuf::PixbufLoader::with_type("svg")?;
        loader.connect_size_prepared(|_, w, h| {
            // This signal precedes allocation. Never silently resize a valid SVG.
            if w <= 0 || h <= 0 || w as u64 * h as u64 > MAX_PIXELS {
                eprintln!("roost-wallpaper-svg: intrinsic dimension bound");
                std::process::exit(1);
            }
        });
        loader.write(&bytes)?;
        loader.close()?;
        loader.pixbuf().ok_or("missing pixels")?
    };
    if identity {
        let receipt = backend_identity(decoded.is_some())?;
        if argument.as_deref() == Some("--receipt") {
            println!("{receipt}");
        } else {
            println!(
                "{}",
                receipt["sha256"].as_str().ok_or("identity unavailable")?
            );
        }
        return Ok(());
    }
    let (w, h, stride, channels) = (
        pixbuf.width(),
        pixbuf.height(),
        pixbuf.rowstride(),
        pixbuf.n_channels(),
    );
    if w <= 0
        || h <= 0
        || w as u64 * h as u64 > MAX_PIXELS
        || pixbuf.colorspace() != gdk_pixbuf::Colorspace::Rgb
        || pixbuf.has_alpha() != (channels == 4)
        || pixbuf.bits_per_sample() != 8
        || !matches!(channels, 3 | 4)
        || stride < w.checked_mul(channels).ok_or("stride overflow")?
    {
        return Err("invalid pixel layout".into());
    }
    let pixels = pixbuf.read_pixel_bytes();
    let pixels = pixels.as_ref();
    let end = (h as usize - 1)
        .checked_mul(stride as usize)
        .and_then(|v| v.checked_add(w as usize * channels as usize))
        .ok_or("length overflow")?;
    if pixels.len() < end {
        return Err("short pixel buffer".into());
    }
    let mut output = std::io::stdout().lock();
    output.write_all(b"RSVG0001")?;
    output.write_all(&(w as u32).to_le_bytes())?;
    output.write_all(&(h as u32).to_le_bytes())?;
    for row in 0..h as usize {
        let start = row * stride as usize;
        let row = &pixels[start..start + w as usize * channels as usize];
        if channels == 4 {
            output.write_all(row)?;
        } else {
            for pixel in row.chunks_exact(3) {
                output.write_all(&[pixel[0], pixel[1], pixel[2], 255])?;
            }
        }
    }
    Ok(())
}

#[repr(C)]
struct FontSet {
    nfont: libc::c_int,
    sfont: libc::c_int,
    fonts: *mut *mut libc::c_void,
}

#[link(name = "fontconfig")]
unsafe extern "C" {
    fn FcInitLoadConfigAndFonts() -> *mut libc::c_void;
    fn FcConfigDestroy(config: *mut libc::c_void);
    fn FcConfigGetConfigFiles(config: *mut libc::c_void) -> *mut libc::c_void;
    fn FcStrListNext(list: *mut libc::c_void) -> *const libc::c_uchar;
    fn FcStrListDone(list: *mut libc::c_void);
    fn FcPatternCreate() -> *mut libc::c_void;
    fn FcPatternDestroy(pattern: *mut libc::c_void);
    fn FcObjectSetBuild(first: *const libc::c_char, ...) -> *mut libc::c_void;
    fn FcObjectSetDestroy(set: *mut libc::c_void);
    fn FcFontList(
        config: *mut libc::c_void,
        pattern: *mut libc::c_void,
        objects: *mut libc::c_void,
    ) -> *mut FontSet;
    fn FcFontSetDestroy(set: *mut FontSet);
    fn FcPatternGetString(
        pattern: *mut libc::c_void,
        object: *const libc::c_char,
        index: libc::c_int,
        value: *mut *const libc::c_uchar,
    ) -> libc::c_int;
}

fn backend_identity(modern: bool) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    use std::{collections::BTreeSet, ffi::CStr, os::unix::ffi::OsStrExt};
    let mut paths = BTreeSet::new();
    paths.insert(std::env::current_exe()?);
    let processors = provenance::observe(&mut paths, modern)?;
    // Actual mapped helper/loader/native libraries, including the dynamically
    // selected SVG module. The compositor observes this receipt off-thread.
    for line in std::fs::read_to_string("/proc/self/maps")?.lines() {
        if let Some((_, path)) = line.split_once('/') {
            let path = std::path::PathBuf::from(format!("/{path}"));
            if path.is_file() {
                paths.insert(path);
            }
        }
    }
    // SAFETY: the ABI is FontConfig's public FcFontSet layout. Returned lists
    // and patterns live until their matching destroy calls below. Strings are
    // copied before destruction. This short-lived process is resource bounded.
    unsafe {
        let config = FcInitLoadConfigAndFonts();
        if config.is_null() {
            return Err("FontConfig initialization failed".into());
        }
        let files = FcConfigGetConfigFiles(config);
        if files.is_null() {
            return Err("FontConfig files unavailable".into());
        }
        loop {
            let value = FcStrListNext(files);
            if value.is_null() {
                break;
            }
            paths.insert(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
                CStr::from_ptr(value.cast()).to_bytes(),
            )));
            if paths.len() > 16384 {
                return Err("font resource count bound".into());
            }
        }
        FcStrListDone(files);
        let pattern = FcPatternCreate();
        let objects = FcObjectSetBuild(c"file".as_ptr(), std::ptr::null::<libc::c_char>());
        if pattern.is_null() || objects.is_null() {
            return Err("FontConfig query unavailable".into());
        }
        let fonts = FcFontList(config, pattern, objects);
        if fonts.is_null()
            || (*fonts).nfont < 0
            || (*fonts).nfont > 16384
            || ((*fonts).nfont > 0 && (*fonts).fonts.is_null())
        {
            return Err("font resource count bound".into());
        }
        for i in 0..(*fonts).nfont as usize {
            let mut value = std::ptr::null();
            if FcPatternGetString(*(*fonts).fonts.add(i), c"file".as_ptr(), 0, &mut value) != 0
                || value.is_null()
            {
                return Err("font file identity unavailable".into());
            }
            paths.insert(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
                CStr::from_ptr(value.cast()).to_bytes(),
            )));
        }
        FcFontSetDestroy(fonts);
        FcObjectSetDestroy(objects);
        FcPatternDestroy(pattern);
        FcConfigDestroy(config);
    }
    let mut hash = Sha256::new();
    let mut resources = Vec::new();
    hash.update(b"roost-svg-stream-v1");
    for path in paths {
        let file = std::fs::File::open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_INPUT {
            return Err("backend file bound".into());
        }
        hash.update((path.as_os_str().as_bytes().len() as u64).to_le_bytes());
        hash.update(path.as_os_str().as_bytes());
        hash.update(metadata.len().to_le_bytes());
        let mut file_hash = Sha256::new();
        let mut reader = file.take(MAX_INPUT + 1);
        let mut buffer = [0; 16384];
        let mut length = 0;
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            length += count as u64;
            if length > MAX_INPUT {
                return Err("backend file bound".into());
            }
            hash.update(&buffer[..count]);
            file_hash.update(&buffer[..count]);
        }
        let after = reader.get_ref().metadata()?;
        use std::os::unix::fs::MetadataExt;
        if length != metadata.len()
            || (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ) != (
                after.dev(),
                after.ino(),
                after.len(),
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec(),
            )
        {
            return Err("backend file changed".into());
        }
        resources.push(serde_json::json!({"path":path.to_string_lossy(),"bytes":length,"sha256":format!("{:x}",file_hash.finalize())}));
    }
    Ok(
        serde_json::json!({"kind":"actual no-base SVG stream loader, owned processors, mapped libraries and FontConfig resources", "sha256":format!("{:x}",hash.finalize()), "resources":resources,"processors":processors,"modern_glycin":modern}),
    )
}
