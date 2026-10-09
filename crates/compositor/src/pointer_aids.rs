//! GNOME 51's pointer accessibility aids (#350), applied to pointer
//! events before the window manager routes them, so every client
//! (Wayland and XWayland alike) gets the clicks they make.
//!
//! The behaviour follows Clutter's seat-level pointer accessibility:
//!
//! - **Simulated secondary click**: holding the primary button still for
//!   `secondary-click-time` makes a secondary click when it is released.
//!   The primary press and release are delivered as usual; moving past
//!   `dwell-threshold`, or another button, cancels.
//! - **Hover (dwell) click**: once the pointer rests (100 ms without
//!   motion), a `dwell-time` timeout starts; moving past `dwell-threshold`
//!   cancels it, otherwise it clicks where the pointer is. In `window`
//!   mode the click is the type picked in the panel chooser (single,
//!   double, drag, secondary); a one-shot type returns to a single click
//!   once used, and a drag presses on one dwell and releases on the next.
//!   In `gesture` mode the dwell opens a second timeout: the direction the
//!   pointer then moves picks the click from the `dwell-gesture-*` keys,
//!   and the pointer goes back to where it dwelled to click.
//!
//! Each timeout is reported to the shell, which draws GNOME's pie timer
//! at the pointer.
//!
//! Like [`crate::input_aids`], this is a pure state machine the runtime
//! feeds with events, a millisecond clock and the pointer position.

use smithay::utils::{Logical, Point};
use tuna_shell_control::{DwellClick, DwellDirection, PointerAids as Settings, PointerTimeoutKind};

use crate::input_aids::{BUTTON_PRIMARY, BUTTON_SECONDARY};
use crate::windows::ManagerInput;

/// How long the pointer must rest before a dwell starts (Clutter's).
const REST_MS: u64 = 100;

/// One thing the pointer aids want done, in order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerEffect {
    /// Route this input on as if the backend had sent it.
    Input(ManagerInput),
    /// A timeout started at `pos`, running `duration_ms`.
    TimeoutStarted {
        kind: PointerTimeoutKind,
        duration_ms: u32,
        pos: Point<f64, Logical>,
    },
    /// A timeout ended; `clicked` when it ran out and acted.
    TimeoutStopped {
        kind: PointerTimeoutKind,
        clicked: bool,
    },
    /// The click the next dwell makes changed.
    ClickType(DwellClick),
}

/// The pointer aids' state machine.
pub struct PointerAids {
    settings: Settings,
    click: DwellClick,
    pos: Point<f64, Logical>,
    anchor: (u32, u64),
    /// Physical buttons held: no dwell starts under a held button.
    held: u32,
    // Simulated secondary click: when the hold runs out, where it began,
    // and whether it ran out.
    secondary: Option<(u64, Point<f64, Logical>)>,
    secondary_ready: bool,
    // Hover click.
    rest: Option<u64>,
    dwell: Option<(u64, Point<f64, Logical>)>,
    gesture: Option<(u64, Point<f64, Logical>)>,
    /// After a gesture click no dwell starts until this time.
    quiet_until: u64,
    dragging: bool,
    clicks: u64,
    secondary_clicks: u64,
}

impl Default for PointerAids {
    fn default() -> Self {
        Self::new()
    }
}

fn button(button: u32, pressed: bool, time: u32) -> PointerEffect {
    PointerEffect::Input(ManagerInput::Button {
        button,
        pressed,
        time,
    })
}

impl PointerAids {
    pub fn new() -> Self {
        Self {
            settings: Settings::default(),
            click: DwellClick::Primary,
            pos: (0.0, 0.0).into(),
            anchor: (0, 0),
            held: 0,
            secondary: None,
            secondary_ready: false,
            rest: None,
            dwell: None,
            gesture: None,
            quiet_until: 0,
            dragging: false,
            clicks: 0,
            secondary_clicks: 0,
        }
    }

    /// Whether either aid is on, or still has something to finish.
    pub fn active(&self) -> bool {
        self.settings.secondary_click
            || self.settings.dwell_click
            || self.dragging
            || self.secondary.is_some()
    }

