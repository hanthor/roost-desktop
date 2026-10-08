//! Native color parsing and versioned wallpaper publication; no GTK initialization.

/// Build strict placement/shading metadata without allowing an invalid
/// underlay color to suppress a valid wallpaper URI. GNOME Shell continues
/// loading its image after a failed pattern-color parse. Black is the
/// deterministic RGB underlay fallback; this does not make malformed CSS valid.
pub(super) fn picture_settings(
    placement: &str,
    shading: &str,
    primary: &str,
    secondary: &str,
) -> Option<roost_shell_control::background::PictureSettings> {
    use roost_shell_control::background::{PictureSettings, Placement, Shading};
    let color = |text: &str| {
        gtk4::gdk::RGBA::parse(text)
            .map(|rgb| {
                [rgb.red(), rgb.green(), rgb.blue()]
                    .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
            })
            .unwrap_or([0, 0, 0])
    };
    Some(PictureSettings {
        placement: Placement::from_key(placement)?,
        shading: Shading::from_key(shading)?,
        primary: color(primary),
        secondary: color(secondary),
    })
}

/// The complete publisher wire payload, shared with regression tests. Typed
/// metadata stays versioned; unknown placement/shading still fails upstream.
pub(super) fn append_metadata(
    mut text: String,
    lock_uri: &str,
    desktop: roost_shell_control::background::PictureSettings,
    lock: roost_shell_control::background::PictureSettings,
) -> Result<String, serde_json::Error> {
    text.push_str(lock_uri);
    text.push('\n');
    text.push_str(&serde_json::to_string(
        &roost_shell_control::background::BackgroundMetadata {
            version: 1,
            desktop,
            lock,
        },
    )?);
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod background_metadata_tests {
    use super::{append_metadata, picture_settings};
    use crate::logic::wallpaper_drop;
    use roost_shell_control::background::BackgroundMetadata;

    #[test]
    fn invalid_primary_or_secondary_never_suppresses_desktop_dark_or_lock_uri() {
        for (primary, secondary) in [("000000", "#ffffff"), ("#123456", "FFFFFF")] {
            let desktop = picture_settings("zoom", "solid", primary, secondary).unwrap();
            let lock = picture_settings("scaled", "horizontal", secondary, primary).unwrap();
            for dark in [false, true] {
                let text = append_metadata(
                    wallpaper_drop(
                        "file:///valid-light.jpg",
                        "file:///valid-dark.jpg",
                        dark,
                        "zoom",
                        primary,
                        "blue",
                    ),
                    "file:///valid-lock.jpg",
                    desktop,
                    lock,
                )
                .unwrap();
                let lines: Vec<_> = text.lines().collect();
                assert_eq!(lines.len(), 5);
                assert_eq!(
                    lines[0],
                    if dark {
                        "file:///valid-dark.jpg"
                    } else {
                        "file:///valid-light.jpg"
                    }
                );
                assert_eq!(lines[3], "file:///valid-lock.jpg");
                let metadata: BackgroundMetadata = serde_json::from_str(lines[4]).unwrap();
                assert_eq!(metadata.version, 1);
                assert_eq!(metadata.desktop, desktop);
                assert_eq!(metadata.lock, lock);
                if primary == "000000" {
                    assert_eq!(desktop.primary, [0, 0, 0]);
                }
                if secondary == "FFFFFF" {
                    assert_eq!(desktop.secondary, [0, 0, 0]);
                }
            }
        }
    }

    #[test]
    fn valid_color_channels_and_metadata_enums_remain_exact() {
        let actual =
            picture_settings("spanned", "vertical", "#123456", "rgb(200, 10, 20)").unwrap();
        assert_eq!(actual.primary, [0x12, 0x34, 0x56]);
        assert_eq!(actual.secondary, [200, 10, 20]);
        assert!(picture_settings("unknown", "solid", "000000", "FFFFFF").is_none());
        assert!(picture_settings("zoom", "unknown", "000000", "FFFFFF").is_none());
    }
}
