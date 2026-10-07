//! CI exercises the real built helper and installed SVG/font loader. These
//! small intrinsic pixel contracts do not qualify Settings/native/shipping.
use std::io::Write;

fn helper(bytes: &[u8]) -> std::process::Output {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_roost-wallpaper-svg"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    child.wait_with_output().unwrap()
}

fn rgba(bytes: &[u8], width: u32, height: u32) -> Vec<u8> {
    let output = helper(bytes);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(&output.stdout[..8], b"RSVG0001");
    assert_eq!(&output.stdout[8..12], &width.to_le_bytes());
    assert_eq!(&output.stdout[12..16], &height.to_le_bytes());
    assert_eq!(
        output.stdout.len(),
        16 + width as usize * height as usize * 4
    );
    output.stdout[16..].to_vec()
}

#[test]
fn actual_svg_loader_intrinsic_viewbox_css_internal_use_and_alpha() {
    for size in ["width='20' height='10'", "viewBox='0 0 20 10'"] {
        let svg = format!("<svg xmlns='http://www.w3.org/2000/svg' {size}><style>.paint {{ fill:rgb(25,87,200) }}</style><defs><rect id='r' width='20' height='10' class='paint'/></defs><use href='#r'/></svg>");
        let pixels = rgba(svg.as_bytes(), 20, 10);
        assert!(pixels.chunks_exact(4).all(|p| p == [25, 87, 200, 255]));
    }
    let pixels = rgba(b"<svg xmlns='http://www.w3.org/2000/svg' width='2' height='1'><rect width='1' height='1' fill='#c80a14' fill-opacity='0.5'/></svg>",2,1);
    assert_eq!(&pixels[4..], &[0, 0, 0, 0]);
    assert!(pixels[3] == 127 || pixels[3] == 128);
    // Cairo's premultiplied storage is converted by the actual loader to
    // straight RGBA. Allow its unavoidable one-byte unpremultiplication error.
    assert!((pixels[0] as i16 - 200).abs() <= 1);
    assert!((pixels[1] as i16 - 10).abs() <= 1);
    assert!((pixels[2] as i16 - 20).abs() <= 1);
}

#[test]
fn actual_svg_loader_text_uses_native_fonts_and_stream_has_no_base_uri() {
    let pixels = rgba(b"<svg xmlns='http://www.w3.org/2000/svg' width='120' height='32'><text x='2' y='24' font-family='sans-serif' font-size='22'>Roost</text></svg>",120,32);
    assert!(pixels.chunks_exact(4).filter(|p| p[3] != 0).count() > 100);
    let pixels = rgba(b"<svg xmlns='http://www.w3.org/2000/svg' width='2' height='1'><image href='file:///etc/passwd' width='2' height='1'/></svg>",2,1);
    assert!(pixels.iter().all(|v| *v == 0));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roost-wallpaper-svg"))
        .arg("--identity")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout.len(), 65);
    assert!(output.stdout[..64].iter().all(u8::is_ascii_hexdigit));
}

#[test]
fn actual_svg_helper_rejects_malformed_and_excessive_intrinsic_dimensions() {
    for bytes in [
        b"<svg".as_slice(),
        b"<svg xmlns='http://www.w3.org/2000/svg' width='90000' height='90000'/>".as_slice(),
    ] {
        let output = helper(bytes);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}
