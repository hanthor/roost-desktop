//! GNOME 51's keyboard accessibility aids (#350), applied to backend key
//! events before keybindings, the lock screen or any client sees them, so
//! every client (Wayland and XWayland alike) gets the same filtered stream.
//!
//! The behaviour follows Mutter's seat-level keyboard accessibility:
//!
//! - **Sticky Keys** latch a modifier tapped on its own (it applies to
//!   the next key), lock it on a second tap and release it on a third.
//!   Pressing a modifier together with another key never latches it. With
//!   `stickykeys-two-key-off`, two modifiers pressed at once switch the
//!   aid off. A latch is a held press: the modifier's release is withheld
//!   until the next key's release, so clients and keybindings see an
//!   ordinary chord.
//! - **Slow Keys** hold a press back until the key has been down for
//!   `slowkeys-delay`; a key released sooner is never delivered.
//! - **Bounce Keys** drop a press of the key just released within
//!   `bouncekeys-delay` (and its release with it).
//! - **Mouse Keys** move the pointer with the keypad (Num Lock off), with
//!   Mutter's acceleration curve; 5 clicks, 0 presses, `.` releases, `+`
//!   double-clicks, `/` `*` `-` choose the primary, middle or secondary
//!   button.
//! - **Toggle Keys** beep when Caps Lock or Num Lock is pressed.
//! - **Keyboard shortcuts** (`enable`): Shift pressed five times in a row
//!   switches Sticky Keys, Shift held for eight seconds switches Slow
//!   Keys. The shell saves the change and confirms it.
//!
//! The engine is a pure state machine: the runtime feeds it events and a
//! millisecond clock, polls it every frame for timers, and routes the
//! [`AidEffect`]s it returns. Key identities never leave it; the state
//! file gets counters and modifier classes only.

use std::collections::{BTreeMap, BTreeSet};

use tuna_shell_control::{KeyboardAid, KeyboardAids};

use crate::windows::ManagerInput;

/// One thing the aids want done, in order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AidEffect {
    /// Route this input on as if the backend had sent it.
    Input(ManagerInput),
    /// Mouse Keys moved the pointer by this many logical pixels.
    PointerMove { dx: f64, dy: f64, time: u32 },
    /// Sound the bell (Mutter's `meta_bell_notify`).
    Bell,
    /// A keyboard shortcut switched an aid; the shell saves and confirms.
    Toggled { aid: KeyboardAid, enabled: bool },
}

/// A modifier's role, by kernel keycode. Mutter decides by keysym; the
/// keycodes here are the keys that carry those keysyms in GNOME's
/// layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Modifier {
    Shift,
    Control,
    Alt,
    Level3,
    Super,
}

impl Modifier {
    fn name(self) -> &'static str {
        match self {
            Modifier::Shift => "shift",
            Modifier::Control => "control",
            Modifier::Alt => "alt",
            Modifier::Level3 => "level3",
            Modifier::Super => "super",
        }
    }
}

const SHIFT_LEFT: u32 = 42;
const SHIFT_RIGHT: u32 = 54;
const CAPS_LOCK: u32 = 58;
const NUM_LOCK: u32 = 69;

/// What a key is to Sticky Keys: a modifier (Caps Lock counts, but is
/// never itself made sticky, as in Mutter) or anything else.
fn modifier_of(keycode: u32) -> Option<Option<Modifier>> {
    Some(Some(match keycode {
        SHIFT_LEFT | SHIFT_RIGHT => Modifier::Shift,
        29 | 97 => Modifier::Control,
        56 => Modifier::Alt,
        100 => Modifier::Level3,
        125 | 126 => Modifier::Super,
        CAPS_LOCK => return Some(None),
        _ => return None,
    }))
}

/// Mutter's shortcut timings.
const SHIFT_RUN_WINDOW_MS: u64 = 15_000;
const SHIFT_RUN_PRESSES: u32 = 5;
const SHIFT_HOLD_MS: u64 = 8_000;

/// Mouse Keys: Mutter repeats motion every 100 ms on a curve of 1.05.
const MOUSE_INTERVAL_MS: u64 = 100;
const MOUSE_CURVE: f64 = 1.0 + 50.0 * 0.001;

/// Linux button codes.
pub const BUTTON_PRIMARY: u32 = 0x110;
pub const BUTTON_SECONDARY: u32 = 0x111;
pub const BUTTON_MIDDLE: u32 = 0x112;

/// What a keypad key does under Mouse Keys (Num Lock off).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pad {
    Move(i8, i8),
    Choose(u32),
    Click,
    DoubleClick,
    Press,
    Release,
}

fn pad_of(keycode: u32) -> Option<Pad> {
    Some(match keycode {
        71 => Pad::Move(-1, -1),
        72 => Pad::Move(0, -1),
        73 => Pad::Move(1, -1),
        75 => Pad::Move(-1, 0),
        77 => Pad::Move(1, 0),
        79 => Pad::Move(-1, 1),
        80 => Pad::Move(0, 1),
        81 => Pad::Move(1, 1),
        76 => Pad::Click,
        78 => Pad::DoubleClick,
        82 => Pad::Press,
        83 => Pad::Release,
        98 => Pad::Choose(BUTTON_PRIMARY),
        55 => Pad::Choose(BUTTON_MIDDLE),
        74 => Pad::Choose(BUTTON_SECONDARY),
        _ => return None,
    })
}

