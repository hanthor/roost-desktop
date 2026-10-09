//! The time base for compositor animations.
//!
//! Sessions animate on the monotonic clock. Nested proofs that also set
//! `TUNA_COMPOSITOR_STATE` can name an absolute file path in
//! `TUNA_ANIMATION_CLOCK` to take over animation time:
//!
//! - while the file is absent, animation time is real time;
//! - when it first holds a whole number of milliseconds `N`, animation
//!   time freezes at the moment it was read plus `N`;
//! - rewriting it with another `M` moves time to that moment plus `M`,
//!   so a transition can be sampled at exact delays however slowly
//!   llvmpipe renders;
//! - removing it resumes real time from where the clock stood.
//!
//! Animation time never runs backwards across those switches. Only
//! animations use it: input, idle and frame timing keep real time.
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Environment variable naming the clock file.
pub const CLOCK_ENV: &str = "TUNA_ANIMATION_CLOCK";

#[derive(Debug)]
enum Mode {
    /// `offset` plus real time since `origin`.
    Real { origin: Instant, offset: Duration },
    /// Frozen at `now`; file values count from `base`.
    Manual { base: Duration, now: Duration },
}

#[derive(Debug)]
pub struct AnimationClock {
    mode: Mode,
    /// The clock file, when a proof may drive the clock.
    path: Option<PathBuf>,
    /// The file body last applied, so each write applies once.
    last: String,
}

impl AnimationClock {
    /// Real time, starting at zero.
    pub fn real() -> Self {
        Self {
            mode: Mode::Real {
                origin: Instant::now(),
                offset: Duration::ZERO,
            },
            path: None,
            last: String::new(),
        }
    }

    /// A clock frozen at zero for tests, moved only by [`Self::set`].
    pub fn manual() -> Self {
        Self {
            mode: Mode::Manual {
                base: Duration::ZERO,
                now: Duration::ZERO,
            },
            path: None,
            last: String::new(),
        }
    }

    /// Real time, drivable through `TUNA_ANIMATION_CLOCK` when the
    /// session is `instrumented` (state file on) and the path is absolute.
    pub fn from_env(instrumented: bool) -> Self {
        Self::with_file(
            std::env::var_os(CLOCK_ENV)
                .map(PathBuf::from)
                .filter(|p| instrumented && p.is_absolute()),
        )
    }

    fn with_file(path: Option<PathBuf>) -> Self {
        Self {
            path,
            ..Self::real()
        }
    }

    /// Whether time is frozen (a proof or test is driving it).
    pub fn is_manual(&self) -> bool {
        matches!(self.mode, Mode::Manual { .. })
    }

    /// Animation time since the clock started.
    pub fn now(&self) -> Duration {
        match self.mode {
            Mode::Real { origin, offset } => offset + origin.elapsed(),
            Mode::Manual { now, .. } => now,
        }
    }

    /// Freeze the clock and move it to `base + to` (tests).
    pub fn set(&mut self, to: Duration) {
        let base = match self.mode {
            Mode::Manual { base, .. } => base,
            Mode::Real { .. } => self.now(),
        };
        self.mode = Mode::Manual {
            base,
            now: base + to,
        };
    }

    /// Resume real time from where the clock stands.
    pub fn resume(&mut self) {
        if let Mode::Manual { now, .. } = self.mode {
            self.mode = Mode::Real {
                origin: Instant::now(),
                offset: now,
            };
        }
    }

    /// Follow the clock file once per change (called every loop pass).
    pub fn poll(&mut self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        match crate::runtime_signal::read(path) {
            Ok(body) => {
                if body == self.last {
                    return;
                }
                if let Ok(ms) = body.trim().parse::<u64>() {
                    self.set(Duration::from_millis(ms));
                }
                self.last = body;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.last.clear();
                self.resume();
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manual_clock_stays_frozen_until_stepped() {
        let mut clock = AnimationClock::manual();
        assert!(clock.is_manual());
        assert_eq!(clock.now(), Duration::ZERO);
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(clock.now(), Duration::ZERO, "real time does not leak in");
        clock.set(Duration::from_millis(125));
        assert_eq!(clock.now(), Duration::from_millis(125));
        clock.poll();
        assert_eq!(
            clock.now(),
            Duration::from_millis(125),
            "no file, no change"
        );
        clock.resume();
        assert!(!clock.is_manual());
        assert!(clock.now() >= Duration::from_millis(125));
    }

    #[test]
    fn the_real_clock_runs() {
        let clock = AnimationClock::real();
        assert!(!clock.is_manual());
        let before = clock.now();
        std::thread::sleep(Duration::from_millis(2));
        assert!(clock.now() > before);
    }

    #[test]
    fn a_file_freezes_steps_and_releases_the_clock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clock");
        let mut clock = AnimationClock::with_file(Some(path.clone()));
        clock.poll();
        assert!(!clock.is_manual(), "no file: real time");
        std::fs::write(&path, "0\n").unwrap();
        clock.poll();
        assert!(clock.is_manual());
        let base = clock.now();
        std::thread::sleep(Duration::from_millis(5));
        clock.poll();
        assert_eq!(clock.now(), base, "frozen while the file is unchanged");
        std::fs::write(&path, "125").unwrap();
        clock.poll();
        assert_eq!(clock.now() - base, Duration::from_millis(125));
        std::fs::write(&path, "not a time").unwrap();
        clock.poll();
        assert_eq!(
            clock.now() - base,
            Duration::from_millis(125),
            "garbage is ignored"
        );
        std::fs::write(&path, "250").unwrap();
        clock.poll();
        assert_eq!(clock.now() - base, Duration::from_millis(250));
        std::fs::remove_file(&path).unwrap();
        clock.poll();
        assert!(!clock.is_manual(), "removing the file resumes real time");
        assert!(
            clock.now() >= base + Duration::from_millis(250),
            "never backwards"
        );
        // A new file freezes again from the current time.
        std::fs::write(&path, "0").unwrap();
        clock.poll();
        assert!(clock.is_manual() && clock.now() >= base + Duration::from_millis(250));
    }

    #[test]
    fn only_instrumented_sessions_read_the_variable() {
        assert!(AnimationClock::from_env(false).path.is_none());
    }
}
