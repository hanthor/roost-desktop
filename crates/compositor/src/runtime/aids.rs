//! The runtime's side of GNOME's accessibility input aids (#350):
//! backend input passes through the keyboard aids ([`InputAids`]) and
//! then the pointer aids ([`PointerAids`]) before the lock screen,
//! keybindings or any client sees it, and what comes out is routed as if
//! the backend had sent it.

use std::time::Instant;

use tuna_shell_control::Message;

use crate::input_aids::{AidEffect, InputAids};
use crate::pointer_aids::{PointerAids, PointerEffect};
use crate::windows::ManagerInput;

use super::Runtime;

/// The aids' engines and the clock they run on.
pub(super) struct RuntimeAids {
    keyboard: InputAids,
    pointer: PointerAids,
    epoch: Instant,
}

impl Default for RuntimeAids {
    fn default() -> Self {
        Self {
            keyboard: InputAids::new(),
            pointer: PointerAids::new(),
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

    fn active(&self) -> bool {
        self.keyboard.active() || self.pointer.active()
    }
}

impl Runtime {
    /// One event from the hardware or nested backend, filtered by the
    /// aids first. Remote-desktop input does not come this way: GNOME's
    /// aids serve the person at the keyboard and pointer.
    pub(super) fn on_backend_input(&mut self, input: ManagerInput) {
        if !self.input_aids.active() {
            self.on_manager_input(input);
            return;
        }
        if !self.input_aids.keyboard.active() {
            self.route_pointer_aids(input);
            return;
        }
        let now = self.input_aids.now_ms();
        let num_lock = self.manager.num_lock();
        let effects = self.input_aids.keyboard.process(input, now, num_lock);
        self.apply_aid_effects(effects);
    }

    /// The aids' timers (Slow Keys, the Shift-hold shortcut, Mouse Keys
    /// motion, hover and secondary clicks), run every tick.
    pub(super) fn poll_input_aids(&mut self) {
        if !self.input_aids.active() {
            return;
        }
        let now = self.input_aids.now_ms();
        let effects = self.input_aids.keyboard.poll(now);
        self.apply_aid_effects(effects);
        if self.input_aids.pointer.active() {
            let pointer = self.manager.pointer_pos();
            let effects = self.input_aids.pointer.poll(now, pointer);
            self.apply_pointer_effects(effects);
        }
    }

    /// GNOME's `org.gnome.desktop.a11y.keyboard` and `.mouse`, live from
    /// the shell.
    pub(super) fn configure_input_aids(
        &mut self,
        keyboard: &tuna_shell_control::KeyboardAids,
        pointer: &tuna_shell_control::PointerAids,
    ) {
        let now = self.input_aids.now_ms();
        let effects = self.input_aids.keyboard.configure(keyboard, now);
        self.apply_aid_effects(effects);
        let effects = self.input_aids.pointer.configure(pointer, now);
        self.apply_pointer_effects(effects);
    }

    /// The hover-click chooser picked the next dwell's click.
    pub(super) fn set_dwell_click_type(&mut self, click: tuna_shell_control::DwellClick) {
        let effects = self.input_aids.pointer.set_click_type(click);
        self.apply_pointer_effects(effects);
    }

    /// The aids for the state file; nothing while locked.
    pub(super) fn input_aids_summary(&self) -> serde_json::Value {
        if self.is_locked() {
            return serde_json::Value::Null;
        }
        let mut summary = self.input_aids.keyboard.summary();
        summary["pointer"] = self.input_aids.pointer.summary();
        summary
    }

    fn apply_aid_effects(&mut self, effects: Vec<AidEffect>) {
        for effect in effects {
            match effect {
                AidEffect::Input(input) => self.route_pointer_aids(input),
                AidEffect::PointerMove { dx, dy, time } => {
                    let pos = self.manager.pointer_pos();
                    let pos = self.clamp_to_outputs(pos.x + dx, pos.y + dy);
                    self.route_pointer_aids(ManagerInput::Motion {
                        pos: pos.into(),
                        time,
                    });
                }
                AidEffect::Bell => self.state.ring_bell(),
                AidEffect::Toggled { aid, enabled } => {
                    eprintln!("tuna-compositor: keyboard aid {aid:?} switched enabled={enabled}");
                    self.control
                        .queue_message(Message::KeyboardAidToggled { aid, enabled });
                }
            }
        }
    }

    /// Pointer events pass the pointer aids on their way in.
    fn route_pointer_aids(&mut self, input: ManagerInput) {
        let pointer_event = matches!(
            input,
            ManagerInput::Motion { .. } | ManagerInput::Button { .. }
        );
        if !pointer_event || !self.input_aids.pointer.active() {
            self.on_manager_input(input);
            return;
        }
        let now = self.input_aids.now_ms();
        let effects = self.input_aids.pointer.process(input, now);
        self.apply_pointer_effects(effects);
    }

    fn apply_pointer_effects(&mut self, effects: Vec<PointerEffect>) {
        for effect in effects {
            match effect {
                PointerEffect::Input(input) => self.on_manager_input(input),
                PointerEffect::TimeoutStarted {
                    kind,
                    duration_ms,
                    pos,
                } => self.control.queue_message(Message::PointerTimeout {
                    kind,
                    duration_ms: Some(duration_ms),
                    clicked: false,
                    x: pos.x.round() as i32,
                    y: pos.y.round() as i32,
                }),
                PointerEffect::TimeoutStopped { kind, clicked } => {
                    let pos = self.manager.pointer_pos();
                    self.control.queue_message(Message::PointerTimeout {
                        kind,
                        duration_ms: None,
                        clicked,
                        x: pos.x.round() as i32,
                        y: pos.y.round() as i32,
                    });
                }
                PointerEffect::ClickType(click) => {
                    self.control
                        .queue_message(Message::DwellClickType { click });
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