/// Counters the state file publishes for proofs.
#[derive(Debug, Default, Clone, Copy)]
struct Counters {
    slow_accepted: u64,
    slow_rejected: u64,
    bounce_rejected: u64,
    sticky_latches: u64,
    mouse_moves: u64,
    mouse_clicks: u64,
    toggles: u64,
    bells: u64,
}

/// The keyboard aids' state machine.
pub struct InputAids {
    settings: KeyboardAids,
    /// Sticky and Slow Keys as currently in force: a shortcut flips them
    /// ahead of the setting the shell then writes.
    sticky: bool,
    slow: bool,
    /// Newest backend stamp and the clock reading it arrived at, so
    /// synthesized events carry stamps in the backend's base.
    anchor: (u32, u64),
    // Keyboard shortcuts.
    shift_run: u32,
    last_shift: Option<u64>,
    shift_held_since: Option<u64>,
    // Bounce Keys: the key just released and until when it bounces.
    bounce: Option<(u32, u64)>,
    bounce_dropped: BTreeSet<u32>,
    // Slow Keys: presses waiting out the delay, oldest first.
    slow_pending: Vec<(u32, u64)>,
    // Sticky Keys.
    held: BTreeMap<u32, Modifier>,
    depressed: BTreeMap<Modifier, u32>,
    latched: BTreeMap<Modifier, u32>,
    locked: BTreeMap<Modifier, u32>,
    /// Modifier keys whose press clients have seen and whose release
    /// they have not.
    delivered: BTreeSet<u32>,
    // Mouse Keys.
    mouse_button: u32,
    mouse_down: BTreeSet<u32>,
    mouse_dir: Option<(i8, i8)>,
    mouse_next: Option<u64>,
    mouse_first: Option<u64>,
    mouse_last: u64,
    counters: Counters,
}

impl Default for InputAids {
    fn default() -> Self {
        Self::new()
    }
}

impl InputAids {
    pub fn new() -> Self {
        Self {
            settings: KeyboardAids::default(),
            sticky: false,
            slow: false,
            anchor: (0, 0),
            shift_run: 0,
            last_shift: None,
            shift_held_since: None,
            bounce: None,
            bounce_dropped: BTreeSet::new(),
            slow_pending: Vec::new(),
            held: BTreeMap::new(),
            depressed: BTreeMap::new(),
            latched: BTreeMap::new(),
            locked: BTreeMap::new(),
            delivered: BTreeSet::new(),
            mouse_button: BUTTON_PRIMARY,
            mouse_down: BTreeSet::new(),
            mouse_dir: None,
            mouse_next: None,
            mouse_first: None,
            mouse_last: 0,
            counters: Counters::default(),
        }
    }

    /// Whether any aid can change the event stream (the runtime's fast path).
    pub fn active(&self) -> bool {
        self.sticky
            || self.slow
            || self.settings.shortcuts
            || self.settings.bounce
            || self.settings.mouse
            || self.settings.toggle_beep
            || !self.slow_pending.is_empty()
            || !self.delivered.is_empty()
    }

    /// Apply GNOME's settings. An aid whose setting changed starts from a
    /// clean state, as in Mutter; switching one off releases whatever it
    /// was holding down.
    pub fn configure(&mut self, settings: &KeyboardAids, now: u64) -> Vec<AidEffect> {
        let mut out = Vec::new();
        let time = self.stamp(now);
        let old = std::mem::replace(&mut self.settings, settings.clone());
        if old.sticky != settings.sticky {
            self.set_sticky(settings.sticky, time, &mut out);
        }
        if old.slow != settings.slow {
            self.slow = settings.slow;
            self.slow_pending.clear();
        }
        if old.bounce != settings.bounce {
            self.bounce = None;
        }
        if old.shortcuts != settings.shortcuts {
            self.shift_run = 0;
            self.last_shift = None;
            self.shift_held_since = None;
        }
        if old.mouse != settings.mouse {
            self.stop_mouse();
            self.mouse_button = BUTTON_PRIMARY;
            if !settings.mouse {
                for button in std::mem::take(&mut self.mouse_down) {
                    out.push(AidEffect::Input(ManagerInput::Button {
                        button,
                        pressed: false,
                        time,
                    }));
                }
            }
        }
        out
    }

    /// Filter one backend event. `num_lock` is the seat's Num Lock: Mouse
    /// Keys stand aside while it is on, as in GNOME.
    pub fn process(&mut self, input: ManagerInput, now: u64, num_lock: bool) -> Vec<AidEffect> {
        let mut out = Vec::new();
        match input {
            ManagerInput::Key {
                keycode,
                pressed,
                time,
            } => {
                self.anchor = (time, now);
                self.key(keycode, pressed, time, now, num_lock, &mut out);
            }
            other => out.push(AidEffect::Input(other)),
        }
        out
    }

