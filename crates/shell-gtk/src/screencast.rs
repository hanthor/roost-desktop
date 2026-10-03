//! GNOME 51's screen recorder (#61): `org.gnome.Shell.Screencast`, what
//! the screenshot UI's record switch starts (screenshot.js) and the
//! recording indicator's stop button ends.
//!
//! As GNOME's screencast service (screencastService.js) does, a
//! recording asks Mutter's ScreenCast for the area (`RecordArea`, which
//! Roost's compositor serves), and GStreamer encodes the PipeWire stream
//! to VP8 in WebM under `~/Videos/Screencasts`. GNOME runs the pipeline
//! in-process; Roost runs the same software pipeline through
//! `gst-launch-1.0`, ending it with an EOS (SIGINT under `-e`) so the
//! file is finalized.
//!
//! Not yet: the pointer in recordings (the compositor's streams carry
//! no cursor), GNOME's hardware-encoder pipelines and their crash
//! blocklist, and the `framerate` option.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

pub const NAME: &str = "org.gnome.Shell.Screencast";
pub const PATH: &str = "/org/gnome/Shell/Screencast";
const IFACE: &str = "org.gnome.Shell.Screencast";
const MUTTER: &str = "org.gnome.Mutter.ScreenCast";

/// GNOME's file template (screenshot.js): `Screencasts/Screencast From
/// %d %t`, relative to the videos directory.
pub const TEMPLATE: &str = "Screencasts/Screencast From %d %t";

/// How long Mutter has to hand over the PipeWire node.
const START_DEADLINE: Duration = Duration::from_secs(10);
/// How long GStreamer has to finish the file after EOS.
const STOP_DEADLINE: Duration = Duration::from_secs(10);

const XML: &str = r#"<node>
  <interface name="org.gnome.Shell.Screencast">
    <property name="ScreencastSupported" type="b" access="read"/>
    <method name="Screencast">
      <arg type="s" direction="in" name="file_template"/>
      <arg type="a{sv}" direction="in" name="options"/>
      <arg type="b" direction="out" name="success"/>
      <arg type="s" direction="out" name="filename_used"/>
    </method>
    <method name="ScreencastArea">
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="i" direction="in" name="width"/>
      <arg type="i" direction="in" name="height"/>
      <arg type="s" direction="in" name="file_template"/>
      <arg type="a{sv}" direction="in" name="options"/>
      <arg type="b" direction="out" name="success"/>
      <arg type="s" direction="out" name="filename_used"/>
    </method>
    <method name="StopScreencast">
      <arg type="b" direction="out" name="success"/>
    </method>
    <signal name="Error">
      <arg type="s" name="name"/>
      <arg type="s" name="message"/>
    </signal>
  </interface>
</node>"#;

/// Expand GNOME's file template (screencastService.js
/// `_generateFilePath`): `%d` the date, `%t` the time, `%%` a percent
/// sign; a trailing `.webm` dropped (the extension is added); relative
/// names go under `videos`. Returns the path with `.webm`.
pub fn file_path(template: &str, now: &jiff::civil::DateTime, videos: &Path) -> PathBuf {
    let template = template.strip_suffix(".webm").unwrap_or(template);
    let mut name = String::new();
    let mut escape = false;
    for c in template.chars() {
        if escape {
            match c {
                '%' => name.push('%'),
                'd' => name.push_str(&now.strftime("%Y-%m-%d").to_string()),
                't' => name.push_str(&now.strftime("%H-%M-%S").to_string()),
                _ => {}
            }
            escape = false;
        } else if c == '%' {
            escape = true;
        } else {
            name.push(c);
        }
    }
    if escape {
        name.push('%');
    }
    let path = Path::new(&name);
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        videos.join(path)
    };
    let mut os = path.into_os_string();
    os.push(".webm");
    PathBuf::from(os)
}

