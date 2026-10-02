//! Screenshots (#61): GNOME Shell's `org.gnome.Shell.Screenshot`.
//!
//! xdg-desktop-portal-gnome's Screenshot portal (and the quick-settings
//! button) call this interface, so screenshots work the GNOME way with
//! the stock portal, as niri does it. The D-Bus side runs on its own
//! thread and hands each request to the event loop over a calloop
//! channel; the loop renders the scene offscreen, saves a PNG, and
//! answers. In the nested preview the host's GNOME Shell owns the name,
//! so this service stays down there.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// Bus name and object GNOME Shell serves.
pub const NAME: &str = "org.gnome.Shell.Screenshot";
pub const PATH: &str = "/org/gnome/Shell/Screenshot";
/// Longest a caller waits for the frame.
const DEADLINE: Duration = Duration::from_secs(10);

/// One request from D-Bus: where to save (empty: GNOME's default).
pub struct Request {
    pub filename: PathBuf,
    pub reply: mpsc::Sender<Option<PathBuf>>,
}

struct Service {
    to_loop: calloop::channel::Sender<Request>,
}

#[zbus::interface(name = "org.gnome.Shell.Screenshot")]
impl Service {
    /// GNOME Shell's signature: `(include_cursor, flash, filename) ->
    /// (success, filename_used)`.
    fn screenshot(
        &self,
        _include_cursor: bool,
        _flash: bool,
        filename: String,
    ) -> zbus::fdo::Result<(bool, String)> {
        let (reply, answer) = mpsc::channel();
        self.to_loop
            .send(Request {
                filename: PathBuf::from(filename),
                reply,
            })
            .map_err(|_| zbus::fdo::Error::Failed("compositor gone".into()))?;
        match answer.recv_timeout(DEADLINE) {
            Ok(Some(path)) => Ok((true, path.to_string_lossy().into_owned())),
            _ => Err(zbus::fdo::Error::Failed("screenshot failed".into())),
        }
    }
}

/// Serve the interface on the session bus from a thread. Returns the
/// receiving end for the event loop; the thread ends quietly when there
/// is no bus or the name is taken (a real GNOME Shell is running).
pub fn start() -> calloop::channel::Channel<Request> {
    let (to_loop, from_dbus) = calloop::channel::channel();
    let _ = std::thread::Builder::new()
        .name("roost-screenshot".into())
        .spawn(move || {
            let conn = match zbus::blocking::connection::Builder::session()
                .and_then(|b| b.serve_at(PATH, Service { to_loop }))
                .and_then(|b| b.build())
            {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("roost-compositor: screenshot service: no session bus: {e}");
                    return;
                }
            };
            let flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
            match conn.request_name_with_flags(NAME, flags) {
                Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                    eprintln!("roost-compositor: screenshot service on {NAME}");
                    // The connection serves from its own executor; keep it.
                    loop {
                        std::thread::park();
                    }
                }
                _ => eprintln!("roost-compositor: {NAME} is taken; screenshot service off"),
            }
        });
    from_dbus
}

/// Where GNOME saves a screenshot: `~/Pictures/Screenshots/Screenshot
/// From YYYY-MM-DD HH-MM-SS.png` (XDG pictures dir when known).
pub fn default_path(now: &jiff_like::Stamp) -> Option<PathBuf> {
    let pictures = std::env::var_os("XDG_PICTURES_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Pictures")))?;
    Some(
        pictures
            .join("Screenshots")
            .join(format!("Screenshot From {}.png", now.0)),
    )
}

/// The requested filename when absolute, else GNOME's default.
pub fn target_path(requested: &Path, now: &jiff_like::Stamp) -> Option<PathBuf> {
    if requested.is_absolute() {
        return Some(requested.to_owned());
    }
    default_path(now)
}

/// A local wall-clock stamp, `YYYY-MM-DD HH-MM-SS` (no time crate here).
pub mod jiff_like {
    pub struct Stamp(pub String);

    impl Stamp {
        /// Now, via `date` for the local zone; UTC seconds as a fallback.
        pub fn now() -> Self {
            let out = std::process::Command::new("date")
                .arg("+%Y-%m-%d %H-%M-%S")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_owned());
            Self(out.unwrap_or_else(|| {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                format!("{secs}")
            }))
        }
    }
}

/// Encode RGBA8 rows (top-down) as a PNG at `path`, creating its dir.
pub fn save_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    writer
        .write_image_data(rgba)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_gnome() {
        let stamp = jiff_like::Stamp("2026-10-01 17-50-00".into());
        assert_eq!(
            target_path(Path::new("/tmp/x.png"), &stamp),
            Some(PathBuf::from("/tmp/x.png"))
        );
        std::env::set_var("XDG_PICTURES_DIR", "/home/u/Pictures");
        assert_eq!(
            target_path(Path::new(""), &stamp),
            Some(PathBuf::from(
                "/home/u/Pictures/Screenshots/Screenshot From 2026-10-01 17-50-00.png"
            ))
        );
    }

    #[test]
    fn png_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b.png");
        let rgba = [255u8, 0, 0, 255, 0, 255, 0, 255];
        save_png(&path, 2, 1, &rgba).unwrap();
        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(&buf[..8], &rgba);
    }
}