    /// Run the aids' timers: Slow Keys acceptance, the Shift-hold
    /// shortcut and Mouse Keys motion.
    pub fn poll(&mut self, now: u64) -> Vec<AidEffect> {
        let mut out = Vec::new();
        if self
            .shift_held_since
            .is_some_and(|since| now >= since + SHIFT_HOLD_MS)
        {
            self.shift_held_since = None;
            self.feature_beep(&mut out);
            self.slow = !self.slow;
            self.slow_pending.clear();
            self.counters.toggles += 1;
            out.push(AidEffect::Toggled {
                aid: KeyboardAid::SlowKeys,
                enabled: self.slow,
            });
        }
        while let Some(index) = self
            .slow_pending
            .iter()
            .position(|(_, deadline)| *deadline <= now)
        {
            let (keycode, _) = self.slow_pending.remove(index);
            self.counters.slow_accepted += 1;
            let time = self.stamp(now);
            self.deliver(keycode, true, time, &mut out);
            if self.settings.slow_beep_accept {
                self.bell(&mut out);
            }
        }
        if self.mouse_next.is_some_and(|next| now >= next) {
            self.move_mouse(now, &mut out);
        }
        out
    }

    /// A stamp in the backend's base for an event made now.
    fn stamp(&self, now: u64) -> u32 {
        let (time, at) = self.anchor;
        time.wrapping_add(now.saturating_sub(at) as u32)
    }

    fn bell(&mut self, out: &mut Vec<AidEffect>) {
        self.counters.bells += 1;
        out.push(AidEffect::Bell);
    }

    fn feature_beep(&mut self, out: &mut Vec<AidEffect>) {
        if self.settings.feature_beep {
            self.bell(out);
        }
    }

    fn key(
        &mut self,
        keycode: u32,
        pressed: bool,
        time: u32,
        now: u64,
        num_lock: bool,
        out: &mut Vec<AidEffect>,
    ) {
        if self.settings.shortcuts {
            self.shortcut_key(keycode, pressed, time, out);
        }
        if self.settings.mouse && self.mouse_key(keycode, pressed, time, now, num_lock, out) {
            return;
        }
        if self.settings.bounce && self.settings.bounce_delay_ms > 0 {
            if pressed {
                if self
                    .bounce
                    .is_some_and(|(key, until)| key == keycode && now < until)
                {
                    self.counters.bounce_rejected += 1;
                    self.bounce_dropped.insert(keycode);
                    if self.settings.bounce_beep_reject {
                        self.bell(out);
                    }
                    return;
                }
            } else {
                self.bounce = Some((keycode, now + u64::from(self.settings.bounce_delay_ms)));
                // The dropped press's release is dropped too: clients
                // never hear a release for a key they never saw go down.
                if self.bounce_dropped.remove(&keycode) {
                    return;
                }
            }
        }
        if self.slow && self.settings.slow_delay_ms > 0 {
            if pressed {
                if !self.slow_pending.iter().any(|(key, _)| *key == keycode) {
                    self.slow_pending
                        .push((keycode, now + u64::from(self.settings.slow_delay_ms)));
                    if self.settings.slow_beep_press {
                        self.bell(out);
                    }
                }
                return;
            }
            if let Some(index) = self
                .slow_pending
                .iter()
                .position(|(key, _)| *key == keycode)
            {
                self.slow_pending.remove(index);
                self.counters.slow_rejected += 1;
                if self.settings.slow_beep_reject {
                    self.bell(out);
                }
                return;
            }
        }
        self.deliver(keycode, pressed, time, out);
    }

    /// Shift five times within fifteen seconds switches Sticky Keys on
    /// its last release; Shift held eight seconds switches Slow Keys
    /// (fired from [`poll`](Self::poll)). Any other key resets both.
    fn shortcut_key(&mut self, keycode: u32, pressed: bool, time: u32, out: &mut Vec<AidEffect>) {
        let shift = keycode == SHIFT_LEFT || keycode == SHIFT_RIGHT;
        let now = self.anchor.1;
        if pressed {
            if shift {
                if self.shift_held_since.is_none() {
                    self.shift_held_since = Some(now);
                }
                let fresh = self
                    .last_shift
                    .is_none_or(|last| now > last + SHIFT_RUN_WINDOW_MS);
                self.shift_run = if fresh { 1 } else { self.shift_run + 1 };
                self.last_shift = Some(now);
            } else {
                self.shift_run = 0;
                self.shift_held_since = None;
            }
        } else if shift {
            self.shift_held_since = None;
            if self.shift_run >= SHIFT_RUN_PRESSES {
                self.shift_run = 0;
                self.feature_beep(out);
                let enable = !self.sticky;
                self.set_sticky(enable, time, out);
                self.counters.toggles += 1;
                out.push(AidEffect::Toggled {
                    aid: KeyboardAid::StickyKeys,
                    enabled: enable,
                });
            }
        }
    }

    /// Switch Sticky Keys, dropping every latch and lock (their held
    /// presses are released).
    fn set_sticky(&mut self, enable: bool, time: u32, out: &mut Vec<AidEffect>) {
        self.sticky = enable;
        self.latched.clear();
        self.locked.clear();
        self.depressed.clear();
        self.release_unheld(time, out);
        // From here physical holds pass straight through; their releases
        // still reach clients because unknown releases are forwarded.
        self.delivered.clear();
        self.held.clear();
    }

    /// Whether a latch or lock keeps this key down for clients.
    fn retained(&self, keycode: u32) -> bool {
        self.latched
            .values()
            .chain(self.locked.values())
            .any(|key| *key == keycode)
    }

    /// Release every delivered modifier no finger and no latch or lock
    /// holds any more.
    fn release_unheld(&mut self, time: u32, out: &mut Vec<AidEffect>) {
        let stale: Vec<u32> = self
            .delivered
            .iter()
            .copied()
            .filter(|key| !self.held.contains_key(key) && !self.retained(*key))
            .collect();
        for keycode in stale {
            self.delivered.remove(&keycode);
            out.push(AidEffect::Input(ManagerInput::Key {
                keycode,
                pressed: false,
                time,
            }));
        }
    }