/// Where recordings go: `XDG_VIDEOS_DIR`, the user's Videos directory,
/// else home (GNOME's order, the variable first as for screenshots).
pub fn videos_dir() -> PathBuf {
    std::env::var_os("XDG_VIDEOS_DIR")
        .map(PathBuf::from)
        .or_else(|| glib::user_special_dir(glib::UserDirectory::Videos))
        .unwrap_or_else(glib::home_dir)
}

/// GNOME's software VP8 pipeline (`swenc-memfd-vp8-vp8enc`) for
/// `threads` encoder threads, or the caller's own encoder, as
/// gst-launch arguments from the PipeWire node to the file.
pub fn pipeline(node: u32, encoder: Option<&str>, threads: u32, path: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "pipewiresrc".into(),
        format!("path={node}"),
        "do-timestamp=true".into(),
        "keepalive-time=1000".into(),
        "resend-last=true".into(),
        "!".into(),
    ];
    match encoder {
        Some(custom) => args.extend(custom.split_whitespace().map(str::to_owned)),
        None => {
            let t = threads.clamp(1, 64);
            args.extend(
                [
                    "videoconvert",
                    "chroma-mode=none",
                    "dither=none",
                    "matrix-mode=output-only",
                    &format!("n-threads={t}"),
                    "!",
                    "queue",
                    "!",
                    "vp8enc",
                    "cpu-used=16",
                    "max-quantizer=17",
                    "deadline=1",
                    "keyframe-mode=disabled",
                    &format!("threads={t}"),
                    "static-threshold=1000",
                    "buffer-size=20000",
                    "!",
                    "queue",
                    "!",
                    "webmmux",
                ]
                .iter()
                .map(|s| (*s).to_owned()),
            );
        }
    }
    args.push("!".into());
    args.push("filesink".into());
    // gst-launch escapes each argument, so spaces in the name are fine.
    args.push(format!("location={}", path.display()));
    args
}

/// What a recording is doing.
enum Phase {
    Idle,
    Starting,
    Recording {
        process: gio::Subprocess,
        conn: gio::DBusConnection,
        session: String,
        path: PathBuf,
        _closed: gio::SignalSubscription,
    },
    Stopping,
}

/// Says something happened: (summary, body).
pub type Notify = Rc<dyn Fn(&str, &str)>;

/// Told whether a recording is in progress.
pub type Watcher = Rc<dyn Fn(bool)>;

/// One recording at a time, for the screenshot UI and D-Bus callers.
pub struct Recorder {
    phase: RefCell<Phase>,
    /// Told when a recording starts (true) and ends (false).
    watchers: RefCell<Vec<Watcher>>,
    notify: Notify,
    /// The D-Bus connection holding the name, for the Error signal.
    bus: RefCell<Option<gio::DBusConnection>>,
    /// Bumped per recording, so a stale exit is not taken for a crash.
    generation: Cell<u64>,
}

/// Whether recordings can be made: GStreamer's launcher is installed.
pub fn supported() -> bool {
    glib::find_program_in_path("gst-launch-1.0").is_some()
}

impl Recorder {
    pub fn new(notify: Notify) -> Rc<Self> {
        Rc::new(Self {
            phase: RefCell::new(Phase::Idle),
            watchers: RefCell::new(Vec::new()),
            notify,
            bus: RefCell::new(None),
            generation: Cell::new(0),
        })
    }

    /// Call `watcher` whenever a recording starts or ends.
    pub fn watch(&self, watcher: Watcher) {
        self.watchers.borrow_mut().push(watcher);
    }

    pub fn in_progress(&self) -> bool {
        !matches!(*self.phase.borrow(), Phase::Idle)
    }

    fn changed(&self, recording: bool) {
        let watchers = self.watchers.borrow().clone();
        for watcher in watchers {
            watcher(recording);
        }
    }

