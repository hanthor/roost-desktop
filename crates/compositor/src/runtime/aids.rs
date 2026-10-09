//! The runtime's side of GNOME's keyboard accessibility aids (#350):
//! backend input passes through [`InputAids`] before the lock screen,
//! keybindings or any client sees it, and what comes out is routed as if
//! the backend had sent it.

use std::time::Instant;

use crate::input_aids::{AidEffect, InputAids};
use crate::windows::ManagerInput;

use super::Runtime;

/// The aids' engine and the clock it runs on.
pub(super) struct RuntimeAids {
    engine: InputAids,
    epoch: Instant,
}

impl Default for RuntimeAids {
    fn default() -> Self {
        Self {
            engine: InputAids::new(),
            epoch: Instant::now(),
        }
    }
}

impl RuntimeAids {
    fn now_ms(&self) -> u64 {
        self.epoch
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

impl Runtime {
    /// One event from the hardware or nested backend, filtered by the
    /// aids first. Remote-desktop input does not come this way: GNOME's
    /// aids serve the person at the keyboard.
    pub(super) fn on_backend_input(&mut self, input: ManagerInput) {
        if !self.input_aids.engine.active() {
            self.on_manager_input(input);
            return;
        }
        let now = self.input_aids.now_ms();
        let num_lock = self.manager.num_lock();
        let effects = self.input_aids.engine.process(input, now, num_lock);
        self.apply_aid_effects(effects);
    }

    /// The aids' timers (Slow Keys, the Shift-hold shortcut, Mouse Keys
    /// motion), run every tick.
    pub(super) fn poll_input_aids(&mut self) {
        if !self.input_aids.engine.active() {
            return;
        }
        let now = self.input_aids.now_ms();
        let effects = self.input_aids.engine.poll(now);
        self.apply_aid_effects(effects);
    }

    /// GNOME's `org.gnome.desktop.a11y.keyboard`, live from the shell.
    pub(super) fn configure_input_aids(&mut self, settings: &tuna_shell_control::KeyboardAids) {
        let now = self.input_aids.now_ms();
        let effects = self.input_aids.engine.configure(settings, now);
        self.apply_aid_effects(effects);
    }

    /// The aids for the state file; nothing while locked.
    pub(super) fn input_aids_summary(&self) -> serde_json::Value {
        if self.is_locked() {
            return serde_json::Value::Null;
        }
        self.input_aids.engine.summary()
    }

    fn apply_aid_effects(&mut self, effects: Vec<AidEffect>) {
        for effect in effects {
            match effect {
                AidEffect::Input(input) => self.on_manager_input(input),
                AidEffect::PointerMove { dx, dy, time } => {
                    let pos = self.manager.pointer_pos();
                    let pos = self.clamp_to_outputs(pos.x + dx, pos.y + dy);
                    self.on_manager_input(ManagerInput::Motion {
                        pos: pos.into(),
                        time,
                    });
                }
                AidEffect::Bell => self.state.ring_bell(),
                AidEffect::Toggled { aid, enabled } => {
                    eprintln!("tuna-compositor: keyboard aid {aid:?} switched enabled={enabled}");
                    self.control
                        .queue_message(tuna_shell_control::Message::KeyboardAidToggled {
                            aid,
                            enabled,
                        });
                }
            }
        }
    }

    /// Keep a Mouse Keys move on some output: the nearest point of the
    /// nearest output when it would leave them all.
    fn clamp_to_outputs(&self, x: f64, y: f64) -> (f64, f64) {
        let mut best = (x, y);
        let mut best_distance = f64::INFINITY;
        for output in &self.state.outputs {
            let (left, top) = (f64::from(output.loc.0), f64::from(output.loc.1));
            let right = left + f64::from(output.size.w) - 1.0;
            let bottom = top + f64::from(output.size.h) - 1.0;
            let clamped = (
                x.clamp(left, right.max(left)),
                y.clamp(top, bottom.max(top)),
            );
            let distance = (clamped.0 - x).powi(2) + (clamped.1 - y).powi(2);
            if distance < best_distance {
                best_distance = distance;
                best = clamped;
            }
        }
        best
    }
}