    /// The last stage: Sticky Keys and the Toggle Keys beep, then out.
    fn deliver(&mut self, keycode: u32, pressed: bool, time: u32, out: &mut Vec<AidEffect>) {
        let key = AidEffect::Input(ManagerInput::Key {
            keycode,
            pressed,
            time,
        });
        if pressed && self.settings.toggle_beep && (keycode == CAPS_LOCK || keycode == NUM_LOCK) {
            self.bell(out);
        }
        if !self.sticky {
            out.push(key);
            return;
        }
        let Some(modifier) = modifier_of(keycode) else {
            out.push(key);
            if !pressed {
                // A key typed with a modifier held never latches it; a
                // latched modifier is spent on this key.
                self.depressed.clear();
                if !self.latched.is_empty() {
                    self.latched.clear();
                    self.release_unheld(time, out);
                }
            }
            return;
        };
        if pressed {
            if !self.depressed.is_empty() && self.settings.sticky_two_key_off {
                self.set_sticky(false, time, out);
                self.counters.toggles += 1;
                out.push(key);
                out.push(AidEffect::Toggled {
                    aid: KeyboardAid::StickyKeys,
                    enabled: false,
                });
                return;
            }
            if let Some(modifier) = modifier {
                self.held.insert(keycode, modifier);
                if self.delivered.insert(keycode) {
                    out.push(key);
                }
            } else {
                out.push(key);
            }
            self.depressed = self.held.iter().map(|(key, m)| (*m, *key)).collect();
            return;
        }
        self.held.remove(&keycode);
        let depressed = std::mem::take(&mut self.depressed);
        if !depressed.is_empty() {
            if depressed.keys().any(|m| self.locked.contains_key(m)) {
                for m in depressed.keys() {
                    self.locked.remove(m);
                }
            } else if depressed.keys().any(|m| self.latched.contains_key(m)) {
                for (m, key) in &depressed {
                    self.latched.remove(m);
                    self.locked.insert(*m, *key);
                }
            } else {
                self.counters.sticky_latches += 1;
                self.latched.extend(depressed);
            }
            if self.settings.sticky_beep {
                self.bell(out);
            }
        }
        if modifier.is_none() {
            out.push(key);
        } else if !self.retained(keycode) {
            self.delivered.remove(&keycode);
            out.push(key);
        }
        self.release_unheld(time, out);
    }

    /// Mouse Keys. Returns whether the keypad key was consumed.
    fn mouse_key(
        &mut self,
        keycode: u32,
        pressed: bool,
        time: u32,
        now: u64,
        num_lock: bool,
        out: &mut Vec<AidEffect>,
    ) -> bool {
        if pressed {
            // Any key stops the motion, as in Mutter.
            self.stop_mouse();
        }
        if num_lock {
            return false;
        }
        let Some(pad) = pad_of(keycode) else {
            return false;
        };
        if !pressed {
            self.stop_mouse();
            return true;
        }
        match pad {
            Pad::Choose(button) => self.mouse_button = button,
            Pad::Click => {
                self.mouse_press(time, out);
                self.mouse_release(time, out);
            }
            Pad::DoubleClick => {
                for _ in 0..2 {
                    self.mouse_press(time, out);
                    self.mouse_release(time, out);
                }
            }
            Pad::Press => self.mouse_press(time, out),
            Pad::Release => self.mouse_release(time, out),
            Pad::Move(dx, dy) => {
                self.mouse_dir = Some((dx, dy));
                self.move_mouse(now, out);
            }
        }
        true
    }

    fn mouse_press(&mut self, time: u32, out: &mut Vec<AidEffect>) {
        let button = self.mouse_button;
        if self.mouse_down.insert(button) {
            self.counters.mouse_clicks += 1;
            out.push(AidEffect::Input(ManagerInput::Button {
                button,
                pressed: true,
                time,
            }));
        }
    }

    fn mouse_release(&mut self, time: u32, out: &mut Vec<AidEffect>) {
        let button = self.mouse_button;
        if self.mouse_down.remove(&button) {
            out.push(AidEffect::Input(ManagerInput::Button {
                button,
                pressed: false,
                time,
            }));
        }
    }

    fn stop_mouse(&mut self) {
        self.mouse_dir = None;
        self.mouse_next = None;
        self.mouse_first = None;
        self.mouse_last = 0;
    }

    /// One Mouse Keys step: the first moves a pixel at once, the next
    /// waits `mousekeys-init-delay`, then one every 100 ms, accelerating
    /// to `mousekeys-max-speed` pixels a second over
    /// `mousekeys-accel-time`.
    fn move_mouse(&mut self, now: u64, out: &mut Vec<AidEffect>) {
        let Some((dx, dy)) = self.mouse_dir else {
            self.mouse_next = None;
            return;
        };
        self.mouse_next = Some(if self.mouse_first.is_none() {
            now + u64::from(self.settings.mouse_init_delay_ms)
        } else {
            now + MOUSE_INTERVAL_MS
        });
        let speed = self.mouse_speed(now);
        let step = |d: i8| {
            let v = f64::from(d) * speed;
            if d < 0 {
                v.floor()
            } else {
                v.ceil()
            }
        };
        let (mx, my) = (step(dx), step(dy));
        if mx != 0.0 || my != 0.0 {
            self.counters.mouse_moves += 1;
            out.push(AidEffect::PointerMove {
                dx: mx,
                dy: my,
                time: self.stamp(now),
            });
        }
    }