    /// Record `area` (global logical pixels; `None`: the primary
    /// monitor) into a file named after `template`. `done` gets the path
    /// once GStreamer runs, or why it could not start.
    pub fn start(
        self: &Rc<Self>,
        area: Option<(i32, i32, i32, i32)>,
        template: &str,
        encoder: Option<String>,
        done: impl FnOnce(Result<PathBuf, String>) + 'static,
    ) {
        if self.in_progress() {
            done(Err("a screencast is already running".into()));
            return;
        }
        if !supported() {
            done(Err("gst-launch-1.0 is not installed".into()));
            return;
        }
        let area = match area.or_else(primary_monitor) {
            Some(a) if a.2 > 0 && a.3 > 0 => a,
            _ => {
                done(Err("no area to record".into()));
                return;
            }
        };
        let path = file_path(template, &jiff::Zoned::now().datetime(), &videos_dir());
        *self.phase.borrow_mut() = Phase::Starting;
        // As GNOME, the indicator shows from the start.
        self.changed(true);
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            match this
                .begin(area, &path, encoder.as_deref(), generation)
                .await
            {
                Ok(()) => done(Ok(path)),
                Err(e) => {
                    eprintln!("roost-shell-gtk: screencast failed to start: {e}");
                    *this.phase.borrow_mut() = Phase::Idle;
                    this.changed(false);
                    (this.notify)("Screencast Failed to Start", "");
                    done(Err(e));
                }
            }
        });
    }

    async fn begin(
        self: &Rc<Self>,
        (x, y, w, h): (i32, i32, i32, i32),
        path: &Path,
        encoder: Option<&str>,
        generation: u64,
    ) -> Result<(), String> {
        let err = |e: glib::Error| e.to_string();
        let conn = gio::bus_get_future(gio::BusType::Session)
            .await
            .map_err(err)?;
        let call = |object: String,
                    iface: &'static str,
                    method: &'static str,
                    args: glib::Variant,
                    reply: &'static str| {
            conn.call_future(
                Some(MUTTER),
                &object,
                iface,
                method,
                Some(&args),
                glib::VariantTy::new(reply).ok(),
                gio::DBusCallFlags::NONE,
                10_000,
            )
        };
        let (session,) = call(
            "/org/gnome/Mutter/ScreenCast".into(),
            "org.gnome.Mutter.ScreenCast",
            "CreateSession",
            glib::Variant::tuple_from_iter([glib::VariantDict::new(None).end()]),
            "(o)",
        )
        .await
        .map_err(err)?
        .get::<(glib::variant::ObjectPath,)>()
        .ok_or("CreateSession answered oddly")?;
        let session = session.as_str().to_owned();
        let props = glib::VariantDict::new(None);
        props.insert("is-recording", true);
        props.insert("cursor-mode", 1u32);
        let stream = match call(
            session.clone(),
            "org.gnome.Mutter.ScreenCast.Session",
            "RecordArea",
            glib::Variant::tuple_from_iter([
                x.to_variant(),
                y.to_variant(),
                w.to_variant(),
                h.to_variant(),
                props.end(),
            ]),
            "(o)",
        )
        .await
        .map_err(err)
        .and_then(|v| {
            v.get::<(glib::variant::ObjectPath,)>()
                .ok_or_else(|| "RecordArea answered oddly".to_owned())
        }) {
            Ok((stream,)) => stream.as_str().to_owned(),
            Err(e) => {
                stop_session(&conn, &session);
                return Err(e);
            }
        };
        let node: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
        let added = {
            let node = node.clone();
            conn.subscribe_to_signal(
                Some(MUTTER),
                Some("org.gnome.Mutter.ScreenCast.Stream"),
                Some("PipeWireStreamAdded"),
                Some(&stream),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    if let Some((id,)) = signal.parameters.get::<(u32,)>() {
                        node.set(Some(id));
                    }
                },
            )
        };
        if let Err(e) = call(
            session.clone(),
            "org.gnome.Mutter.ScreenCast.Session",
            "Start",
            ().to_variant(),
            "()",
        )
        .await
        {
            stop_session(&conn, &session);
            return Err(e.to_string());
        }
        let mut waited = Duration::ZERO;
        let step = Duration::from_millis(50);
        let node = loop {
            if let Some(node) = node.get() {
                break node;
            }
            if waited >= START_DEADLINE {
                stop_session(&conn, &session);
                return Err("Mutter never announced the PipeWire stream".into());
            }
            glib::timeout_future(step).await;
            waited += step;
        };
        drop(added);
        if let Some(dir) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                stop_session(&conn, &session);
                return Err(format!("{}: {e}", dir.display()));
            }
        }
        let threads = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1);
        let mut argv: Vec<std::ffi::OsString> = vec!["gst-launch-1.0".into(), "-e".into()];
        argv.extend(
            pipeline(node, encoder, threads, path)
                .into_iter()
                .map(Into::into),
        );
        let argv: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_os_str()).collect();
        let process = match gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE) {
            Ok(p) => p,
            Err(e) => {
                stop_session(&conn, &session);
                return Err(e.to_string());
            }
        };
        // The compositor ending the session (its stream failed) ends
        // the recording, file finalized.
        let closed = {
            let weak = Rc::downgrade(self);
            conn.subscribe_to_signal(
                Some(MUTTER),
                Some("org.gnome.Mutter.ScreenCast.Session"),
                Some("Closed"),
                Some(&session),
                None,
                gio::DBusSignalFlags::NONE,
                move |_| {
                    // Not from inside this subscription's own callback:
                    // stopping drops it.
                    let weak = weak.clone();
                    glib::idle_add_local_once(move || {
                        if let Some(this) = weak.upgrade() {
                            this.stop(|_| {});
                        }
                    });
                },
            )
        };
        // GStreamer exiting on its own is a failed recording.
        {
            let weak = Rc::downgrade(self);
            let waiting = process.clone();
            glib::MainContext::default().spawn_local(async move {
                let _ = waiting.wait_future().await;
                let Some(this) = weak.upgrade() else { return };
                if this.generation.get() != generation {
                    return;
                }
                if !matches!(*this.phase.borrow(), Phase::Recording { .. }) {
                    return;
                }
                let phase = std::mem::replace(&mut *this.phase.borrow_mut(), Phase::Idle);
                if let Phase::Recording { conn, session, .. } = phase {
                    stop_session(&conn, &session);
                }
                eprintln!("roost-shell-gtk: the screencast pipeline stopped on its own");
                this.emit_error(
                    "org.gnome.Shell.Screencast.Error.PipelineFailed",
                    "recording failed",
                );
                this.changed(false);
                (this.notify)("Screencast Failed", "");
            });
        }
        *self.phase.borrow_mut() = Phase::Recording {
            process,
            conn,
            session,
            path: path.to_owned(),
            _closed: closed,
        };
        eprintln!(
            "roost-shell-gtk: recording {x},{y} {w}x{h} to {}",
            path.display()
        );
        Ok(())
    }

    /// End the recording: GStreamer finishes the file, then the session
    /// stops. `done` says whether a recording was saved.
    pub fn stop(self: &Rc<Self>, done: impl FnOnce(bool) + 'static) {
        let phase = std::mem::replace(&mut *self.phase.borrow_mut(), Phase::Stopping);
        let (process, conn, session, path) = match phase {
            Phase::Recording {
                process,
                conn,
                session,
                path,
                ..
            } => (process, conn, session, path),
            other => {
                // Starting or idle: nothing to finish yet.
                *self.phase.borrow_mut() = other;
                done(false);
                return;
            }
        };
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            // gst-launch -e turns SIGINT into EOS and exits once the
            // muxer has written the file out.
            process.send_signal(libc::SIGINT);
            let finished = glib::future_with_timeout(STOP_DEADLINE, process.wait_future())
                .await
                .is_ok();
            if !finished {
                process.force_exit();
            }
            stop_session(&conn, &session);
            *this.phase.borrow_mut() = Phase::Idle;
            this.changed(false);
            let saved = finished && path.exists();
            if saved {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                eprintln!("roost-shell-gtk: screencast saved to {}", path.display());
                (this.notify)("Screencast Recorded", &name);
            } else {
                (this.notify)("Screencast Failed", "");
            }
            done(saved);
        });
    }

    fn emit_error(&self, name: &str, message: &str) {
        if let Some(conn) = self.bus.borrow().as_ref() {
            let _ = conn.emit_signal(
                None,
                PATH,
                IFACE,
                "Error",
                Some(&(name, message).to_variant()),
            );
        }
    }
}

