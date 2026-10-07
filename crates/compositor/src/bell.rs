//! GNOME 51 bell policy and bounded playback. Rendering is compositor-owned;
//! sound lookup, process creation and reaping stay on one bounded worker.
use std::{
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc,
    },
    time::{Duration, Instant},
};

use roost_shell_control::BellPreferences;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

pub const BELL_SOUND: &str = "bell-window-system";

pub(crate) struct Visual {
    pub surface: Option<WlSurface>,
    pub fullscreen: bool,
    /// A hidden/destroyed window never turns its frame alert into a stage alert.
    pub window_target: bool,
    start: Instant,
    repeats: u8,
}

impl Visual {
    pub fn alpha(&self, now: Instant) -> f32 {
        flash_alpha(now.saturating_duration_since(self.start), self.repeats)
    }
}

/// Mutter51 compositor.c uses 50ms ease-in-quad legs, opacity192/255,
/// auto-reverse and Clutter repeat_count1/2 (two/three total legs).
fn flash_alpha(elapsed: Duration, repeats: u8) -> f32 {
    let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
    let leg = (elapsed_ms / 50.0).floor() as u64;
    if leg > u64::from(repeats) {
        return 0.0;
    }
    let progress = (elapsed_ms % 50.0) / 50.0;
    let progress = if leg % 2 == 0 {
        progress
    } else {
        1.0 - progress
    };
    ((192.0 / 255.0) * progress * progress) as f32
}

#[derive(Default)]
pub(crate) struct Bell {
    pub rings: u64,
    pub visual_requests: u64,
    policy: BellPreferences,
    locked: bool,
    last_sound: Option<Instant>,
    last_visual: Option<Instant>,
    visual: Option<Visual>,
    audio: Option<Audio>,
}

impl Bell {
    pub fn set_policy(&mut self, mut policy: BellPreferences) {
        // Bound wire-controlled allocation; an invalid theme fails closed.
        if policy.theme.len() > 256 || policy.theme.contains('\0') {
            policy.event_sounds = false;
            policy.theme.clear();
        }
        if self.policy != policy {
            if let Some(audio) = self.audio.as_ref().filter(|_| {
                self.policy.audible != policy.audible
                    || self.policy.event_sounds != policy.event_sounds
                    || self.policy.theme != policy.theme
            }) {
                audio.epoch.fetch_add(1, Ordering::AcqRel);
            }
            if self.policy.visual != policy.visual || self.policy.fullscreen != policy.fullscreen {
                self.visual = None;
            }
            self.policy = policy;
        }
    }