    fn mouse_speed(&mut self, now: u64) -> f64 {
        let max_speed = f64::from(self.settings.mouse_max_speed.max(1));
        let accel = f64::from(self.settings.mouse_accel_ms.max(1));
        let Some(first) = self.mouse_first else {
            let first = now + u64::from(self.settings.mouse_init_delay_ms);
            self.mouse_first = Some(first);
            self.mouse_last = first;
            return 1.0;
        };
        if now < self.mouse_last {
            return 0.0;
        }
        let since = now.saturating_sub(first) as f64;
        let dt = (now - self.mouse_last) as f64;
        self.mouse_last = now;
        if since < accel {
            let curve = max_speed / accel.powf(MOUSE_CURVE);
            curve * since.powf(MOUSE_CURVE) * dt / 1000.0
        } else {
            max_speed * dt / 1000.0
        }
    }

    /// State for the compositor state file: switches, modifier classes
    /// and counters, never key identities.
    pub fn summary(&self) -> serde_json::Value {
        let names = |map: &BTreeMap<Modifier, u32>| -> Vec<&'static str> {
            map.keys().map(|m| m.name()).collect()
        };
        let c = self.counters;
        serde_json::json!({
            "sticky": self.sticky,
            "slow": self.slow,
            "bounce": self.settings.bounce,
            "mouse": self.settings.mouse,
            "shortcuts": self.settings.shortcuts,
            "latched": names(&self.latched),
            "locked": names(&self.locked),
            "slow_pending": self.slow_pending.len(),
            "slow_accepted": c.slow_accepted,
            "slow_rejected": c.slow_rejected,
            "bounce_rejected": c.bounce_rejected,
            "sticky_latches": c.sticky_latches,
            "mouse_moves": c.mouse_moves,
            "mouse_clicks": c.mouse_clicks,
            "mouse_buttons_down": self.mouse_down.len(),
            "toggles": c.toggles,
            "bells": c.bells,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u32 = 30;
    const B: u32 = 48;
    const CTRL: u32 = 29;
    const ALT: u32 = 56;

    fn key(keycode: u32, pressed: bool, time: u32) -> ManagerInput {
        ManagerInput::Key {
            keycode,
            pressed,
            time,
        }
    }

    /// The delivered key stream as (keycode, pressed).
    fn keys(effects: &[AidEffect]) -> Vec<(u32, bool)> {
        effects
            .iter()
            .filter_map(|e| match e {
                AidEffect::Input(ManagerInput::Key {
                    keycode, pressed, ..
                }) => Some((*keycode, *pressed)),
                _ => None,
            })
            .collect()
    }

    fn buttons(effects: &[AidEffect]) -> Vec<(u32, bool)> {
        effects
            .iter()
            .filter_map(|e| match e {
                AidEffect::Input(ManagerInput::Button {
                    button, pressed, ..
                }) => Some((*button, *pressed)),
                _ => None,
            })
            .collect()
    }

    fn bells(effects: &[AidEffect]) -> usize {
        effects.iter().filter(|e| **e == AidEffect::Bell).count()
    }

    fn toggles(effects: &[AidEffect]) -> Vec<(KeyboardAid, bool)> {
        effects
            .iter()
            .filter_map(|e| match e {
                AidEffect::Toggled { aid, enabled } => Some((*aid, *enabled)),
                _ => None,
            })
            .collect()
    }

    /// Run a script of (keycode, pressed, ms) through the aids, polling
    /// the timers at each step, and collect every effect.
    fn run(aids: &mut InputAids, script: &[(u32, bool, u64)]) -> Vec<AidEffect> {
        let mut out = Vec::new();
        for &(keycode, pressed, at) in script {
            out.extend(aids.poll(at));
            out.extend(aids.process(key(keycode, pressed, at as u32), at, false));
        }
        out
    }

    fn with(settings: KeyboardAids) -> InputAids {
        let mut aids = InputAids::new();
        assert!(aids.configure(&settings, 0).is_empty());
        aids
    }

    fn tap(keycode: u32, at: u64) -> [(u32, bool, u64); 2] {
        [(keycode, true, at), (keycode, false, at + 10)]
    }

    #[test]
    fn everything_passes_untouched_with_the_aids_off() {
        let mut aids = InputAids::new();
        assert!(!aids.active());
        let script = [
            (SHIFT_LEFT, true, 0),
            (A, true, 5),
            (A, false, 6),
            (SHIFT_LEFT, false, 7),
            (A, true, 8),
            (A, false, 9),
        ];
        let out = run(&mut aids, &script);
        assert_eq!(
            keys(&out),
            script.iter().map(|(k, p, _)| (*k, *p)).collect::<Vec<_>>()
        );
        let motion = ManagerInput::Motion {
            pos: (3.0, 4.0).into(),
            time: 1,
        };
        assert_eq!(
            aids.process(motion, 10, false),
            vec![AidEffect::Input(motion)]
        );
    }

    #[test]
    fn sticky_shift_latches_for_one_key_then_releases() {
        let mut aids = with(KeyboardAids {
            sticky: true,
            ..KeyboardAids::default()
        });
        let mut script = tap(SHIFT_LEFT, 0).to_vec();
        script.extend(tap(A, 100));
        script.extend(tap(B, 200));
        let out = run(&mut aids, &script);
        // Shift stays down across A, comes up after A, and B is plain.
        assert_eq!(
            keys(&out),
            vec![
                (SHIFT_LEFT, true),
                (A, true),
                (A, false),
                (SHIFT_LEFT, false),
                (B, true),
                (B, false),
            ]
        );
        assert_eq!(aids.summary()["latched"], serde_json::json!([]));
    }

    #[test]
    fn sticky_second_tap_locks_and_third_unlocks() {
        let mut aids = with(KeyboardAids {
            sticky: true,
            ..KeyboardAids::default()
        });
        let mut script = tap(CTRL, 0).to_vec();
        script.extend(tap(CTRL, 50));
        let out = run(&mut aids, &script);
        assert_eq!(keys(&out), vec![(CTRL, true)]);
        assert_eq!(aids.summary()["locked"], serde_json::json!(["control"]));
        // Locked: survives several keys.
        let mut script = tap(A, 100).to_vec();
        script.extend(tap(B, 200));
        let out = run(&mut aids, &script);
        assert_eq!(
            keys(&out),
            vec![(A, true), (A, false), (B, true), (B, false)]
        );
        // Third tap releases it.
        let out = run(&mut aids, &tap(CTRL, 300));
        assert_eq!(keys(&out), vec![(CTRL, false)]);
        assert_eq!(aids.summary()["locked"], serde_json::json!([]));
    }

    #[test]
    fn sticky_modifier_used_in_a_chord_does_not_latch() {
        let mut aids = with(KeyboardAids {
            sticky: true,
            ..KeyboardAids::default()
        });
        let out = run(
            &mut aids,
            &[
                (SHIFT_LEFT, true, 0),
                (A, true, 10),
                (A, false, 20),
                (SHIFT_LEFT, false, 30),
            ],
        );
        assert_eq!(
            keys(&out),
            vec![
                (SHIFT_LEFT, true),
                (A, true),
                (A, false),
                (SHIFT_LEFT, false)
            ]
        );
        assert_eq!(aids.summary()["latched"], serde_json::json!([]));
    }

    #[test]
    fn sticky_two_modifiers_together_latch_both_or_switch_the_aid_off() {
        let mut aids = with(KeyboardAids {
            sticky: true,
            ..KeyboardAids::default()
        });
        let out = run(
            &mut aids,
            &[
                (CTRL, true, 0),
                (ALT, true, 10),
                (ALT, false, 20),
                (CTRL, false, 30),
            ],
        );
        assert_eq!(keys(&out), vec![(CTRL, true), (ALT, true)]);
        assert_eq!(
            aids.summary()["latched"],
            serde_json::json!(["control", "alt"])
        );
        let out = run(&mut aids, &tap(A, 100));
        assert_eq!(
            keys(&out),
            vec![(A, true), (A, false), (CTRL, false), (ALT, false)]
        );

        // stickykeys-two-key-off: the second modifier ends the aid, its
        // latches released, and the shell hears so.
        let mut aids = with(KeyboardAids {
            sticky: true,
            sticky_two_key_off: true,
            ..KeyboardAids::default()
        });
        run(&mut aids, &tap(SHIFT_LEFT, 0));
        let out = run(&mut aids, &[(CTRL, true, 100), (ALT, true, 110)]);
        assert_eq!(
            keys(&out),
            vec![(CTRL, true), (SHIFT_LEFT, false), (ALT, true)]
        );
        assert_eq!(toggles(&out), vec![(KeyboardAid::StickyKeys, false)]);
        // Off now: plain pass-through, releases included.
        let out = run(&mut aids, &[(ALT, false, 120), (CTRL, false, 130)]);
        assert_eq!(keys(&out), vec![(ALT, false), (CTRL, false)]);
        assert_eq!(aids.summary()["sticky"], false);
    }

    #[test]
    fn switching_sticky_off_releases_latched_and_locked_modifiers() {
        let on = KeyboardAids {
            sticky: true,
            ..KeyboardAids::default()
        };
        let mut aids = with(on.clone());
        run(&mut aids, &tap(SHIFT_LEFT, 0));
        run(&mut aids, &tap(CTRL, 20));
        run(&mut aids, &tap(CTRL, 40));
        let out = aids.configure(&KeyboardAids::default(), 100);
        let mut released = keys(&out);
        released.sort();
        assert_eq!(released, vec![(CTRL, false), (SHIFT_LEFT, false)]);
        assert!(toggles(&out).is_empty());
    }

    #[test]
    fn sticky_beep_sounds_on_each_latch_change() {
        let mut aids = with(KeyboardAids {
            sticky: true,
            sticky_beep: true,
            ..KeyboardAids::default()
        });
        let out = run(&mut aids, &tap(SHIFT_LEFT, 0));
        assert_eq!(bells(&out), 1);
        let out = run(&mut aids, &tap(A, 50));
        assert_eq!(bells(&out), 0);
    }

    #[test]
    fn slow_keys_deliver_only_keys_held_past_the_delay() {
        let mut aids = with(KeyboardAids {
            slow: true,
            slow_delay_ms: 300,
            slow_beep_press: true,
            slow_beep_accept: true,
            slow_beep_reject: true,
            ..KeyboardAids::default()
        });
        // A short tap never reaches anyone.
        let out = run(&mut aids, &[(A, true, 0), (A, false, 100)]);
        assert!(keys(&out).is_empty());
        assert_eq!(bells(&out), 2, "press and reject beeps");
        // A held key arrives once the delay passes, then its release.
        let mut out = run(&mut aids, &[(B, true, 1000)]);
        out.extend(aids.poll(1299));
        assert!(keys(&out).is_empty());
        out.extend(aids.poll(1300));
        out.extend(run(&mut aids, &[(B, false, 1500)]));
        assert_eq!(keys(&out), vec![(B, true), (B, false)]);
        let summary = aids.summary();
        assert_eq!(summary["slow_accepted"], 1);
        assert_eq!(summary["slow_rejected"], 1);
        // Switching the aid off drops a pending press.
        run(&mut aids, &[(A, true, 2000)]);
        aids.configure(&KeyboardAids::default(), 2010);
        assert!(keys(&aids.poll(5000)).is_empty());
    }

    #[test]
    fn slow_accepted_modifiers_still_latch_under_sticky_keys() {
        let mut aids = with(KeyboardAids {
            slow: true,
            slow_delay_ms: 200,
            sticky: true,
            ..KeyboardAids::default()
        });
        let mut out = run(&mut aids, &[(SHIFT_LEFT, true, 0)]);
        out.extend(aids.poll(250));
        out.extend(run(&mut aids, &[(SHIFT_LEFT, false, 300)]));
        assert_eq!(keys(&out), vec![(SHIFT_LEFT, true)]);
        assert_eq!(aids.summary()["latched"], serde_json::json!(["shift"]));
    }

    #[test]
    fn bounce_keys_drop_a_quick_repeat_of_the_same_key() {
        let mut aids = with(KeyboardAids {
            bounce: true,
            bounce_delay_ms: 300,
            bounce_beep_reject: true,
            ..KeyboardAids::default()
        });
        let mut script = tap(A, 0).to_vec();
        script.extend(tap(A, 100)); // bounces
        script.extend(tap(B, 150)); // another key is fine
        script.extend(tap(A, 600)); // after the window
        let out = run(&mut aids, &script);
        assert_eq!(
            keys(&out),
            vec![
                (A, true),
                (A, false),
                (B, true),
                (B, false),
                (A, true),
                (A, false)
            ]
        );
        assert_eq!(bells(&out), 1);
        assert_eq!(aids.summary()["bounce_rejected"], 1);
        // The window restarts on every release, the dropped one included.
        let mut script = tap(A, 1000).to_vec();
        script.extend(tap(A, 1200));
        script.extend(tap(A, 1400));
        let out = run(&mut aids, &script);
        assert_eq!(keys(&out), vec![(A, true), (A, false)]);
    }

    #[test]
    fn shift_five_times_switches_sticky_keys_and_back() {
        let mut aids = with(KeyboardAids {
            shortcuts: true,
            feature_beep: true,
            ..KeyboardAids::default()
        });
        let mut script = Vec::new();
        for i in 0..5 {
            script.extend(tap(SHIFT_LEFT, i * 100));
        }
        let out = run(&mut aids, &script);
        assert_eq!(toggles(&out), vec![(KeyboardAid::StickyKeys, true)]);
        assert_eq!(bells(&out), 1);
        assert_eq!(aids.summary()["sticky"], true);
        // The setting echoing back changes nothing.
        let echoed = KeyboardAids {
            shortcuts: true,
            feature_beep: true,
            sticky: true,
            ..KeyboardAids::default()
        };
        assert!(aids.configure(&echoed, 600).is_empty());
        assert_eq!(aids.summary()["sticky"], true);
        // Five more switch it off again.
        let mut script = Vec::new();
        for i in 0..5 {
            script.extend(tap(SHIFT_LEFT, 1000 + i * 100));
        }
        let out = run(&mut aids, &script);
        assert_eq!(toggles(&out), vec![(KeyboardAid::StickyKeys, false)]);
    }

    #[test]
    fn shift_run_breaks_on_other_keys_and_slow_runs_and_without_the_setting() {
        let mut aids = with(KeyboardAids {
            shortcuts: true,
            ..KeyboardAids::default()
        });
        let mut script = Vec::new();
        for i in 0..4 {
            script.extend(tap(SHIFT_LEFT, i * 100));
        }
        script.extend(tap(A, 450));
        script.extend(tap(SHIFT_LEFT, 500));
        assert!(toggles(&run(&mut aids, &script)).is_empty());
        // More than fifteen seconds between presses starts a new run.
        let mut script = Vec::new();
        for i in 0..5 {
            script.extend(tap(SHIFT_RIGHT, 10_000 + i * 16_000));
        }
        assert!(toggles(&run(&mut aids, &script)).is_empty());
        // Without `enable` the keyboard switches nothing.
        let mut aids = InputAids::new();
        let mut script = Vec::new();
        for i in 0..5 {
            script.extend(tap(SHIFT_LEFT, i * 100));
        }
        assert!(toggles(&run(&mut aids, &script)).is_empty());
    }

    #[test]
    fn shift_held_eight_seconds_switches_slow_keys() {
        let mut aids = with(KeyboardAids {
            shortcuts: true,
            ..KeyboardAids::default()
        });
        run(&mut aids, &[(SHIFT_LEFT, true, 0)]);
        assert!(toggles(&aids.poll(7_999)).is_empty());
        let out = aids.poll(8_000);
        assert_eq!(toggles(&out), vec![(KeyboardAid::SlowKeys, true)]);
        assert_eq!(aids.summary()["slow"], true);
        // The shortcut's own release is delivered: no slow press waits
        // for it.
        let out = run(&mut aids, &[(SHIFT_LEFT, false, 9_000)]);
        assert_eq!(keys(&out), vec![(SHIFT_LEFT, false)]);
        // Releasing early cancels.
        run(
            &mut aids,
            &[(SHIFT_RIGHT, true, 10_000), (SHIFT_RIGHT, false, 17_000)],
        );
        assert!(toggles(&aids.poll(19_000)).is_empty());
        assert_eq!(aids.summary()["slow"], true);
    }

    #[test]
    fn toggle_keys_beep_on_caps_and_num_lock() {
        let mut aids = with(KeyboardAids {
            toggle_beep: true,
            ..KeyboardAids::default()
        });
        let mut script = tap(CAPS_LOCK, 0).to_vec();
        script.extend(tap(NUM_LOCK, 50));
        script.extend(tap(A, 100));
        let out = run(&mut aids, &script);
        assert_eq!(bells(&out), 2);
        assert_eq!(keys(&out).len(), 6);
    }

    fn mouse() -> InputAids {
        with(KeyboardAids {
            mouse: true,
            mouse_max_speed: 1000,
            mouse_accel_ms: 1000,
            mouse_init_delay_ms: 300,
            ..KeyboardAids::default()
        })
    }

    fn moves(effects: &[AidEffect]) -> Vec<(f64, f64)> {
        effects
            .iter()
            .filter_map(|e| match e {
                AidEffect::PointerMove { dx, dy, .. } => Some((*dx, *dy)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn mouse_keys_move_and_accelerate_until_release() {
        let mut aids = mouse();
        // KP6: one pixel at once, consumed.
        let out = aids.process(key(77, true, 0), 0, false);
        assert!(keys(&out).is_empty());
        assert_eq!(moves(&out), vec![(1.0, 0.0)]);
        // Nothing before the initial delay.
        assert!(moves(&aids.poll(299)).is_empty());
        // Then steps every 100 ms that grow toward max speed.
        let mut steps = Vec::new();
        for t in (300..=1500).step_by(100) {
            steps.extend(moves(&aids.poll(t)));
        }
        let xs: Vec<f64> = steps.iter().map(|(x, _)| *x).collect();
        assert!(xs.windows(2).all(|w| w[1] >= w[0]), "{xs:?}");
        assert_eq!(*xs.last().unwrap(), 100.0, "1000 px/s at 100 ms steps");
        assert!(steps.iter().all(|(_, y)| *y == 0.0));
        // Release stops it; the release is consumed too.
        let out = aids.process(key(77, false, 1550), 1550, false);
        assert!(keys(&out).is_empty());
        assert!(moves(&aids.poll(3000)).is_empty());
        // Diagonals and negative directions round away from zero.
        let out = aids.process(key(71, true, 4000), 4000, false);
        assert_eq!(moves(&out), vec![(-1.0, -1.0)]);
    }

    #[test]
    fn mouse_keys_click_press_release_and_choose_buttons() {
        let mut aids = mouse();
        let mut out = Vec::new();
        for (k, at) in [(76, 0), (74, 100), (78, 200), (98, 300), (82, 400)] {
            out.extend(aids.process(key(k, true, at as u32), at, false));
            out.extend(aids.process(key(k, false, at as u32 + 5), at + 5, false));
        }
        assert!(keys(&out).is_empty(), "keypad keys are consumed");
        assert_eq!(
            buttons(&out),
            vec![
                (BUTTON_PRIMARY, true),
                (BUTTON_PRIMARY, false),
                (BUTTON_SECONDARY, true),
                (BUTTON_SECONDARY, false),
                (BUTTON_SECONDARY, true),
                (BUTTON_SECONDARY, false),
                (BUTTON_PRIMARY, true),
            ]
        );
        // Switching the aid off releases the held button.
        let out = aids.configure(&KeyboardAids::default(), 600);
        assert_eq!(buttons(&out), vec![(BUTTON_PRIMARY, false)]);
    }

    #[test]
    fn mouse_keys_stand_aside_for_num_lock_and_other_keys() {
        let mut aids = mouse();
        let out = aids.process(key(77, true, 0), 0, true);
        assert_eq!(keys(&out), vec![(77, true)]);
        assert!(moves(&out).is_empty());
        let out = aids.process(key(A, true, 10), 10, false);
        assert_eq!(keys(&out), vec![(A, true)]);
        // Another key stops a running motion.
        aids.process(key(72, true, 100), 100, false);
        aids.process(key(A, true, 150), 150, false);
        assert!(moves(&aids.poll(1000)).is_empty());
    }

    #[test]
    fn synthesized_events_carry_backend_stamps() {
        let mut aids = with(KeyboardAids {
            slow: true,
            slow_delay_ms: 300,
            ..KeyboardAids::default()
        });
        // Backend clock 5000 at our clock 100.
        aids.process(key(A, true, 5000), 100, false);
        let out = aids.poll(400);
        assert_eq!(
            out,
            vec![AidEffect::Input(ManagerInput::Key {
                keycode: A,
                pressed: true,
                time: 5300
            })]
        );
    }
}