    /// Apply GNOME's settings. Switching an aid off cancels its timeout
    /// and ends a dwell drag.
    pub fn configure(&mut self, settings: &Settings, now: u64) -> Vec<PointerEffect> {
        let mut out = Vec::new();
        let old = std::mem::replace(&mut self.settings, settings.clone());
        if old.secondary_click && !settings.secondary_click {
            self.cancel_secondary(&mut out);
        }
        if old.dwell_click && !settings.dwell_click {
            self.cancel_dwell(&mut out);
            if self.dragging {
                self.dragging = false;
                out.push(button(BUTTON_PRIMARY, false, self.stamp(now)));
            }
        }
        out
    }

    /// The chooser picked the next dwell's click.
    pub fn set_click_type(&mut self, click: DwellClick) -> Vec<PointerEffect> {
        self.click = click;
        vec![PointerEffect::ClickType(click)]
    }

    pub fn click_type(&self) -> DwellClick {
        self.click
    }

    /// Filter one pointer event. Keys pass untouched.
    pub fn process(&mut self, input: ManagerInput, now: u64) -> Vec<PointerEffect> {
        let mut out = vec![PointerEffect::Input(input)];
        match input {
            ManagerInput::Motion { pos, time } => {
                self.anchor = (time, now);
                self.pos = pos;
                self.motion(now, &mut out);
            }
            ManagerInput::Button {
                button: code,
                pressed,
                time,
            } => {
                self.anchor = (time, now);
                if pressed {
                    self.held += 1;
                } else {
                    self.held = self.held.saturating_sub(1);
                }
                self.button(code, pressed, now, &mut out);
            }
            _ => {}
        }
        out
    }

    /// Run the timeouts. `pointer` is where the pointer is now (a client
    /// may have moved it).
    pub fn poll(&mut self, now: u64, pointer: Point<f64, Logical>) -> Vec<PointerEffect> {
        let mut out = Vec::new();
        self.pos = pointer;
        if self
            .secondary
            .is_some_and(|(deadline, _)| !self.secondary_ready && now >= deadline)
        {
            self.secondary_ready = true;
            out.push(PointerEffect::TimeoutStopped {
                kind: PointerTimeoutKind::SecondaryClick,
                clicked: true,
            });
        }
        if self.rest.is_some_and(|at| now >= at) {
            self.rest = None;
            if self.settings.dwell_click && self.held == 0 && self.dwell.is_none() {
                self.dwell = Some((now + u64::from(self.settings.dwell_ms), self.pos));
                out.push(PointerEffect::TimeoutStarted {
                    kind: PointerTimeoutKind::Dwell,
                    duration_ms: self.settings.dwell_ms,
                    pos: self.pos,
                });
            }
        }
        if let Some((deadline, origin)) = self.dwell {
            if now >= deadline {
                self.dwell = None;
                out.push(PointerEffect::TimeoutStopped {
                    kind: PointerTimeoutKind::Dwell,
                    clicked: true,
                });
                let time = self.stamp(now);
                if self.settings.dwell_gesture && !self.dragging {
                    self.gesture = Some((now + u64::from(self.settings.dwell_ms), origin));
                    out.push(PointerEffect::TimeoutStarted {
                        kind: PointerTimeoutKind::Gesture,
                        duration_ms: self.settings.dwell_ms,
                        pos: origin,
                    });
                } else if self.settings.dwell_gesture {
                    self.emit_click(DwellClick::Drag, time, &mut out);
                } else {
                    self.emit_click(self.click, time, &mut out);
                    self.spend_click_type(&mut out);
                }
            }
        }
        if let Some((deadline, origin)) = self.gesture {
            if now >= deadline {
                self.gesture = None;
                let time = self.stamp(now);
                let direction = self.direction(origin);
                // Back to where the pointer dwelled, then click there.
                self.pos = origin;
                out.push(PointerEffect::Input(ManagerInput::Motion {
                    pos: origin,
                    time,
                }));
                let click = direction.and_then(|d| self.click_for(d));
                if let Some(click) = click {
                    self.emit_click(click, time, &mut out);
                }
                out.push(PointerEffect::TimeoutStopped {
                    kind: PointerTimeoutKind::Gesture,
                    clicked: click.is_some(),
                });
                // The warp back must not start another dwell at once.
                self.quiet_until = now + u64::from(self.settings.dwell_ms);
                self.rest = None;
            }
        }
        out
    }

    fn stamp(&self, now: u64) -> u32 {
        let (time, at) = self.anchor;
        time.wrapping_add(now.saturating_sub(at) as u32)
    }

