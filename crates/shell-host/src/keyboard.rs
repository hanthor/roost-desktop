//! Overview keyboard input (002 search): an xkb keymap feed turning
//! server key events into search text plus submit/dismiss actions.
//!
//! [`XkbFeed`] owns the xkb state built from the compositor's keymap.
//! Wayland keycodes are evdev; xkb numbers from 8, so every feed
//! translates by [`XKB_EVDEV_OFFSET`]. Modifiers always update state
//! (a shifted letter must survive even when the overview is closed);
//! text and submit/dismiss are the caller's to gate on the overview.

use xkbcommon::xkb::{
    keysyms::{KEY_BackSpace, KEY_Escape, KEY_KP_Enter, KEY_Return},
    Context, KeyDirection, Keycode, Keymap, State, KEYMAP_COMPILE_NO_FLAGS, KEYMAP_FORMAT_TEXT_V1,
};

/// Evdev-to-xkb keycode offset (xkb numbers keys from 8).
pub const XKB_EVDEV_OFFSET: u32 = 8;

/// What one key press means for the overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// Printable text for the search buffer.
    Text(String),
    /// Delete the last buffer character.
    Erase,
    /// Activate the top hit.
    Submit,
    /// Leave the overview.
    Dismiss,
    /// Modifier, release, or non-text key: state updated, nothing to do.
    None,
}

/// xkb state behind one keyboard. Rebuilt whenever the server sends a
/// new keymap; unusable (all [`KeyAction::None`]) only before the
/// first keymap arrives.
pub struct XkbFeed {
    _context: Context,
    _keymap: Keymap,
    state: State,
}

impl XkbFeed {
    /// Feed over a names-built keymap (tests and fallback).
    pub fn from_names(
        rules: &str,
        model: &str,
        layout: &str,
        variant: &str,
        options: Option<String>,
    ) -> Option<Self> {
        let context = Context::new(0);
        let keymap = Keymap::new_from_names(
            &context,
            rules,
            model,
            layout,
            variant,
            options,
            KEYMAP_COMPILE_NO_FLAGS,
        )?;
        Some(Self::from_parts(context, keymap))
    }

    /// Feed over a server keymap string (the keymap event payload).
    pub fn from_string(keymap: String) -> Option<Self> {
        let context = Context::new(0);
        let keymap = Keymap::new_from_string(
            &context,
            keymap,
            KEYMAP_FORMAT_TEXT_V1,
            KEYMAP_COMPILE_NO_FLAGS,
        )?;
        Some(Self::from_parts(context, keymap))
    }

    /// Feed over a server keymap fd (borrowed: the Wayland message
    /// owns the fd and closes it after dispatch). A short read or
    /// non-UTF8 payload refuses the keymap instead of guessing a
    /// layout.
    pub fn from_fd(fd: &std::os::fd::OwnedFd, size: u32) -> Option<Self> {
        if size == 0 || size > 1 << 20 {
            return None;
        }
        let mut buf = Vec::with_capacity(size as usize);
        let mut chunk = [0u8; 4096];
        loop {
            match rustix::io::read(fd, &mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(rustix::io::Errno::INTR) => continue,
                Err(_) => return None,
            }
        }
        String::from_utf8(buf).ok().and_then(Self::from_string)
    }

    fn from_parts(context: Context, keymap: Keymap) -> Self {
        let state = State::new(&keymap);
        Self {
            _context: context,
            _keymap: keymap,
            state,
        }
    }

    /// Apply a server modifiers event (depressed/latched/locked mods
    /// plus the effective layout group).
    #[allow(clippy::too_many_arguments)]
    pub fn update_mask(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        self.state
            .update_mask(depressed, latched, locked, 0, 0, group);
    }

    /// Feed one key event (evdev code from the server). Releases only
    /// update state; presses resolve to an action.
    pub fn key(&mut self, evdev: u32, pressed: bool) -> KeyAction {
        let code = Keycode::new(evdev.wrapping_add(XKB_EVDEV_OFFSET));
        self.state.update_key(
            code,
            if pressed {
                KeyDirection::Down
            } else {
                KeyDirection::Up
            },
        );
        if !pressed {
            return KeyAction::None;
        }
        // Upstream keysym names are not upper-case; allow the lint
        // locally instead of renaming protocol constants.
        match self.state.key_get_one_sym(code).raw() {
            #[allow(non_upper_case_globals)]
            KEY_BackSpace => KeyAction::Erase,
            #[allow(non_upper_case_globals)]
            KEY_Return | KEY_KP_Enter => KeyAction::Submit,
            #[allow(non_upper_case_globals)]
            KEY_Escape => KeyAction::Dismiss,
            _ => {
                let text = self.state.key_get_utf8(code);
                if text.is_empty() {
                    KeyAction::None
                } else {
                    KeyAction::Text(text)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Evdev codes on a `us` layout (xkb hotels the +8 internally).
    const EV_A: u32 = 30;
    const EV_SHIFT_LEFT: u32 = 42;
    const EV_BACKSPACE: u32 = 14;
    const EV_RETURN: u32 = 28;
    const EV_ESCAPE: u32 = 1;
    const EV_SPACE: u32 = 57;

    fn us_feed() -> XkbFeed {
        XkbFeed::from_names("evdev", "pc105", "us", "", None)
            .expect("us keymap compiles where xkb-data exists")
    }

    #[test]
    fn letters_type_shift_cases_and_space() {
        let mut feed = us_feed();
        assert_eq!(feed.key(EV_A, true), KeyAction::Text("a".to_owned()));
        assert_eq!(feed.key(EV_A, false), KeyAction::None);
        feed.key(EV_SHIFT_LEFT, true);
        assert_eq!(feed.key(EV_A, true), KeyAction::Text("A".to_owned()));
        feed.key(EV_A, false);
        feed.key(EV_SHIFT_LEFT, false);
        assert_eq!(feed.key(EV_SPACE, true), KeyAction::Text(" ".to_owned()));
    }

    #[test]
    fn editing_and_command_keys_resolve() {
        let mut feed = us_feed();
        assert_eq!(feed.key(EV_BACKSPACE, true), KeyAction::Erase);
        assert_eq!(feed.key(EV_RETURN, true), KeyAction::Submit);
        assert_eq!(feed.key(EV_ESCAPE, true), KeyAction::Dismiss);
        // Shift alone is a modifier: state updates, no action.
        assert_eq!(feed.key(EV_SHIFT_LEFT, true), KeyAction::None);
        assert_eq!(feed.key(EV_SHIFT_LEFT, false), KeyAction::None);
    }

    #[test]
    fn garbage_keymap_string_refuses() {
        assert!(XkbFeed::from_string("not a keymap".to_owned()).is_none());
    }
}