    pub fn set_locked(&mut self, locked: bool) {
        if self.locked != locked {
            self.locked = locked;
            self.visual = None;
            if let Some(audio) = &self.audio {
                audio.epoch.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    pub fn ring(&mut self, surface: Option<WlSurface>, window_target: bool) {
        self.rings = self.rings.saturating_add(1);
        if self.locked {
            return;
        }
        let now = Instant::now();
        self.visual_at(surface, window_target, now);
        if !self.policy.audible
            || !self.policy.event_sounds
            || std::env::var_os("ROOST_BELL").is_some_and(|v| v == "0")
            || self
                .last_sound
                .is_some_and(|last| now.duration_since(last) < Duration::from_millis(100))
        {
            return;
        }
        self.last_sound = Some(now);
        if self.audio.is_none() {
            self.audio = Audio::new();
        }
        let Some(audio) = self.audio.as_ref() else {
            return;
        };
        let _ = audio.send.try_send(Sound {
            epoch: audio.epoch.load(Ordering::Acquire),
            theme: self.policy.theme.clone(),
        });
    }

    fn visual_at(&mut self, surface: Option<WlSurface>, window_target: bool, now: Instant) {
        if !self.policy.visual {
            return;
        }
        let elapsed = self.last_visual.map(|last| now.duration_since(last));
        // GNOME51 bell.c: global500ms spacing, double pattern only after3s.
        if elapsed.is_some_and(|dt| dt < Duration::from_millis(500)) {
            return;
        }
        self.last_visual = Some(now);
        self.visual_requests = self.visual_requests.saturating_add(1);
        self.visual = Some(Visual {
            surface,
            fullscreen: self.policy.fullscreen,
            window_target,
            start: now,
            repeats: if elapsed.is_some_and(|dt| dt < Duration::from_secs(3)) {
                1
            } else {
                2
            },
        });
    }

    pub fn visual(&self) -> Option<&Visual> {
        if self.locked || !self.policy.visual {
            return None;
        }
        self.visual.as_ref().filter(|v| {
            Instant::now().saturating_duration_since(v.start)
                < Duration::from_millis(50 * (u64::from(v.repeats) + 1))
        })
    }
}

struct Sound {
    epoch: u64,
    theme: String,
}

struct Audio {
    send: SyncSender<Sound>,
    epoch: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl Audio {
    fn new() -> Option<Self> {
        let (send, receive) = sync_channel(1);
        let epoch = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_epoch, worker_stop) = (epoch.clone(), stop.clone());
        std::thread::Builder::new()
            .name("roost-bell".into())
            .spawn(move || play(receive, worker_epoch, worker_stop))
            .ok()?;
        Some(Self { send, epoch, stop })
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        // The worker polls every10ms and kills/reaps its own child;
        // destruction on the display thread never waits for a player.
        self.stop.store(true, Ordering::Release);
    }
}

fn play(receive: Receiver<Sound>, epoch: Arc<AtomicU64>, stop: Arc<AtomicBool>) {
    let mut child: Option<(Child, Instant, u64)> = None;
    loop {
        if let Some((process, started, generation)) = child.as_mut() {
            let expired = stop.load(Ordering::Acquire)
                || epoch.load(Ordering::Acquire) != *generation
                || started.elapsed() >= Duration::from_secs(5);
            if expired {
                let _ = process.kill();
                let _ = process.wait();
                child = None;
            } else if process.try_wait().is_ok_and(|status| status.is_some()) {
                child = None;
            }
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        match receive.recv_timeout(Duration::from_millis(10)) {
            Ok(sound) => {
                if child.is_some() || epoch.load(Ordering::Acquire) != sound.epoch {
                    continue;
                }
                let mut command = Command::new("canberra-gtk-play");
                command
                    .args([
                        "--id",
                        BELL_SOUND,
                        "--description",
                        "Bell event",
                        "--property",
                        "canberra.enable=1",
                        "--property",
                    ])
                    .arg(format!("canberra.xdg-theme.name={}", sound.theme))
                    .env("GDK_BACKEND", "wayland")
                    .env_remove("DISPLAY")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                use std::os::unix::process::CommandExt;
                let owner = rustix::process::getpid();
                // SAFETY: only async-signal-safe syscalls in the child.
                unsafe {
                    command.pre_exec(move || {
                        rustix::process::set_parent_process_death_signal(Some(
                            rustix::process::Signal::KILL,
                        ))?;
                        if rustix::process::getppid() != Some(owner) {
                            return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
                        }
                        Ok(())
                    });
                }
                if let Ok(process) = command.spawn() {
                    child = Some((process, Instant::now(), sound.epoch));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                stop.store(true, Ordering::Release);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_gnome_alpha_and_timeline_leg_counts() {
        let ms = Duration::from_millis;
        assert_eq!(flash_alpha(ms(0), 1), 0.0);
        assert!((flash_alpha(ms(25), 1) - (192.0 / 255.0 / 4.0)).abs() < 0.000001);
        assert_eq!(flash_alpha(ms(50), 1), 192.0 / 255.0);
        assert_eq!(flash_alpha(ms(100), 1), 0.0);
        assert!(flash_alpha(ms(125), 2) > 0.0);
        assert_eq!(flash_alpha(ms(150), 2), 0.0);
    }

    #[test]
    fn visual_spacing_is_global_and_sound_policy_is_independent() {
        let mut bell = Bell::default();
        bell.policy.visual = true;
        bell.policy.audible = false;
        let start = Instant::now();
        bell.visual_at(None, false, start);
        assert_eq!(bell.visual.as_ref().unwrap().repeats, 2);
        bell.visual_at(None, false, start + Duration::from_millis(499));
        assert_eq!(bell.visual.as_ref().unwrap().start, start);
        bell.visual_at(None, false, start + Duration::from_millis(500));
        assert_eq!(bell.visual.as_ref().unwrap().repeats, 1);
        bell.visual_at(None, false, start + Duration::from_millis(3500));
        assert_eq!(bell.visual.as_ref().unwrap().repeats, 2);
    }

    #[test]
    fn lock_and_policy_changes_cancel_old_visuals() {
        let mut bell = Bell::default();
        let policy = BellPreferences {
            visual: true,
            audible: false,
            ..Default::default()
        };
        bell.set_policy(policy.clone());
        bell.ring(None, false);
        assert!(bell.visual.is_some());
        bell.set_locked(true);
        assert!(bell.visual.is_none());
        bell.ring(None, false);
        assert!(bell.visual.is_none());
        bell.set_locked(false);
        bell.set_policy(BellPreferences::default());
        assert!(bell.visual().is_none());
    }

    #[test]
    fn sound_preferences_do_not_cancel_an_independent_visual_alert() {
        let mut bell = Bell::default();
        bell.set_policy(BellPreferences {
            visual: true,
            audible: false,
            ..Default::default()
        });
        bell.ring(None, false);
        let start = bell.visual.as_ref().unwrap().start;
        bell.set_policy(BellPreferences {
            visual: true,
            audible: true,
            event_sounds: false,
            theme: "custom".into(),
            ..Default::default()
        });
        assert_eq!(bell.visual.as_ref().unwrap().start, start);
        assert!(bell.audio.is_none());
    }
}