    fn moved_from(&self, origin: Point<f64, Logical>) -> bool {
        let threshold = f64::from(self.settings.dwell_threshold);
        (self.pos.x - origin.x).hypot(self.pos.y - origin.y) > threshold
    }

    /// The gesture's direction: the axis it moved furthest along, once
    /// past the threshold.
    fn direction(&self, origin: Point<f64, Logical>) -> Option<DwellDirection> {
        if !self.moved_from(origin) {
            return None;
        }
        let (dx, dy) = (self.pos.x - origin.x, self.pos.y - origin.y);
        Some(if dx.abs() >= dy.abs() {
            if dx < 0.0 {
                DwellDirection::Left
            } else {
                DwellDirection::Right
            }
        } else if dy < 0.0 {
            DwellDirection::Up
        } else {
            DwellDirection::Down
        })
    }

    fn click_for(&self, direction: DwellDirection) -> Option<DwellClick> {
        let s = &self.settings;
        [
            (s.gesture_single, DwellClick::Primary),
            (s.gesture_double, DwellClick::Double),
            (s.gesture_drag, DwellClick::Drag),
            (s.gesture_secondary, DwellClick::Secondary),
        ]
        .into_iter()
        .find(|(d, _)| *d == Some(direction))
        .map(|(_, click)| click)
    }

    fn motion(&mut self, now: u64, out: &mut Vec<PointerEffect>) {
        if let Some((_, origin)) = self.secondary {
            if !self.secondary_ready && self.moved_from(origin) {
                self.cancel_secondary(out);
            }
        }
        if !self.settings.dwell_click {
            return;
        }
        self.rest = None;
        if let Some((_, origin)) = self.dwell {
            if self.moved_from(origin) {
                self.dwell = None;
                out.push(PointerEffect::TimeoutStopped {
                    kind: PointerTimeoutKind::Dwell,
                    clicked: false,
                });
            }
        }
        if self.dwell.is_none() && self.gesture.is_none() && now >= self.quiet_until {
            self.rest = Some(now + REST_MS);
        }
    }

    fn button(&mut self, code: u32, pressed: bool, now: u64, out: &mut Vec<PointerEffect>) {
        if pressed {
            // A real click cancels a pending hover click.
            self.cancel_dwell(out);
            if self.settings.secondary_click {
                if code == BUTTON_PRIMARY {
                    self.cancel_secondary(out);
                    let ms = self.settings.secondary_click_ms;
                    self.secondary = Some((now + u64::from(ms), self.pos));
                    self.secondary_ready = false;
                    out.push(PointerEffect::TimeoutStarted {
                        kind: PointerTimeoutKind::SecondaryClick,
                        duration_ms: ms,
                        pos: self.pos,
                    });
                } else {
                    self.cancel_secondary(out);
                }
            }
            return;
        }
        if code == BUTTON_PRIMARY && self.secondary.is_some() {
            if self.secondary_ready {
                self.secondary = None;
                self.secondary_ready = false;
                self.secondary_clicks += 1;
                let time = self.stamp(now);
                out.push(button(BUTTON_SECONDARY, true, time));
                out.push(button(BUTTON_SECONDARY, false, time));
            } else {
                self.cancel_secondary(out);
            }
        }
    }

    fn cancel_secondary(&mut self, out: &mut Vec<PointerEffect>) {
        if self.secondary.take().is_some() && !std::mem::take(&mut self.secondary_ready) {
            out.push(PointerEffect::TimeoutStopped {
                kind: PointerTimeoutKind::SecondaryClick,
                clicked: false,
            });
        }
    }

    fn cancel_dwell(&mut self, out: &mut Vec<PointerEffect>) {
        self.rest = None;
        if self.dwell.take().is_some() {
            out.push(PointerEffect::TimeoutStopped {
                kind: PointerTimeoutKind::Dwell,
                clicked: false,
            });
        }
        if self.gesture.take().is_some() {
            out.push(PointerEffect::TimeoutStopped {
                kind: PointerTimeoutKind::Gesture,
                clicked: false,
            });
        }
    }

