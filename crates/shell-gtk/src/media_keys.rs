//! Native GNOME media bindings when the session has no gsd-media-keys.
//! Mutations are serialized and each external call has a five-second bound.
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use gio::prelude::*;
use gtk4::{gio, glib};

const AMPLIFIED_KEY: &str = "allow-volume-above-100-percent";

pub fn sound_settings() -> Option<gio::Settings> {
    let schema = gio::SettingsSchemaSource::default()?.lookup("org.gnome.desktop.sound", true)?;
    schema
        .has_key(AMPLIFIED_KEY)
        .then(|| gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None))
}

pub fn output_limit(settings: Option<&gio::Settings>) -> f64 {
    crate::logic::volume_limit(settings.is_some_and(|s| s.boolean(AMPLIFIED_KEY)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeKey {
    Up,
    Down,
    Mute,
}

fn parse_volume(output: &str) -> Option<(f64, bool)> {
    let mut words = output.split_whitespace();
    if words.next()? != "Volume:" {
        return None;
    }
    let percent = words.next()?.parse::<f64>().ok()? * 100.0;
    percent
        .is_finite()
        .then_some((percent, output.contains("[MUTED]")))
}

/// GNOME unmutes without raising an existing nonzero level; lowering to
/// zero mutes it. The ordinary range stops at 100 percent.
fn adjusted(percent: f64, muted: bool, key: VolumeKey, step: f64, limit: f64) -> (f64, bool) {
    match key {
        VolumeKey::Mute => (percent, !muted),
        VolumeKey::Up => (
            (if muted && percent > 0.0 {
                percent
            } else {
                percent + step
            })
            .clamp(0.0, limit),
            false,
        ),
        VolumeKey::Down => {
            let percent = (percent - step).clamp(0.0, limit);
            (percent, muted || percent == 0.0)
        }
    }
}

type Osd = Rc<dyn Fn(f64, bool, bool)>;
#[derive(Clone)]
pub struct MediaKeys(Rc<Inner>);
struct Inner {
    queue: RefCell<VecDeque<(VolumeKey, f64, bool)>>,
    busy: Cell<bool>,
    osd: Osd,
    sound: Option<gio::Settings>,
}
impl MediaKeys {
    pub fn new(osd: Osd) -> Self {
        Self(Rc::new(Inner {
            queue: RefCell::new(VecDeque::new()),
            busy: Cell::new(false),
            osd,
            sound: sound_settings(),
        }))
    }
    pub fn change(&self, key: VolumeKey, step: f64, microphone: bool) {
        // Bound repeated input while a missing audio service is timing out.
        if self.0.queue.borrow().len() >= 32 {
            return;
        }
        self.0.queue.borrow_mut().push_back((key, step, microphone));
        self.next();
    }
    fn finish(&self) {
        self.0.busy.set(false);
        self.next();
    }
    fn next(&self) {
        if self.0.busy.get() {
            return;
        }
        let Some((key, step, microphone)) = self.0.queue.borrow_mut().pop_front() else {
            return;
        };
        self.0.busy.set(true);
        let target = if microphone {
            "@DEFAULT_AUDIO_SOURCE@"
        } else {
            "@DEFAULT_AUDIO_SINK@"
        };
        let this = self.clone();
        wpctl(&["get-volume", target], move |out| {
            let Some((percent, muted)) = out.as_deref().and_then(parse_volume) else {
                this.finish();
                return;
            };
            let (percent, muted) = adjusted(
                percent,
                muted,
                key,
                step,
                if microphone {
                    100.0
                } else {
                    output_limit(this.0.sound.as_ref())
                },
            );
            let volume = format!("{:.4}", percent / 100.0);
            wpctl(&["set-volume", target, &volume], move |success| {
                if success.is_none() {
                    this.finish();
                    return;
                }
                wpctl(
                    &["set-mute", target, if muted { "1" } else { "0" }],
                    move |success| {
                        if success.is_none() {
                            this.finish();
                            return;
                        }
                        wpctl(&["get-volume", target], move |out| {
                            if let Some((percent, muted)) = out.as_deref().and_then(parse_volume) {
                                (this.0.osd)(percent, muted, microphone);
                            }
                            this.finish();
                        });
                    },
                );
            });
        });
    }
}

fn wpctl(args: &[&str], done: impl FnOnce(Option<String>) + 'static) {
    let mut argv = vec![std::ffi::OsStr::new("wpctl")];
    argv.extend(args.iter().map(std::ffi::OsStr::new));
    let Ok(process) = gio::Subprocess::newv(
        &argv,
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    ) else {
        done(None);
        return;
    };
    let timer = Rc::new(RefCell::new(None));
    let timeout_process = process.clone();
    let timeout_timer = timer.clone();
    *timer.borrow_mut() = Some(glib::timeout_add_local_once(
        std::time::Duration::from_secs(5),
        move || {
            timeout_timer.borrow_mut().take();
            timeout_process.force_exit();
        },
    ));
    let read_process = process.clone();
    process.communicate_utf8_async(None, gio::Cancellable::NONE, move |result| {
        if let Some(timer) = timer.borrow_mut().take() {
            timer.remove();
        }
        let output = result
            .ok()
            .filter(|_| read_process.is_successful())
            .map(|(out, _)| out.map_or_else(String::new, |out| out.to_string()));
        done(output);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn amplified_output_obeys_live_limit_and_preserves_unmute_level() {
        let max = crate::logic::volume_limit(true);
        assert_eq!(
            adjusted(98.0, false, VolumeKey::Up, 6.0, max),
            (104.0, false)
        );
        assert_eq!(
            adjusted(150.0, false, VolumeKey::Up, 6.0, max),
            (max, false)
        );
        assert_eq!(
            adjusted(120.0, true, VolumeKey::Up, 6.0, max),
            (120.0, false)
        );
        assert_eq!(
            adjusted(120.0, false, VolumeKey::Up, 6.0, 100.0),
            (100.0, false)
        );
        assert_eq!(
            adjusted(98.0, false, VolumeKey::Up, 6.0, 100.0),
            (100.0, false)
        );
        assert_eq!(output_limit(None), 100.0);
    }

    #[test]
    fn gnome_volume_unmute_zero_and_bounds() {
        assert_eq!(
            adjusted(40.0, true, VolumeKey::Up, 6.0, 100.0),
            (40.0, false)
        );
        assert_eq!(adjusted(0.0, true, VolumeKey::Up, 6.0, 100.0), (6.0, false));
        assert_eq!(
            adjusted(4.0, false, VolumeKey::Down, 6.0, 100.0),
            (0.0, true)
        );
        assert_eq!(
            adjusted(98.0, false, VolumeKey::Up, 6.0, 100.0),
            (100.0, false)
        );
        assert_eq!(
            adjusted(40.0, false, VolumeKey::Mute, 0.0, 100.0),
            (40.0, true)
        );
        assert_eq!(parse_volume("Volume: 0.40 [MUTED]"), Some((40.0, true)));
        assert_eq!(parse_volume("Volume: NaN"), None);
        assert_eq!(parse_volume("no default sink"), None);
    }
}
