//! Physical XKB preview for accessibility decisions, before real seat state or
//! compositor shortcuts run. Grabbed lock keys never reach the real XKB state.
use crate::{
    a11y_keyboard::{Key, Monitor},
    State,
};
use smithay::input::keyboard::{KeyboardHandle, SerializedMods};
use std::collections::HashSet;
use xkbcommon::xkb;

#[derive(Default)]
pub struct Shadow {
    generation: u64,
    state: Option<xkb::State>,
    physical: HashSet<u32>,
}
impl Shadow {
    pub fn key(
        &mut self,
        seat: &KeyboardHandle<State>,
        data: &mut State,
        generation: u64,
        monitor: &Monitor,
        physical: (u32, bool),
        delay: i32,
    ) -> bool {
        let (raw, pressed) = physical;
        let Some(code) = raw.checked_add(8).and_then(|code| u16::try_from(code).ok()) else {
            return false;
        };
        let mods = seat.modifier_state().serialized;
        if self.state.is_none() || generation != self.generation {
            let keymap = seat.with_xkb_state(data, |context| {
                let xkb = context.xkb().lock().unwrap();
                // Refcounts never escape Smithay's guarded Xkb. Compile an
                // independent context/state from the serialized keymap.
                unsafe { xkb.keymap().get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1) }
            });
            let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
            let Some(keymap) = xkb::Keymap::new_from_string(
                &context,
                keymap,
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            ) else {
                return false;
            };
            let mut state = xkb::State::new(&keymap);
            for held in &self.physical {
                state.update_key((*held).into(), xkb::KeyDirection::Down);
            }
            preserve_locks(&mut state, mods);
            self.state = Some(state);
            self.generation = generation;
        }
        let state = self.state.as_mut().unwrap();
        // Layout and latched/locked modifiers belong to the actual seat.
        // Depressed modifiers come from physical keys, even during GrabKeyboard.
        preserve_locks(state, mods);
        let key = Key {
            released: !pressed,
            state: state.serialize_mods(xkb::STATE_MODS_EFFECTIVE),
            keysym: state.key_get_one_sym(u32::from(code).into()).raw(),
            unichar: state.key_get_utf32(u32::from(code).into()),
            keycode: code,
        };
        let grabbed = monitor.key(key, std::time::Duration::from_millis(delay.max(1) as u64));
        let changed = if pressed {
            self.physical.insert(u32::from(code))
        } else {
            self.physical.remove(&u32::from(code))
        };
        if changed {
            state.update_key(
                u32::from(code).into(),
                if pressed {
                    xkb::KeyDirection::Down
                } else {
                    xkb::KeyDirection::Up
                },
            );
        }
        if grabbed {
            preserve_locks(state, mods);
        }
        grabbed
    }
}
fn preserve_locks(state: &mut xkb::State, mods: SerializedMods) {
    state.update_mask(
        state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
        mods.latched,
        mods.locked,
        0,
        0,
        mods.layout_effective,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::reexports::wayland_server::Display;
    use smithay::{backend::input::KeyState, input::keyboard::XkbConfig};
    #[test]
    fn genuine_seat_caps_and_num_lock_never_toggle_on_a_grabbed_first_press() {
        let display = Display::<State>::new().unwrap();
        let mut state = State::new(&display.handle());
        let keyboard = state
            .seat_mut()
            .add_keyboard(XkbConfig::default(), 30, 250)
            .unwrap();
        let (monitor, _events) = crate::a11y_keyboard::tests::monitor(vec![0xffe5, 0xff7f], vec![]);
        let mut shadow = Shadow::default();
        for raw in [58, 69] {
            assert!(shadow.key(&keyboard, &mut state, 1, &monitor, (raw, true), 250));
            assert!(shadow.key(&keyboard, &mut state, 1, &monitor, (raw, false), 250));
            assert!(!keyboard.modifier_state().caps_lock);
            assert!(!keyboard.modifier_state().num_lock);
            assert!(keyboard.pressed_keys().is_empty());
        }
        // The second standalone NumLock press is intentionally ordinary and
        // must toggle the real seat, with its release also passed through.
        assert!(!shadow.key(&keyboard, &mut state, 1, &monitor, (69, true), 250));
        keyboard.input_discard(&mut state, 77u32.into(), KeyState::Pressed);
        assert!(keyboard.modifier_state().num_lock);
        assert!(!shadow.key(&keyboard, &mut state, 1, &monitor, (69, false), 250));
        keyboard.input_discard(&mut state, 77u32.into(), KeyState::Released);
        assert!(keyboard.pressed_keys().is_empty());
    }
    #[test]
    fn grabbed_modifier_chord_updates_physical_preview_without_real_seat_depression() {
        let display = Display::<State>::new().unwrap();
        let mut state = State::new(&display.handle());
        let keyboard = state
            .seat_mut()
            .add_keyboard(XkbConfig::default(), 30, 250)
            .unwrap();
        let (monitor, _events) = crate::a11y_keyboard::tests::monitor(vec![0xffe5], vec![]);
        let mut shadow = Shadow::default();
        for raw in [58, 29, 59] {
            assert!(shadow.key(&keyboard, &mut state, 1, &monitor, (raw, true), 250));
        }
        assert!(!keyboard.modifier_state().ctrl);
        assert!(!keyboard.modifier_state().caps_lock);
        assert!(keyboard.pressed_keys().is_empty());
        assert_ne!(
            shadow
                .state
                .as_ref()
                .unwrap()
                .serialize_mods(xkb::STATE_MODS_DEPRESSED),
            0
        );
        for raw in [59, 29, 58] {
            assert!(shadow.key(&keyboard, &mut state, 1, &monitor, (raw, false), 250));
        }
        assert_eq!(
            shadow
                .state
                .as_ref()
                .unwrap()
                .serialize_mods(xkb::STATE_MODS_DEPRESSED),
            0
        );
    }
}