    fn emit_click(&mut self, click: DwellClick, time: u32, out: &mut Vec<PointerEffect>) {
        self.clicks += 1;
        match click {
            DwellClick::Primary => {
                out.push(button(BUTTON_PRIMARY, true, time));
                out.push(button(BUTTON_PRIMARY, false, time));
            }
            DwellClick::Double => {
                for _ in 0..2 {
                    out.push(button(BUTTON_PRIMARY, true, time));
                    out.push(button(BUTTON_PRIMARY, false, time));
                }
            }
            DwellClick::Drag => {
                self.dragging = !self.dragging;
                out.push(button(BUTTON_PRIMARY, self.dragging, time));
            }
            DwellClick::Secondary => {
                out.push(button(BUTTON_SECONDARY, true, time));
                out.push(button(BUTTON_SECONDARY, false, time));
            }
        }
    }

    /// One-shot types return to a single click once used; a drag stays
    /// until it has released.
    fn spend_click_type(&mut self, out: &mut Vec<PointerEffect>) {
        let spent = match self.click {
            DwellClick::Double | DwellClick::Secondary => true,
            DwellClick::Drag => !self.dragging,
            DwellClick::Primary => false,
        };
        if spent {
            self.click = DwellClick::Primary;
            out.push(PointerEffect::ClickType(DwellClick::Primary));
        }
    }

    /// State for the compositor state file.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "secondary_click": self.settings.secondary_click,
            "dwell_click": self.settings.dwell_click,
            "dwell_gesture": self.settings.dwell_gesture,
            "click_type": format!("{:?}", self.click).to_lowercase(),
            "dwelling": self.dwell.is_some(),
            "gesture": self.gesture.is_some(),
            "dragging": self.dragging,
            "dwell_clicks": self.clicks,
            "secondary_clicks": self.secondary_clicks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion(x: f64, y: f64, at: u64) -> ManagerInput {
        ManagerInput::Motion {
            pos: (x, y).into(),
            time: at as u32,
        }
    }

    fn press(code: u32, pressed: bool, at: u64) -> ManagerInput {
        ManagerInput::Button {
            button: code,
            pressed,
            time: at as u32,
        }
    }

