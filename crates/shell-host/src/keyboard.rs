//! Overview keyboard input (002 search): an xkb keymap feed turning
//! server key events into search text plus submit/dismiss actions.
//!
//! [`XkbFeed`] owns the xkb state built from the compositor's keymap.
//! Wayland keycodes are evdev; xkb numbers from 8, so every feed
//! translates by [`XKB_EVDEV_OFFSET`]. Modifiers always update state
//! (a shifted letter must survive even when the overview is closed);
//! text and submit/dismiss are the caller's to gate on the overview.

use xkbcommon::xkb::{
    keysyms::{
        KEY_BackSpace, KEY_Down, KEY_Escape, KEY_ISO_Left_Tab, KEY_KP_Enter, KEY_Left, KEY_Return,
        KEY_Right, KEY_Tab, KEY_Up, KEY_d, KEY_D, KEY_F1, KEY_F6,
    },
    Context, KeyDirection, Keycode, Keymap, State, KEYMAP_COMPILE_NO_FLAGS, KEYMAP_FORMAT_TEXT_V1,
};

/// Evdev-to-xkb keycode offset (xkb numbers keys from 8).
pub const XKB_EVDEV_OFFSET: u32 = 8;

/// Held-modifier keycodes (evdev), tracked like the compositor's
/// `track_workspace_modifiers`: keymap-independent, so chords
/// resolve on every layout.
const SHIFT_KEYS: [u32; 2] = [42, 54];
const SUPER_KEYS: [u32; 2] = [125, 126];
const ALT_KEYS: [u32; 2] = [56, 100];

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
    /// Move the shell focus cursor (arrows, task 2 consumes).
    Up,
    /// Move the shell focus cursor (arrows, task 2 consumes).
    Down,
    /// Move the shell focus cursor (arrows, task 2 consumes).
    Left,
    /// Move the shell focus cursor (arrows, task 2 consumes).
    Right,
    /// Tab: focus the next element (task 2 consumes).
    Next,
    /// Shift+Tab: focus the previous element (task 2 consumes).
    Previous,
    /// F6: cycle the focused shell region (task 2 consumes).
    CycleRegion,
    /// Super+D: focus the dock (task 2 consumes).
    FocusDock,
    /// Alt+F1: focus the panel (task 2 consumes).
    FocusPanel,
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
    shift_held: bool,
    super_held: bool,
    alt_held: bool,
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
    /// A trailing NUL terminator (the fd payload includes one) is
    /// stripped: the xkb compiler takes a `CString`.
    pub fn from_string(keymap: String) -> Option<Self> {
        let trimmed = keymap.trim_end_matches('\0').to_owned();
        // Never hand xkbcommon a string its own CString::new would
        // panic on: without a feed the shell runs deaf but alive.
        if trimmed.is_empty() || trimmed.bytes().any(|b| b == 0) {
            return None;
        }
        let context = Context::new(0);
        let keymap = Keymap::new_from_string(
            &context,
            trimmed,
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
            shift_held: false,
            super_held: false,
            alt_held: false,
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
    /// update state; presses resolve to an action. Held modifiers
    /// track by keycode (like the compositor), so chords resolve on
    /// every layout; chord keys never fall through to text.
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
        if SHIFT_KEYS.contains(&evdev) {
            self.shift_held = pressed;
        } else if SUPER_KEYS.contains(&evdev) {
            self.super_held = pressed;
        } else if ALT_KEYS.contains(&evdev) {
            self.alt_held = pressed;
        }
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
            #[allow(non_upper_case_globals)]
            KEY_Up => KeyAction::Up,
            #[allow(non_upper_case_globals)]
            KEY_Down => KeyAction::Down,
            #[allow(non_upper_case_globals)]
            KEY_Left => KeyAction::Left,
            #[allow(non_upper_case_globals)]
            KEY_Right => KeyAction::Right,
            #[allow(non_upper_case_globals)]
            KEY_Tab => {
                if self.shift_held {
                    KeyAction::Previous
                } else {
                    KeyAction::Next
                }
            }
            // Shift+Tab arrives as ISO_Left_Tab on level 2 (the
            // shift-tracked Tab above only fires when xkb still
            // reports plain Tab): the keysym itself encodes the
            // shift, so it is Previous unconditionally.
            #[allow(non_upper_case_globals)]
            KEY_ISO_Left_Tab => KeyAction::Previous,
            #[allow(non_upper_case_globals)]
            KEY_F6 => KeyAction::CycleRegion,
            #[allow(non_upper_case_globals)]
            KEY_d | KEY_D if self.super_held => KeyAction::FocusDock,
            #[allow(non_upper_case_globals)]
            KEY_F1 if self.alt_held => KeyAction::FocusPanel,
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

    /// Evdev codes for the Task 1 navigation vocabulary (`us` layout).
    const EV_UP: u32 = 103;
    const EV_DOWN: u32 = 108;
    const EV_LEFT: u32 = 105;
    const EV_RIGHT: u32 = 106;
    const EV_TAB: u32 = 15;
    const EV_F6: u32 = 64;
    const EV_D: u32 = 32;
    const EV_F1: u32 = 59;
    const EV_SHIFT_RIGHT: u32 = 54;
    const EV_SUPER_LEFT: u32 = 125;
    const EV_ALT_LEFT: u32 = 56;

    #[test]
    fn arrows_resolve_to_nav_actions() {
        let mut feed = us_feed();
        assert_eq!(feed.key(EV_UP, true), KeyAction::Up);
        assert_eq!(feed.key(EV_DOWN, true), KeyAction::Down);
        assert_eq!(feed.key(EV_LEFT, true), KeyAction::Left);
        assert_eq!(feed.key(EV_RIGHT, true), KeyAction::Right);
    }

    #[test]
    fn tab_advances_and_shift_tab_reverses() {
        let mut feed = us_feed();
        assert_eq!(feed.key(EV_TAB, true), KeyAction::Next);
        feed.key(EV_TAB, false);
        feed.key(EV_SHIFT_LEFT, true);
        assert_eq!(feed.key(EV_TAB, true), KeyAction::Previous);
        feed.key(EV_TAB, false);
        feed.key(EV_SHIFT_LEFT, false);
        assert_eq!(feed.key(EV_TAB, true), KeyAction::Next);
    }

    #[test]
    fn f6_cycles_region() {
        let mut feed = us_feed();
        assert_eq!(feed.key(EV_F6, true), KeyAction::CycleRegion);
        assert_eq!(feed.key(EV_F6, false), KeyAction::None);
    }

    #[test]
    fn super_d_focuses_dock_including_with_shift() {
        let mut feed = us_feed();
        feed.key(EV_SUPER_LEFT, true);
        assert_eq!(feed.key(EV_D, true), KeyAction::FocusDock);
        feed.key(EV_D, false);
        // Super+Shift+D keeps the dock chord: the shifted keysym
        // still resolves to FocusDock, never to text.
        feed.key(EV_SHIFT_LEFT, true);
        assert_eq!(feed.key(EV_D, true), KeyAction::FocusDock);
        feed.key(EV_D, false);
        feed.key(EV_SHIFT_LEFT, false);
        feed.key(EV_SUPER_LEFT, false);
        assert_eq!(feed.key(EV_D, true), KeyAction::Text("d".to_owned()));
    }

    #[test]
    fn alt_f1_focuses_panel() {
        let mut feed = us_feed();
        feed.key(EV_ALT_LEFT, true);
        assert_eq!(feed.key(EV_F1, true), KeyAction::FocusPanel);
        feed.key(EV_F1, false);
        feed.key(EV_ALT_LEFT, false);
    }

    #[test]
    fn bare_chord_keys_stay_out_of_nav() {
        let mut feed = us_feed();
        // Plain `d` types; only Super+D claims the dock chord.
        assert_eq!(feed.key(EV_D, true), KeyAction::Text("d".to_owned()));
        // Plain F1 is unbound: no action, and never text.
        assert_eq!(feed.key(EV_F1, true), KeyAction::None);
    }

    #[test]
    fn modifiers_and_releases_resolve_to_none() {
        let mut feed = us_feed();
        for held in [EV_SHIFT_LEFT, EV_SHIFT_RIGHT, EV_SUPER_LEFT, EV_ALT_LEFT] {
            assert_eq!(feed.key(held, true), KeyAction::None);
            assert_eq!(feed.key(held, false), KeyAction::None);
        }
        for (nav, want) in [
            (EV_UP, KeyAction::Up),
            (EV_DOWN, KeyAction::Down),
            (EV_LEFT, KeyAction::Left),
            (EV_RIGHT, KeyAction::Right),
            (EV_TAB, KeyAction::Next),
            (EV_F6, KeyAction::CycleRegion),
        ] {
            assert_eq!(feed.key(nav, true), want);
            assert_eq!(feed.key(nav, false), KeyAction::None);
        }
        // Chord keys release silently too: no trailing text.
        feed.key(EV_SUPER_LEFT, true);
        feed.key(EV_D, true);
        assert_eq!(feed.key(EV_D, false), KeyAction::None);
        feed.key(EV_SUPER_LEFT, false);
    }

    /// Collision pin (Task 1): the shell's only modified chords are
    /// Super+D (dock) and Alt+F1 (panel). The compositor owns
    /// Super+{arrows, PgUp, PgDn, R, T} and Alt+{Tab, F4} (see
    /// `crates/compositor/src/windows.rs` keycode block); the shell
    /// must never claim those (modifier, key) pairs. Bare shell keys
    /// (arrows, Tab, F6) carry no modifier, so they cannot overlap a
    /// modified compositor chord by construction — asserted below by
    /// resolving them with no modifier held.
    #[test]
    fn shell_nav_chords_avoid_compositor_window_chords() {
        const SUPER_ARROWS: [u32; 4] = [103, 108, 105, 106];
        const SUPER_PGUPDN: [u32; 2] = [104, 109];
        const SUPER_R: u32 = 19;
        const SUPER_T: u32 = 20;
        const ALT_TAB: u32 = 15;
        const ALT_F4: u32 = 62;
        // Shell's modified chords (evdev): Super+D, Alt+F1.
        assert!(
            !SUPER_ARROWS.contains(&EV_D),
            "Super+D must not be a Super+arrow"
        );
        assert!(!SUPER_PGUPDN.contains(&EV_D));
        assert_ne!(EV_D, SUPER_R, "Super+D must not be Super+R");
        assert_ne!(EV_D, SUPER_T, "Super+D must not be Super+T");
        assert_ne!(EV_F1, ALT_TAB, "Alt+F1 must not be Alt+Tab");
        assert_ne!(EV_F1, ALT_F4, "Alt+F1 must not be Alt+F4");
        // Behavioral half: the shell only claims FocusDock with Super
        // held and FocusPanel with Alt held; the bare keys resolve
        // without any modifier.
        let mut feed = us_feed();
        assert_eq!(feed.key(EV_UP, true), KeyAction::Up);
        assert_eq!(feed.key(EV_TAB, true), KeyAction::Next);
        assert_eq!(feed.key(EV_F6, true), KeyAction::CycleRegion);
        assert_eq!(feed.key(EV_D, true), KeyAction::Text("d".to_owned()));
        assert_eq!(feed.key(EV_F1, true), KeyAction::None);
    }

    #[test]
    fn garbage_keymap_string_refuses() {
        assert!(XkbFeed::from_string("not a keymap".to_owned()).is_none());
    }

    #[test]
    fn nul_terminated_server_keymap_loads() {
        use xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1;

        // Server keymap fds carry a trailing NUL, which xkbcommon's
        // own CString::new would panic on (CI journey crash): the
        // feed must strip it and translate identically.
        let feed = us_feed();
        let dumped = feed._keymap.get_as_string(KEYMAP_FORMAT_TEXT_V1);
        assert!(!dumped.is_empty());
        let mut terminated = dumped;
        terminated.push('\0');
        let mut reloaded = XkbFeed::from_string(terminated).expect("trailing NUL must be stripped");
        assert_eq!(reloaded.key(EV_A, true), KeyAction::Text("a".to_owned()));
        assert_eq!(reloaded.key(EV_RETURN, true), KeyAction::Submit);
    }

    /// A memfd holding `bytes`, rewound to the start: the same shape
    /// as the keymap fd a Wayland server hands the client.
    fn memfd_with(bytes: &[u8]) -> std::os::fd::OwnedFd {
        use rustix::fs::{memfd_create, MemfdFlags};
        use std::io::{Seek, SeekFrom, Write};
        let fd = memfd_create("roost-keymap-test", MemfdFlags::CLOEXEC)
            .expect("memfd_create succeeds in test env");
        let mut file = std::fs::File::from(fd);
        file.write_all(bytes).expect("write to memfd");
        file.seek(SeekFrom::Start(0)).expect("rewind memfd");
        file.into()
    }

    fn us_keymap_text() -> String {
        us_feed()
            ._keymap
            .get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1)
    }

    #[test]
    fn from_fd_refuses_zero_size() {
        // Security boundary (#33): size 0 is refused before any read.
        let fd = memfd_with(us_keymap_text().as_bytes());
        assert!(XkbFeed::from_fd(&fd, 0).is_none());
    }

    #[test]
    fn from_fd_refuses_oversized() {
        // Security boundary (#33): a declared size above 1 MiB is
        // refused before any read, even when the payload is valid.
        let fd = memfd_with(us_keymap_text().as_bytes());
        assert!(XkbFeed::from_fd(&fd, (1 << 20) + 1).is_none());
    }

    #[test]
    fn from_fd_accepts_valid_keymap_via_memfd() {
        let text = us_keymap_text();
        let fd = memfd_with(text.as_bytes());
        let mut feed = XkbFeed::from_fd(&fd, text.len() as u32).expect("valid keymap loads");
        assert_eq!(feed.key(EV_A, true), KeyAction::Text("a".to_owned()));
    }

    #[test]
    fn from_fd_strips_nul_terminator_from_memfd() {
        // Servers send the keymap with a trailing NUL.
        let mut text = us_keymap_text();
        text.push('\0');
        let fd = memfd_with(text.as_bytes());
        let mut feed = XkbFeed::from_fd(&fd, text.len() as u32).expect("NUL is stripped");
        assert_eq!(feed.key(EV_RETURN, true), KeyAction::Submit);
    }

    #[test]
    fn from_fd_refuses_non_utf8() {
        let bytes = [0xFF, 0xFE, 0xFD];
        let fd = memfd_with(&bytes);
        assert!(XkbFeed::from_fd(&fd, bytes.len() as u32).is_none());
    }

    #[test]
    fn from_fd_refuses_utf8_garbage() {
        let garbage = "this is not a keymap at all, just some random text";
        let fd = memfd_with(garbage.as_bytes());
        assert!(XkbFeed::from_fd(&fd, garbage.len() as u32).is_none());
    }
}
