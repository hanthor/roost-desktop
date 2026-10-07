//! Bounded transport to the separately packaged GNOME SVG loader. Call only
//! from decoding/identity workers; FontConfig and all filesystem work stay in
//! the helper process. No GdkPixbuf/GTK dependency enters the compositor.
use std::{
    io::{Read, Write},
    os::fd::AsRawFd,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const MAX_PIXELS: u64 = 7680 * 4320;
const MAX_INPUT: usize = 64 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);

fn helper_path() -> Option<std::path::PathBuf> {
    let executable = std::env::current_exe().ok()?;
    Some(executable.parent()?.join("roost-wallpaper-svg"))
}

/// Receipt of the actual mapped SVG/native font libraries and configured font
/// files, computed in a bounded process. Worker calls share a one-second
/// observation; never invoke this from a render/frame callback.
pub fn backend_identity() -> Option<String> {
    static OBSERVATION: std::sync::OnceLock<std::sync::Mutex<Option<(Instant, Option<String>)>>> =
        std::sync::OnceLock::new();
    let mut observation = OBSERVATION.get_or_init(Default::default).lock().ok()?;
    if let Some((at, value)) = &*observation {
        if at.elapsed() < Duration::from_secs(1) {
            return value.clone();
        }
    }
    let value = exchange(Some("--identity"), &[], 65).and_then(|bytes| {
        let value = std::str::from_utf8(&bytes).ok()?.strip_suffix('\n')?;
        (value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit()))
            .then(|| value.to_owned())
    });
    *observation = Some((Instant::now(), value.clone()));
    value
}

pub fn decode(bytes: &[u8]) -> Option<image::DynamicImage> {
    let output = exchange(None, bytes, (MAX_PIXELS * 4 + 16) as usize)?;
    parse_output(&output)
}

fn parse_output(output: &[u8]) -> Option<image::DynamicImage> {
    if output.get(..8)? != b"RSVG0001" {
        return None;
    }
    let w = u32::from_le_bytes(output.get(8..12)?.try_into().ok()?);
    let h = u32::from_le_bytes(output.get(12..16)?.try_into().ok()?);
    let area = w as u64 * h as u64;
    if area == 0 || area > MAX_PIXELS || output.len() as u64 != 16 + area * 4 {
        return None;
    }
    image::RgbaImage::from_raw(w, h, output[16..].to_vec()).map(image::DynamicImage::ImageRgba8)
}

fn nonblocking(fd: impl AsRawFd) -> Option<()> {
    // SAFETY: live owned pipe descriptor, flags queried before modification.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return None;
        }
    }
    Some(())
}

fn exchange(argument: Option<&str>, bytes: &[u8], maximum: usize) -> Option<Vec<u8>> {
    if bytes.len() > MAX_INPUT {
        return None;
    }
    let mut command = Command::new(helper_path()?);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(argument) = argument {
        command.arg(argument);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("roost-wallpaper: GNOME SVG helper unavailable: {error}");
            return None;
        }
    };
    // On every exit (including pipe/setup errors), kill and reap the helper.
    // Nonblocking pipes enforce the wall timeout during BOTH input and output,
    // without accumulating extra blocking transport threads behind old jobs.
    let result = (|| {
        let mut input = Some(child.stdin.take()?);
        let mut output = child.stdout.take()?;
        nonblocking(input.as_ref()?)?;
        nonblocking(&output)?;
        let started = Instant::now();
        let mut sent = 0;
        let mut received = Vec::new();
        let mut eof = false;
        let mut buffer = [0; 65536];
        loop {
            if started.elapsed() >= TIMEOUT {
                eprintln!("roost-wallpaper: GNOME SVG helper wall-time bound exceeded");
                return None;
            }
            if let Some(pipe) = input.as_mut() {
                if sent == bytes.len() {
                    input.take();
                } else {
                    match pipe.write(&bytes[sent..]) {
                        Ok(0) => return None,
                        Ok(count) => sent += count,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => return None,
                    }
                }
            }
            // Read available output each iteration so a full pipe cannot
            // deadlock a completed decoder. Check the bound before allocation.
            loop {
                match output.read(&mut buffer) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(count) => {
                        if received.len().checked_add(count)? > maximum {
                            return None;
                        }
                        received.extend_from_slice(&buffer[..count]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return None,
                }
            }
            if let Some(status) = child.try_wait().ok()? {
                if !status.success() {
                    eprintln!("roost-wallpaper: GNOME SVG helper rejected input ({status})");
                    return None;
                }
                if eof && sent == bytes.len() {
                    return Some(received);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_requires_exact_bounded_intrinsic_rgba() {
        let mut output = b"RSVG0001".to_vec();
        output.extend_from_slice(&2u32.to_le_bytes());
        output.extend_from_slice(&1u32.to_le_bytes());
        output.extend_from_slice(&[25, 87, 200, 255, 200, 10, 20, 128]);
        let image = parse_output(&output).unwrap().to_rgba8();
        assert_eq!(image.dimensions(), (2, 1));
        assert_eq!(image.as_raw(), &output[16..]);
        assert!(parse_output(&output[..output.len() - 1]).is_none());
        output.push(0);
        assert!(parse_output(&output).is_none());
        output[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_output(&output).is_none());
        output[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert!(parse_output(&output).is_none());
    }
}