/// Stop a ScreenCast session, not waiting for the answer.
fn stop_session(conn: &gio::DBusConnection, session: &str) {
    conn.call(
        Some(MUTTER),
        session,
        "org.gnome.Mutter.ScreenCast.Session",
        "Stop",
        None,
        None,
        gio::DBusCallFlags::NONE,
        5_000,
        gio::Cancellable::NONE,
        |_| {},
    );
}

/// The primary monitor's logical geometry.
fn primary_monitor() -> Option<(i32, i32, i32, i32)> {
    let display = gtk4::gdk::Display::default()?;
    let monitor = display
        .monitors()
        .item(0)
        .and_then(|m| m.downcast::<gtk4::gdk::Monitor>().ok())?;
    let g = monitor.geometry();
    Some((g.x(), g.y(), g.width(), g.height()))
}

/// The options GNOME's interface takes: `draw-cursor` (ignored: the
/// compositor's streams have no cursor yet), `framerate` (ignored) and
/// `pipeline`, the caller's own encoder.
fn encoder_option(options: &glib::Variant) -> Option<String> {
    glib::VariantDict::new(Some(options))
        .lookup::<String>("pipeline")
        .ok()
        .flatten()
        .filter(|p| !p.trim().is_empty())
}

/// Own `org.gnome.Shell.Screencast` and serve it from `recorder`. Like
/// GNOME's service, it answers any caller on the session bus.
pub fn serve(recorder: Rc<Recorder>) {
    let node = match gio::DBusNodeInfo::for_xml(XML) {
        Ok(node) => node,
        Err(e) => {
            eprintln!("roost-shell-gtk: org.gnome.Shell.Screencast interface: {e}");
            return;
        }
    };
    let Some(info) = node.lookup_interface(IFACE) else {
        return;
    };
    let id = gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        gio::BusNameOwnerFlags::NONE,
        move |conn, _| {
            *recorder.bus.borrow_mut() = Some(conn.clone());
            let calls = recorder.clone();
            let registered = conn
                .register_object(PATH, &info)
                .method_call(move |_, _, _, _, method, params, invocation| {
                    let start =
                        |area: Option<(i32, i32, i32, i32)>,
                         template: String,
                         options: glib::Variant,
                         invocation: gio::DBusMethodInvocation| {
                            let encoder = encoder_option(&options);
                            calls.start(area, &template, encoder, move |result| match result {
                                Ok(path) => invocation.return_value(Some(
                                    &(true, path.to_string_lossy().into_owned()).to_variant(),
                                )),
                                Err(e) => invocation
                                    .return_dbus_error("org.freedesktop.DBus.Error.Failed", &e),
                            });
                        };
                    // The options dictionary is the last argument.
                    let options = |at: usize| params.try_child_value(at);
                    let text =
                        |at: usize| params.try_child_value(at).and_then(|v| v.get::<String>());
                    let int = |at: usize| params.try_child_value(at).and_then(|v| v.get::<i32>());
                    match method {
                        "Screencast" => match (text(0), options(1)) {
                            (Some(template), Some(options)) => {
                                start(None, template, options, invocation)
                            }
                            _ => invocation.return_dbus_error(
                                "org.freedesktop.DBus.Error.InvalidArgs",
                                method,
                            ),
                        },
                        "ScreencastArea" => {
                            match (int(0), int(1), int(2), int(3), text(4), options(5)) {
                                (
                                    Some(x),
                                    Some(y),
                                    Some(w),
                                    Some(h),
                                    Some(template),
                                    Some(options),
                                ) => start(Some((x, y, w, h)), template, options, invocation),
                                _ => invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.InvalidArgs",
                                    method,
                                ),
                            }
                        }
                        "StopScreencast" => calls.stop(move |saved| {
                            invocation.return_value(Some(&(saved,).to_variant()));
                        }),
                        _ => invocation
                            .return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
                    }
                })
                .property(|_, _, _, _, property| match property {
                    "ScreencastSupported" => supported().to_variant(),
                    _ => ().to_variant(),
                })
                .build();
            if let Err(e) = registered {
                eprintln!("roost-shell-gtk: org.gnome.Shell.Screencast object: {e}");
            }
        },
        |_, _| {},
        |_, _| eprintln!("roost-shell-gtk: org.gnome.Shell.Screencast is owned elsewhere"),
    );
    // Held for the session.
    let _ = id;
}