    /// Synthesized buttons only (the passed-through inputs are dropped).
    fn clicks(effects: &[PointerEffect], inputs: &[ManagerInput]) -> Vec<(u32, bool)> {
        effects
            .iter()
            .filter_map(|e| match e {
                PointerEffect::Input(input) if !inputs.contains(input) => match input {
                    ManagerInput::Button {
                        button, pressed, ..
                    } => Some((*button, *pressed)),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    fn timeouts(effects: &[PointerEffect]) -> Vec<String> {
        effects
            .iter()
            .filter_map(|e| match e {
                PointerEffect::TimeoutStarted { kind, .. } => Some(format!("{kind:?}+")),
                PointerEffect::TimeoutStopped { kind, clicked } => Some(format!(
                    "{kind:?}-{}",
                    if *clicked { "clicked" } else { "cancelled" }
                )),
                _ => None,
            })
            .collect()
    }

    /// Feed inputs at their times, polling every 10 ms up to `until`.
    fn run(
        aids: &mut PointerAids,
        inputs: &[(ManagerInput, u64)],
        until: u64,
    ) -> Vec<PointerEffect> {
        let mut out = Vec::new();
        let mut next = 0;
        let mut pointer: Point<f64, Logical> = aids.pos;
        let mut t = 0;
        while t <= until {
            while next < inputs.len() && inputs[next].1 <= t {
                let (input, at) = inputs[next];
                if let ManagerInput::Motion { pos, .. } = input {
                    pointer = pos;
                }
                out.extend(aids.process(input, at));
                next += 1;
            }
            out.extend(aids.poll(t, pointer));
            // The warp a gesture makes moves the real pointer too.
            for e in &out {
                if let PointerEffect::Input(ManagerInput::Motion { pos, .. }) = e {
                    pointer = *pos;
                }
            }
            t += 10;
        }
        out
    }

    fn dwell(settings: Settings) -> PointerAids {
        let mut aids = PointerAids::new();
        aids.configure(&settings, 0);
        aids
    }

    fn window_mode() -> Settings {
        Settings {
            dwell_click: true,
            dwell_ms: 500,
            dwell_threshold: 10,
            ..Settings::default()
        }
    }

    #[test]
    fn nothing_changes_with_the_aids_off() {
        let mut aids = PointerAids::new();
        assert!(!aids.active());
        let inputs = [motion(5.0, 5.0, 0), press(BUTTON_PRIMARY, true, 10)];
        let out = run(
            &mut aids,
            &[
                (inputs[0], 0),
                (inputs[1], 10),
                (press(BUTTON_PRIMARY, false, 3000), 3000),
            ],
            5000,
        );
        assert!(clicks(&out, &[]).len() == 2);
        assert!(timeouts(&out).is_empty());
    }

    #[test]
    fn resting_pointer_clicks_once_after_the_dwell_time() {
        let mut aids = dwell(window_mode());
        let out = run(&mut aids, &[(motion(100.0, 100.0, 0), 0)], 2000);
        // Rest 100 ms, dwell 500 ms, one click, no repeat while still.
        assert_eq!(timeouts(&out), vec!["Dwell+", "Dwell-clicked"]);
        assert_eq!(
            clicks(&out, &[]),
            vec![(BUTTON_PRIMARY, true), (BUTTON_PRIMARY, false)]
        );
        let started = out
            .iter()
            .find_map(|e| match e {
                PointerEffect::TimeoutStarted {
                    duration_ms, pos, ..
                } => Some((*duration_ms, *pos)),
                _ => None,
            })
            .unwrap();
        assert_eq!(started, (500, (100.0, 100.0).into()));
    }

    #[test]
    fn moving_past_the_threshold_cancels_and_small_jitter_does_not() {
        let mut aids = dwell(window_mode());
        let out = run(
            &mut aids,
            &[
                (motion(100.0, 100.0, 0), 0),
                (motion(105.0, 104.0, 300), 300), // within 10 px
                (motion(130.0, 100.0, 400), 400), // beyond
            ],
            450,
        );
        assert_eq!(timeouts(&out), vec!["Dwell+", "Dwell-cancelled"]);
        assert!(clicks(&out, &[]).is_empty());
    }

    #[test]
    fn a_real_button_cancels_the_dwell() {
        let mut aids = dwell(window_mode());
        let inputs = [
            (motion(100.0, 100.0, 0), 0),
            (press(BUTTON_PRIMARY, true, 300), 300),
            (press(BUTTON_PRIMARY, false, 320), 320),
        ];
        let out = run(&mut aids, &inputs, 2000);
        assert_eq!(timeouts(&out), vec!["Dwell+", "Dwell-cancelled"]);
        let real: Vec<ManagerInput> = inputs.iter().map(|(i, _)| *i).collect();
        assert!(clicks(&out, &real).is_empty());
    }

    #[test]
    fn chooser_types_are_one_shot_and_drag_spans_two_dwells() {
        let mut aids = dwell(window_mode());
        assert_eq!(
            aids.set_click_type(DwellClick::Double),
            vec![PointerEffect::ClickType(DwellClick::Double)]
        );
        let out = run(&mut aids, &[(motion(10.0, 10.0, 0), 0)], 1000);
        assert_eq!(clicks(&out, &[]).len(), 4, "a double click");
        assert!(out.contains(&PointerEffect::ClickType(DwellClick::Primary)));
        assert_eq!(aids.click_type(), DwellClick::Primary);

        aids.set_click_type(DwellClick::Secondary);
        let out = run(&mut aids, &[(motion(200.0, 10.0, 0), 0)], 1000);
        assert_eq!(
            clicks(&out, &[]),
            vec![(BUTTON_SECONDARY, true), (BUTTON_SECONDARY, false)]
        );
        assert_eq!(aids.click_type(), DwellClick::Primary);

        aids.set_click_type(DwellClick::Drag);
        let out = run(&mut aids, &[(motion(300.0, 10.0, 0), 0)], 1000);
        assert_eq!(clicks(&out, &[]), vec![(BUTTON_PRIMARY, true)]);
        assert_eq!(aids.click_type(), DwellClick::Drag, "still dragging");
        let out = run(&mut aids, &[(motion(400.0, 60.0, 0), 0)], 1000);
        assert_eq!(clicks(&out, &[]), vec![(BUTTON_PRIMARY, false)]);
        assert_eq!(aids.click_type(), DwellClick::Primary);
    }

    #[test]
    fn switching_dwell_off_ends_a_drag() {
        let mut aids = dwell(window_mode());
        aids.set_click_type(DwellClick::Drag);
        run(&mut aids, &[(motion(10.0, 10.0, 0), 0)], 1000);
        let out = aids.configure(&Settings::default(), 1100);
        assert_eq!(clicks(&out, &[]), vec![(BUTTON_PRIMARY, false)]);
        assert!(!aids.active());
    }

    #[test]
    fn gestures_pick_the_click_by_direction_and_click_where_it_dwelled() {
        let gesture = Settings {
            dwell_gesture: true,
            ..window_mode()
        };
        // Dwell at (100,100), then move right: GNOME's default secondary.
        let mut aids = dwell(gesture.clone());
        let out = run(
            &mut aids,
            &[
                (motion(100.0, 100.0, 0), 0),
                (motion(140.0, 105.0, 700), 700),
            ],
            2500,
        );
        assert_eq!(
            timeouts(&out),
            vec!["Dwell+", "Dwell-clicked", "Gesture+", "Gesture-clicked"]
        );
        assert_eq!(
            clicks(&out, &[]),
            vec![(BUTTON_SECONDARY, true), (BUTTON_SECONDARY, false)]
        );
        // The click happens back at the dwell position.
        let warp = out
            .iter()
            .rev()
            .find_map(|e| match e {
                PointerEffect::Input(ManagerInput::Motion { pos, .. }) => Some(*pos),
                _ => None,
            })
            .unwrap();
        assert_eq!(warp, (100.0, 100.0).into());
        // Up is a double click.
        let mut aids = dwell(gesture.clone());
        let out = run(
            &mut aids,
            &[
                (motion(100.0, 100.0, 0), 0),
                (motion(102.0, 60.0, 700), 700),
            ],
            2500,
        );
        assert_eq!(clicks(&out, &[]).len(), 4);
        // Staying put clicks nothing.
        let mut aids = dwell(gesture);
        let out = run(&mut aids, &[(motion(100.0, 100.0, 0), 0)], 2500);
        assert!(clicks(&out, &[]).is_empty());
        assert!(timeouts(&out).contains(&"Gesture-cancelled".to_owned()));
    }

    fn secondary() -> PointerAids {
        dwell(Settings {
            secondary_click: true,
            secondary_click_ms: 800,
            ..Settings::default()
        })
    }

    #[test]
    fn holding_primary_makes_a_secondary_click_on_release() {
        let mut aids = secondary();
        let inputs = [
            (motion(50.0, 50.0, 0), 0),
            (press(BUTTON_PRIMARY, true, 100), 100),
            (press(BUTTON_PRIMARY, false, 1000), 1000),
        ];
        let out = run(&mut aids, &inputs, 1200);
        let real: Vec<ManagerInput> = inputs.iter().map(|(i, _)| *i).collect();
        assert_eq!(
            clicks(&out, &real),
            vec![(BUTTON_SECONDARY, true), (BUTTON_SECONDARY, false)]
        );
        assert_eq!(
            timeouts(&out),
            vec!["SecondaryClick+", "SecondaryClick-clicked"]
        );
        // The secondary click follows the primary release.
        let release = out
            .iter()
            .position(|e| *e == PointerEffect::Input(inputs[2].0))
            .unwrap();
        let secondary = out
            .iter()
            .position(|e| {
                matches!(
                    e,
                    PointerEffect::Input(ManagerInput::Button {
                        button: BUTTON_SECONDARY,
                        ..
                    })
                )
            })
            .unwrap();
        assert!(secondary > release);
    }

    #[test]
    fn short_presses_and_moves_cancel_the_secondary_click() {
        let mut aids = secondary();
        let inputs = [
            (motion(50.0, 50.0, 0), 0),
            (press(BUTTON_PRIMARY, true, 100), 100),
            (press(BUTTON_PRIMARY, false, 400), 400),
            (press(BUTTON_PRIMARY, true, 500), 500),
            (motion(80.0, 50.0, 700), 700),
            (press(BUTTON_PRIMARY, false, 1600), 1600),
        ];
        let out = run(&mut aids, &inputs, 1800);
        let real: Vec<ManagerInput> = inputs.iter().map(|(i, _)| *i).collect();
        assert!(clicks(&out, &real).is_empty());
        assert_eq!(
            timeouts(&out),
            vec![
                "SecondaryClick+",
                "SecondaryClick-cancelled",
                "SecondaryClick+",
                "SecondaryClick-cancelled"
            ]
        );
    }
}