/// GNOME's recording indicator (status/remoteAccess.js
/// `ScreenRecordingIndicator`): a red panel button with the elapsed time
/// and a stop icon, shown while recording; pressing it stops.
pub fn indicator(recorder: &Rc<Recorder>) -> gtk4::Button {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    let label = gtk4::Label::new(Some("0:00"));
    row.append(&label);
    row.append(&gtk4::Image::from_icon_name("screencast-stop-symbolic"));
    let button = gtk4::Button::builder().child(&row).build();
    button.add_css_class("panel-button");
    button.add_css_class("screen-recording-indicator");
    button.update_property(&[gtk4::accessible::Property::Label("Stop Screencast")]);
    button.set_visible(false);
    {
        let recorder = recorder.clone();
        button.connect_clicked(move |_| recorder.stop(|_| {}));
    }
    let ticker: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let weak_button = button.downgrade();
    recorder.watch(Rc::new(move |recording| {
        let Some(button) = weak_button.upgrade() else {
            return;
        };
        if let Some(id) = ticker.borrow_mut().take() {
            id.remove();
        }
        button.set_visible(recording);
        if recording {
            label.set_text(&elapsed(0));
            let label = label.clone();
            let seconds = Cell::new(0u64);
            *ticker.borrow_mut() = Some(glib::timeout_add_seconds_local(1, move || {
                seconds.set(seconds.get() + 1);
                label.set_text(&elapsed(seconds.get()));
                glib::ControlFlow::Continue
            }));
        }
    }));
    button
}

/// GNOME's elapsed-time label: `M:SS`.
pub fn elapsed(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_named_like_gnome() {
        let t = jiff::civil::date(2026, 10, 3).at(14, 5, 9, 0);
        let videos = Path::new("/home/u/Videos");
        assert_eq!(
            file_path(TEMPLATE, &t, videos),
            PathBuf::from("/home/u/Videos/Screencasts/Screencast From 2026-10-03 14-05-09.webm")
        );
        // The deprecated extension in the template, absolute names, %%.
        assert_eq!(
            file_path("/tmp/a %% b.webm", &t, videos),
            PathBuf::from("/tmp/a % b.webm")
        );
        assert_eq!(
            file_path("x%", &t, videos),
            PathBuf::from("/home/u/Videos/x%.webm")
        );
    }

    #[test]
    fn the_pipeline_is_gnomes_software_vp8() {
        let args = pipeline(42, None, 2, Path::new("/v/Screencast From x.webm"));
        let line = args.join(" ");
        assert!(
            line.starts_with("pipewiresrc path=42 do-timestamp=true"),
            "{line}"
        );
        assert!(
            line.contains("! vp8enc cpu-used=16 max-quantizer=17 deadline=1"),
            "{line}"
        );
        assert!(line.contains("threads=2"), "{line}");
        assert!(
            line.ends_with("! webmmux ! filesink location=/v/Screencast From x.webm"),
            "{line}"
        );
        // The file name stays one argument.
        assert_eq!(args.last().unwrap(), "location=/v/Screencast From x.webm");
        let custom = pipeline(1, Some("x264enc ! mp4mux"), 2, Path::new("/v/a.mp4")).join(" ");
        assert!(custom.contains("! x264enc ! mp4mux ! filesink"), "{custom}");
    }

    #[test]
    fn the_indicator_counts_like_gnome() {
        assert_eq!(elapsed(0), "0:00");
        assert_eq!(elapsed(65), "1:05");
        assert_eq!(elapsed(600), "10:00");
    }
}
