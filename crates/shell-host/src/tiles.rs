//! Panel status tiles: clock plus service presence (002 panel).
//!
//! Every tile is a [`TileState`] the strip renders honestly: `Ready`
//! only when the underlying source is actually present, `Disconnected`
//! when it is absent, `Error` when probing itself fails, `Loading`
//! before the first probe (and while a network or sound toggle awaits
//! confirmation). Probes read local sysfs files or check socket
//! paths — never connect, never block, never spawn — so absent
//! services cannot hang the shell. The network tile additionally owns
//! a [`NetworkRadio`] D-Bus caller (NetworkManager contract), and the
//! sound tile a [`SoundMixer`] D-Bus caller (mute plus output
//! volume), behind the same state shape; the sysfs/socket probes
//! below stay the presence truth they build on.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::prefs::{self, TunaPrefs};
use crate::settings::{self, ClockFormat, SettingsBackend, ShellSettings};

/// NetworkManager bus identity for the radio toggle contract.
pub const NM_NAME: &str = "org.freedesktop.NetworkManager";
/// NetworkManager object path.
pub const NM_PATH: &str = "/org/freedesktop/NetworkManager";
/// NetworkManager interface carrying the radio flags.
pub const NM_IFACE: &str = "org.freedesktop.NetworkManager";
/// Strip index of the network tile (strip order: network, power, sound).
pub const NETWORK_TILE_INDEX: usize = 0;
/// Strip index of the power tile (strip order: network, power, sound).
pub const POWER_TILE_INDEX: usize = 1;
/// Strip index of the sound tile (strip order: network, power, sound).
pub const SOUND_TILE_INDEX: usize = 2;
/// Bounded wait per radio round trip so a hung service cannot stall
/// the slow tick or a menu press.
const RADIO_TIMEOUT: Duration = Duration::from_secs(1);
/// Sound-server control bus identity. Neither PulseAudio nor PipeWire
/// exposes a standard mute/volume D-Bus control in this environment
/// (no `pactl`/`pw-cli`, no session names), so the mixer ships
/// against this stub contract — `Muted` (bool) and `Volume` (double,
/// `0.0..=1.0`) properties — resolved at implement time against a
/// stub on a private bus.
pub const SND_NAME: &str = "org.tuna.Sound";
/// Sound-server control object path.
pub const SND_PATH: &str = "/org/tuna/Sound";
/// Sound-server control interface carrying mute and volume.
pub const SND_IFACE: &str = "org.tuna.Sound";
/// Bounded wait per mixer round trip so a hung service cannot stall
/// the slow tick or a menu press.
const SOUND_TIMEOUT: Duration = Duration::from_secs(1);
/// Volume step per menu-row nudge (`volume_up`/`volume_down`).
const VOLUME_STEP: f64 = 0.05;

/// Last-known NetworkManager radio flags behind the network tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RadioState {
    /// `WirelessEnabled`: wifi radio.
    pub wireless: bool,
    /// `NetworkingEnabled` (via `Enable`): every radio.
    pub networking: bool,
}

/// D-Bus caller behind the network tile: toggles the NetworkManager
/// radio flags and reads them back. Owned by the probe owner
/// ([`TileSet`]), mirroring [`crate::watcher::WatcherBus`]: a
/// disconnected edge until [`NetworkRadio::ensure`] connects, and
/// blocking-proxy calls throughout, so a taken name or no bus reads
/// as `None`/`false` — never a hang.
#[derive(Debug)]
pub struct NetworkRadio {
    conn: Option<zbus::blocking::Connection>,
}

impl NetworkRadio {
    /// Disconnected caller; [`NetworkRadio::ensure`] connects.
    pub fn new() -> Self {
        Self { conn: None }
    }

    /// Connect the session bus with short method timeouts. Returns
    /// true while the bus is reachable. A missing bus reads as no
    /// radio: tiles stay pure sysfs and nothing else changes.
    pub fn ensure(&mut self) -> bool {
        self.connect(None)
    }

    /// Connect over an explicit bus address (`None` means the session
    /// bus). Tests point this at a private daemon; production passes
    /// `None` through [`NetworkRadio::ensure`].
    fn connect(&mut self, address: Option<&str>) -> bool {
        if self.conn.is_some() {
            return true;
        }
        let builder = match address {
            Some(address) => zbus::blocking::connection::Builder::address(address),
            None => zbus::blocking::connection::Builder::session(),
        };
        let builder = match builder {
            Ok(builder) => builder.method_timeout(RADIO_TIMEOUT),
            Err(e) => {
                eprintln!("tuna-shell-host: network radio bus unavailable: {e}");
                return false;
            }
        };
        match builder.build() {
            Ok(conn) => {
                self.conn = Some(conn);
                true
            }
            Err(e) => {
                eprintln!("tuna-shell-host: network radio connect failed: {e}");
                false
            }
        }
    }

    /// Blocking proxy for the NetworkManager object (the watcher.rs
    /// call shape: [`zbus::blocking::Proxy::new`], then property or
    /// method calls). `None` without a bus.
    fn proxy(&self) -> Option<zbus::blocking::Proxy<'_>> {
        let conn = self.conn.as_ref()?;
        zbus::blocking::Proxy::new(conn, NM_NAME, NM_PATH, NM_IFACE).ok()
    }

    /// Current radio flags. `None` when the bus is absent or the
    /// service does not answer within the short timeout.
    pub fn snapshot(&self) -> Option<RadioState> {
        let proxy = self.proxy()?;
        let wireless: bool = proxy.get_property("WirelessEnabled").ok()?;
        let networking: bool = proxy.get_property("NetworkingEnabled").ok()?;
        Some(RadioState {
            wireless,
            networking,
        })
    }

    /// Set the wifi radio (`WirelessEnabled`). True once the service
    /// accepts the write within the short timeout.
    pub fn set_wireless_enabled(&self, enabled: bool) -> bool {
        let Some(proxy) = self.proxy() else {
            return false;
        };
        proxy.set_property("WirelessEnabled", enabled).is_ok()
    }

    /// Set every radio via `Enable`. True once the service accepts
    /// the call within the short timeout.
    pub fn set_networking_enabled(&self, enabled: bool) -> bool {
        let Some(proxy) = self.proxy() else {
            return false;
        };
        proxy.call::<&str, _, ()>("Enable", &(enabled,)).is_ok()
    }
}

impl Default for NetworkRadio {
    /// Disconnected caller, like [`NetworkRadio::new`].
    fn default() -> Self {
        Self::new()
    }
}

/// Last-known sound-server output state behind the sound tile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoundState {
    /// Output mute flag: muting silences output, unmuting restores it.
    pub muted: bool,
    /// Output volume, `0.0..=1.0`.
    pub volume: f64,
}

impl Default for SoundState {
    /// Unmuted at full volume (the fire path's fallback before the
    /// first confirmed read).
    fn default() -> Self {
        Self {
            muted: false,
            volume: 1.0,
        }
    }
}

/// D-Bus caller behind the sound tile: toggles output mute and sets
/// output volume, then reads them back. Owned by the probe owner
/// ([`TileSet`]), mirroring [`NetworkRadio`]: a disconnected edge
/// until [`SoundMixer::ensure`] connects, and blocking-proxy calls
/// throughout, so a taken name or no bus reads as `None`/`false` —
/// never a hang.
#[derive(Debug)]
pub struct SoundMixer {
    conn: Option<zbus::blocking::Connection>,
}

impl SoundMixer {
    /// Disconnected caller; [`SoundMixer::ensure`] connects.
    pub fn new() -> Self {
        Self { conn: None }
    }

    /// Connect the session bus with short method timeouts. Returns
    /// true while the bus is reachable. A missing bus reads as no
    /// mixer: tiles stay pure sysfs and nothing else changes.
    pub fn ensure(&mut self) -> bool {
        self.connect(None)
    }

    /// Connect over an explicit bus address (`None` means the session
    /// bus). Tests point this at a private daemon; production passes
    /// `None` through [`SoundMixer::ensure`].
    fn connect(&mut self, address: Option<&str>) -> bool {
        if self.conn.is_some() {
            return true;
        }
        let builder = match address {
            Some(address) => zbus::blocking::connection::Builder::address(address),
            None => zbus::blocking::connection::Builder::session(),
        };
        let builder = match builder {
            Ok(builder) => builder.method_timeout(SOUND_TIMEOUT),
            Err(e) => {
                eprintln!("tuna-shell-host: sound mixer bus unavailable: {e}");
                return false;
            }
        };
        match builder.build() {
            Ok(conn) => {
                self.conn = Some(conn);
                true
            }
            Err(e) => {
                eprintln!("tuna-shell-host: sound mixer connect failed: {e}");
                false
            }
        }
    }

    /// Blocking proxy for the sound-server control object (the
    /// watcher.rs call shape: [`zbus::blocking::Proxy::new`], then
    /// property calls). `None` without a bus.
    fn proxy(&self) -> Option<zbus::blocking::Proxy<'_>> {
        let conn = self.conn.as_ref()?;
        zbus::blocking::Proxy::new(conn, SND_NAME, SND_PATH, SND_IFACE).ok()
    }

    /// Current output state. `None` when the bus is absent or the
    /// service does not answer within the short timeout.
    pub fn snapshot(&self) -> Option<SoundState> {
        let proxy = self.proxy()?;
        let muted: bool = proxy.get_property("Muted").ok()?;
        let volume: f64 = proxy.get_property("Volume").ok()?;
        Some(SoundState { muted, volume })
    }

    /// Set output mute. True once the service accepts the write
    /// within the short timeout.
    pub fn set_muted(&self, muted: bool) -> bool {
        let Some(proxy) = self.proxy() else {
            return false;
        };
        proxy.set_property("Muted", muted).is_ok()
    }

    /// Set output volume (`0.0..=1.0`). True once the service accepts
    /// the write within the short timeout.
    pub fn set_volume(&self, volume: f64) -> bool {
        let Some(proxy) = self.proxy() else {
            return false;
        };
        proxy.set_property("Volume", volume.clamp(0.0, 1.0)).is_ok()
    }
}

impl Default for SoundMixer {
    /// Disconnected caller, like [`SoundMixer::new`].
    fn default() -> Self {
        Self::new()
    }
}

/// Which service a tile reports on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    /// Local wall clock: always available, always `Ready`.
    Clock,
    /// Wired/wireless link from `/sys/class/net` operstate.
    Network,
    /// Batteries and mains from `/sys/class/power_supply`.
    Power,
    /// Sound server socket presence (PipeWire, then PulseAudio).
    Sound,
}

/// Availability of one tile's service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TileState {
    /// Before the first probe ran.
    #[default]
    Loading,
    /// Source present (and, for links, actually up).
    Ready,
    /// Source absent or down. Normal on hardware without it.
    Disconnected,
    /// Probing itself failed (permissions, malformed sysfs).
    Error,
}

/// One strip tile: its service, availability, and optional level
/// (battery percent for [`ServiceKind::Power`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    /// What this tile reports on.
    pub kind: ServiceKind,
    /// Current availability.
    pub state: TileState,
    /// Battery percent, when known.
    pub level: Option<u8>,
}

impl Tile {
    /// Fresh tile awaiting its first probe.
    pub fn new(kind: ServiceKind) -> Self {
        Self {
            kind,
            state: TileState::Loading,
            level: None,
        }
    }
}

/// The strip's tile set: clock plus one tile per service. Probe roots
/// are injectable so tests run against fake sysfs trees; production
/// passes the real roots.
#[derive(Debug)]
pub struct TileSet {
    /// Local timezone name for the clock (e.g. from `$TZ`); empty
    /// means the system default.
    tz: String,
    /// Real or fake `/sys/class/net`.
    net_root: PathBuf,
    /// Real or fake `/sys/class/power_supply`.
    power_root: PathBuf,
    /// Real or fake runtime dir holding sound sockets.
    runtime_dir: PathBuf,
    /// Current clock text (`HH:MM`, or `h:MM` in twelve-hour mode).
    pub clock: String,
    /// Snapshot of the shared desktop settings feeding the paint code.
    pub settings: ShellSettings,
    /// Tuna-owned prefs snapshot feeding the paint code (bar clock
    /// extras the shared schema never covered). Loaded from the XDG
    /// state file at startup; the calendar surface writes it back.
    pub prefs: TunaPrefs,
    /// Prefs file behind this set; `None` keeps it memory-only. Set
    /// by [`TileSet::restore_prefs`], so a restored set keeps
    /// persisting to the file it came from.
    prefs_path: Option<PathBuf>,
    /// D-Bus caller behind the network tile (NetworkManager radio
    /// flags). The sysfs probe stays the link truth; this only
    /// toggles radios and confirms the tile against them.
    radio: NetworkRadio,
    /// Last radio flags read by [`TileSet::refresh`] (`None` before
    /// the first confirmed read, or while the bus is absent). Menus
    /// paint from this cache, never from the wire.
    radio_state: Option<RadioState>,
    /// Radio flags a fired toggle is waiting for. While set, the
    /// network tile shows the pending (`Loading`) face; the next
    /// [`TileSet::refresh`] whose re-probe matches clears it.
    network_pending: Option<RadioState>,
    /// D-Bus caller behind the sound tile (mute plus output volume).
    /// The socket probe stays the presence truth; this only toggles
    /// output and confirms the menu against it.
    mixer: SoundMixer,
    /// Last output state read by [`TileSet::refresh`] (`None` before
    /// the first confirmed read, or while the bus is absent). Menus
    /// paint from this cache, never from the wire.
    sound_state: Option<SoundState>,
    /// Output state a fired toggle is waiting for. While set, the
    /// sound tile shows the pending (`Loading`) face; the next
    /// [`TileSet::refresh`] whose re-probe matches clears it.
    sound_pending: Option<SoundState>,
    /// Network tile.
    pub network: Tile,
    /// Power tile.
    pub power: Tile,
    /// Sound tile.
    pub sound: Tile,
}

impl TileSet {
    /// Tile set probing the live system roots.
    pub fn system() -> Self {
        // Fail closed (#49): an unset or shared runtime dir yields an
        // empty root, never `/run/user/0`; sound probes then find no
        // socket and wallpaper publishing is skipped.
        let runtime_dir = crate::xdg::runtime_dir().unwrap_or_default();
        Self::with_roots(
            "",
            Path::new("/sys/class/net"),
            Path::new("/sys/class/power_supply"),
            &runtime_dir,
        )
    }

    /// Tile set against explicit roots (tests, containers).
    pub fn with_roots(tz: &str, net_root: &Path, power_root: &Path, runtime_dir: &Path) -> Self {
        Self {
            tz: tz.to_owned(),
            net_root: net_root.to_owned(),
            power_root: power_root.to_owned(),
            runtime_dir: runtime_dir.to_owned(),
            clock: String::new(),
            settings: ShellSettings::default(),
            prefs: TunaPrefs::default(),
            prefs_path: None,
            radio: NetworkRadio::new(),
            radio_state: None,
            network_pending: None,
            mixer: SoundMixer::new(),
            sound_state: None,
            sound_pending: None,
            network: Tile::new(ServiceKind::Network),
            power: Tile::new(ServiceKind::Power),
            sound: Tile::new(ServiceKind::Sound),
        }
    }

    /// Refresh the clock text and re-probe every service. Local reads
    /// stay bounded, nonblocking, spawn-free; the radio and mixer
    /// re-probes are short-timeout D-Bus reads. Settings come from the
    /// platform backend on this same tick, degrading to defaults when
    /// the bus or schema is absent.
    pub fn refresh(&mut self) {
        // The Wayland host has no GLib main loop. Dispatch pending settings
        // backend notifications so keyfile/dconf caches see external writes.
        // Bound this pass so a noisy GLib source cannot starve the shell.
        let context = gio::glib::MainContext::default();
        for _ in 0..16 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        self.refresh_with(&settings::GioBackend);
    }

    /// [`refresh`](Self::refresh) against an explicit backend: the
    /// slow tick passes the platform backend, tests a fake. Paint
    /// code still reads only the snapshot, never a backend.
    pub fn refresh_with(&mut self, backend: &dyn SettingsBackend) {
        settings::refresh(&mut self.settings, backend);
        self.clock = clock_text(
            &self.tz,
            self.settings.clock_format,
            self.prefs.clock_show_weekday,
        );
        self.network = probe_network(&self.net_root);
        self.power = probe_power(&self.power_root);
        self.sound = probe_sound(&self.runtime_dir);
        self.apply_radio();
        self.apply_sound();
    }

    /// Write the bar clock format against the shared desktop schema,
    /// then re-read at once so paint stays on snapshot truth. A
    /// refused write keeps the last good snapshot, quietly.
    pub fn set_clock_format(&mut self, format: ClockFormat) -> bool {
        self.set_clock_format_with(format, &settings::GioBackend)
    }

    /// [`set_clock_format`](Self::set_clock_format) against an
    /// explicit backend (tests substitute a fake).
    pub fn set_clock_format_with(
        &mut self,
        format: ClockFormat,
        backend: &dyn SettingsBackend,
    ) -> bool {
        if !settings::write_clock_format(backend, format) {
            return false;
        }
        settings::refresh(&mut self.settings, backend);
        self.clock = clock_text(
            &self.tz,
            self.settings.clock_format,
            self.prefs.clock_show_weekday,
        );
        true
    }

    /// Load Tuna-owned prefs from `path` into the snapshot, replacing
    /// whatever it holds. Missing, corrupt, or version-skewed files
    /// read as defaults. The loaded path sticks to the set, so later
    /// surface writes persist back to it. Recomputes the clock text at
    /// once so paint stays on snapshot truth.
    pub fn restore_prefs(&mut self, path: &Path) {
        self.prefs = prefs::load(path);
        self.prefs_path = Some(path.to_owned());
        self.clock = clock_text(
            &self.tz,
            self.settings.clock_format,
            self.prefs.clock_show_weekday,
        );
    }

    /// Load Tuna-owned prefs from the system state file at host
    /// start. Same fail-closed rule as
    /// [`TileSet::restore_prefs`]: a bad file never blocks the panel.
    pub fn load_prefs_system(&mut self) {
        if let Some(path) = prefs::system_path() {
            self.restore_prefs(&path);
        }
    }

    /// Persist back to the prefs file when one is pinned. Failures
    /// log and keep the in-memory prefs: a full disk must never lose
    /// or break the live clock.
    fn persist_prefs(&self) {
        if let Some(path) = self.prefs_path.as_ref() {
            if let Err(e) = prefs::save(&self.prefs, path) {
                eprintln!("tuna-shell-host: tuna prefs save failed: {e}");
            }
        }
    }

    /// Set the bar-clock weekday prefix from the calendar surface:
    /// update the snapshot at once, recompute the clock text, and
    /// persist back to the pinned prefs file when one is set.
    pub fn set_clock_show_weekday(&mut self, show: bool) -> bool {
        self.prefs.clock_show_weekday = show;
        self.clock = clock_text(
            &self.tz,
            self.settings.clock_format,
            self.prefs.clock_show_weekday,
        );
        self.persist_prefs();
        true
    }

    /// Flip the bar-clock weekday prefix: the calendar prefs row's
    /// write. Same snapshot-then-persist shape as
    /// [`TileSet::set_clock_show_weekday`].
    pub fn toggle_clock_show_weekday(&mut self) -> bool {
        self.set_clock_show_weekday(!self.prefs.clock_show_weekday)
    }

    /// Flip the bar clock between twelve- and twenty-four-hour: the
    /// calendar toggle row's write. Same write-then-reread shape as
    /// [`set_clock_format`](Self::set_clock_format).
    pub fn toggle_clock_format(&mut self) -> bool {
        self.toggle_clock_format_with(&settings::GioBackend)
    }

    /// [`toggle_clock_format`](Self::toggle_clock_format) against an
    /// explicit backend (tests substitute a fake).
    pub fn toggle_clock_format_with(&mut self, backend: &dyn SettingsBackend) -> bool {
        let next = match self.settings.clock_format {
            ClockFormat::Twelve => ClockFormat::TwentyFour,
            ClockFormat::TwentyFour => ClockFormat::Twelve,
        };
        self.set_clock_format_with(next, backend)
    }

    /// Write the desktop wallpaper URI against the shared schema,
    /// then re-read at once so paint stays on snapshot truth. A
    /// refused write keeps the last good snapshot, quietly.
    pub fn set_wallpaper_uri(&mut self, uri: &str) -> bool {
        self.set_wallpaper_uri_with(uri, &settings::GioBackend)
    }

    /// [`set_wallpaper_uri`](Self::set_wallpaper_uri) against an
    /// explicit backend (tests substitute a fake).
    pub fn set_wallpaper_uri_with(&mut self, uri: &str, backend: &dyn SettingsBackend) -> bool {
        if !settings::write_wallpaper_uri(backend, uri) {
            return false;
        }
        settings::refresh(&mut self.settings, backend);
        true
    }

    /// Connect the radio caller to the session bus (the slow tick
    /// calls this before [`TileSet::refresh`]). True while the bus is
    /// reachable; without it tiles stay pure sysfs.
    pub fn ensure_radio(&mut self) -> bool {
        self.radio.ensure()
    }

    /// Last radio flags confirmed by [`TileSet::refresh`]. `None`
    /// before the first confirmed read or while the bus is absent —
    /// menus then offer no toggles.
    pub fn radio_state(&self) -> Option<RadioState> {
        self.radio_state
    }

    /// Radio flags a fired toggle is still waiting for (`None` when
    /// no toggle is in flight).
    pub fn network_pending(&self) -> Option<RadioState> {
        self.network_pending
    }

    /// Connect the mixer caller to the session bus (the slow tick
    /// calls this before [`TileSet::refresh`]). True while the bus is
    /// reachable; without it tiles stay pure sysfs.
    pub fn ensure_sound(&mut self) -> bool {
        self.mixer.ensure()
    }

    /// Last output state confirmed by [`TileSet::refresh`]. `None`
    /// before the first confirmed read or while the bus is absent —
    /// menus then offer no toggles.
    pub fn sound_state(&self) -> Option<SoundState> {
        self.sound_state
    }

    /// Output state a fired toggle is still waiting for (`None` when
    /// no toggle is in flight).
    pub fn sound_pending(&self) -> Option<SoundState> {
        self.sound_pending
    }

    /// Fire the mute toggle from the sound tile menu: show the
    /// pending face, then write `Muted`. True once the service accepts
    /// the write; the pending face stays until a later
    /// [`TileSet::refresh`] re-probe confirms it. A refused write
    /// clears the pending face and re-probes sysfs at once, so the
    /// tile never sticks on a toggle that did not happen.
    pub fn set_muted(&mut self, muted: bool) -> bool {
        self.fire_sound_toggle(SoundToggle::Mute(muted))
    }

    /// Fire the volume set from the sound tile menu: show the pending
    /// face, then write `Volume` (clamped to `0.0..=1.0`). Same
    /// pending/confirm shape as [`TileSet::set_muted`].
    pub fn set_volume(&mut self, volume: f64) -> bool {
        self.fire_sound_toggle(SoundToggle::Volume(volume.clamp(0.0, 1.0)))
    }

    /// Nudge output volume down one menu step from the last confirmed
    /// state. Same pending/confirm shape as [`TileSet::set_muted`].
    pub fn volume_down(&mut self) -> bool {
        self.nudge_volume(-VOLUME_STEP)
    }

    /// Nudge output volume up one menu step from the last confirmed
    /// state. Same pending/confirm shape as [`TileSet::set_muted`].
    pub fn volume_up(&mut self) -> bool {
        self.nudge_volume(VOLUME_STEP)
    }

    /// Shared volume-step path: round to whole hundredths so the
    /// pending want matches the re-probe bit for bit.
    fn nudge_volume(&mut self, delta: f64) -> bool {
        let base = self.sound_state.map(|state| state.volume).unwrap_or(1.0);
        let volume = ((base + delta) * 100.0).round() / 100.0;
        self.fire_sound_toggle(SoundToggle::Volume(volume.clamp(0.0, 1.0)))
    }

    /// Shared toggle path: arm the pending face, fire the write, and
    /// roll back to sysfs when the write is refused, reporting why on
    /// stderr so a failed toggle never fails silently.
    fn fire_sound_toggle(&mut self, toggle: SoundToggle) -> bool {
        let mut want = self.sound_state.unwrap_or_default();
        match toggle {
            SoundToggle::Mute(muted) => want.muted = muted,
            SoundToggle::Volume(volume) => want.volume = volume,
        }
        self.sound_pending = Some(want);
        self.sound.state = TileState::Loading;
        let ok = match toggle {
            SoundToggle::Mute(muted) => self.mixer.set_muted(muted),
            SoundToggle::Volume(volume) => self.mixer.set_volume(volume),
        };
        if !ok {
            self.sound_pending = None;
            let what = match toggle {
                SoundToggle::Mute(true) => "mute".to_owned(),
                SoundToggle::Mute(false) => "unmute".to_owned(),
                SoundToggle::Volume(volume) => format!("volume {volume:.2}"),
            };
            if self.sound_state.is_none() {
                eprintln!(
                    "tuna-shell-host: sound {what} failed: \
                     no sound server on the bus; tile keeps its socket probe"
                );
            } else {
                eprintln!(
                    "tuna-shell-host: sound {what} failed: \
                     sound server refused the write; tile keeps its socket probe"
                );
            }
            self.sound = probe_sound(&self.runtime_dir);
        }
        ok
    }

    /// Confirm the sound tile against the output state: clear a
    /// pending toggle whose re-probe matches and keep the pending
    /// face while it does not. The sysfs probe stays the presence
    /// truth, so a confirmed toggle never changes the tile itself —
    /// only the menu rows follow mute and volume. Without a bus this
    /// is a no-op: the tile stays pure sysfs.
    fn apply_sound(&mut self) {
        let Some(state) = self.mixer.snapshot() else {
            return;
        };
        self.sound_state = Some(state);
        if self.sound_pending.is_some_and(|want| want == state) {
            self.sound_pending = None;
        }
        if self.sound_pending.is_some() {
            self.sound.state = TileState::Loading;
        }
    }

    /// Fire the wifi toggle from the network tile menu: show the
    /// pending face, then write `WirelessEnabled`. True once the
    /// service accepts the write; the pending face stays until a
    /// later [`TileSet::refresh`] re-probe confirms it. A refused
    /// write clears the pending face and re-probes sysfs at once, so
    /// the tile never sticks on a toggle that did not happen.
    pub fn set_wifi_enabled(&mut self, enabled: bool) -> bool {
        self.fire_radio_toggle(RadioToggle::Wifi(enabled))
    }

    /// Fire the network toggle from the network tile menu: show the
    /// pending face, then call `Enable`. Same pending/confirm shape
    /// as [`TileSet::set_wifi_enabled`].
    pub fn set_networking_enabled(&mut self, enabled: bool) -> bool {
        self.fire_radio_toggle(RadioToggle::Networking(enabled))
    }

    /// Shared toggle path: arm the pending face, fire the write, and
    /// roll back to sysfs when the write is refused, reporting why on
    /// stderr so a failed toggle never fails silently.
    fn fire_radio_toggle(&mut self, toggle: RadioToggle) -> bool {
        let mut want = self.radio_state.unwrap_or(RadioState {
            wireless: true,
            networking: true,
        });
        match toggle {
            RadioToggle::Wifi(enabled) => want.wireless = enabled,
            RadioToggle::Networking(enabled) => want.networking = enabled,
        }
        self.network_pending = Some(want);
        self.network.state = TileState::Loading;
        let ok = match toggle {
            RadioToggle::Wifi(enabled) => self.radio.set_wireless_enabled(enabled),
            RadioToggle::Networking(enabled) => self.radio.set_networking_enabled(enabled),
        };
        if !ok {
            self.network_pending = None;
            let what = match toggle {
                RadioToggle::Wifi(true) => "wifi on",
                RadioToggle::Wifi(false) => "wifi off",
                RadioToggle::Networking(true) => "network on",
                RadioToggle::Networking(false) => "network off",
            };
            if self.radio_state.is_none() {
                eprintln!(
                    "tuna-shell-host: {what} toggle failed: \
                     no NetworkManager on the bus; tile keeps its sysfs probe"
                );
            } else {
                eprintln!(
                    "tuna-shell-host: {what} toggle failed: \
                     NetworkManager refused the write; tile keeps its sysfs probe"
                );
            }
            self.network = probe_network(&self.net_root);
        }
        ok
    }

    /// Confirm the network tile against the radio flags: clear a
    /// pending toggle whose re-probe matches, keep the pending face
    /// while it does not, and mask links whose radio is off (a wifi
    /// toggle never drops a wired link). Without a bus this is a
    /// no-op: the tile stays pure sysfs.
    fn apply_radio(&mut self) {
        let Some(state) = self.radio.snapshot() else {
            return;
        };
        self.radio_state = Some(state);
        if self.network_pending.is_some_and(|want| want == state) {
            self.network_pending = None;
        }
        if self.network_pending.is_some() {
            self.network.state = TileState::Loading;
        } else if !state.networking {
            self.network.state = TileState::Disconnected;
        } else if !state.wireless {
            self.network = probe_network_where(&self.net_root, |name| !is_wireless_iface(name));
        }
    }

    /// Tiles in strip order (network, power, sound).
    pub fn tiles(&self) -> [&Tile; 3] {
        [&self.network, &self.power, &self.sound]
    }

    /// Runtime dir holding sound sockets (and the wallpaper drop
    /// file the compositor polls).
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }
}

/// Current local time as `HH:MM` (twelve-hour `h:MM` when asked;
/// UTC fallback when the zone fails).
fn clock_text(tz: &str, format: ClockFormat, show_weekday: bool) -> String {
    let zone = if tz.is_empty() {
        jiff::tz::TimeZone::system()
    } else {
        jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC)
    };
    let zoned = jiff::Zoned::now().with_time_zone(zone);
    let time = match format {
        ClockFormat::TwentyFour => zoned.strftime("%H:%M").to_string(),
        ClockFormat::Twelve => zoned.strftime("%-I:%M").to_string(),
    };
    if !show_weekday {
        return time;
    }
    // Two-letter weekday in the calendar header's own spelling, so the
    // prefix never depends on locale data the strip may lack.
    let day = match zoned.date().weekday() {
        jiff::civil::Weekday::Monday => "mo",
        jiff::civil::Weekday::Tuesday => "tu",
        jiff::civil::Weekday::Wednesday => "we",
        jiff::civil::Weekday::Thursday => "th",
        jiff::civil::Weekday::Friday => "fr",
        jiff::civil::Weekday::Saturday => "sa",
        jiff::civil::Weekday::Sunday => "su",
    };
    format!("{day} {time}")
}

/// Which radio flag a menu row toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RadioToggle {
    /// `WirelessEnabled` (wifi row).
    Wifi(bool),
    /// `Enable` (network row).
    Networking(bool),
}

/// Which output control a sound menu row fires.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SoundToggle {
    /// `Muted` (mute row).
    Mute(bool),
    /// `Volume` (volume rows).
    Volume(f64),
}

/// A wireless interface name (kernel `wlan*` or predictable `wl*`
/// names): its link only counts while the wifi radio is on.
fn is_wireless_iface(name: &str) -> bool {
    name.starts_with("wl")
}

/// Network tile from interface operstate: `Ready` when any non-loopback
/// interface reports `up`, `Disconnected` when interfaces exist but
/// none is up (or the tree is absent — normal in containers),
/// `Error` when the tree exists but cannot be read.
fn probe_network(net_root: &Path) -> Tile {
    probe_network_where(net_root, |_| true)
}

/// [`probe_network`](probe_network), counting only interfaces
/// `link_ok` accepts. The radio overlay passes wired-only while the
/// wifi radio is off, so a wifi toggle never drops a wired link.
fn probe_network_where(net_root: &Path, link_ok: impl Fn(&str) -> bool) -> Tile {
    let mut tile = Tile::new(ServiceKind::Network);
    let entries = match std::fs::read_dir(net_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tile.state = TileState::Disconnected;
            return tile;
        }
        Err(_) => {
            tile.state = TileState::Error;
            return tile;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == "lo" {
            continue;
        }
        // Undecodable names always count, as before: only a readable
        // name the filter rejects is skipped.
        if name.to_str().is_some_and(|name| !link_ok(name)) {
            continue;
        }
        let operstate = std::fs::read_to_string(entry.path().join("operstate"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        if operstate == "up" {
            tile.state = TileState::Ready;
            return tile;
        }
    }
    tile.state = TileState::Disconnected;
    tile
}

/// Power tile from the supply tree: `Ready` with the first battery's
/// percent when a battery exists, `Ready` without a level on AC-only
/// machines, `Disconnected` with no supplies, `Error` on unreadable
/// entries.
fn probe_power(power_root: &Path) -> Tile {
    let mut tile = Tile::new(ServiceKind::Power);
    let entries = match std::fs::read_dir(power_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tile.state = TileState::Disconnected;
            return tile;
        }
        Err(_) => {
            tile.state = TileState::Error;
            return tile;
        }
    };
    let mut any = false;
    for entry in entries.flatten() {
        any = true;
        let dir = entry.path();
        let kind = std::fs::read_to_string(dir.join("type"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        if kind == "Battery" {
            let level = std::fs::read_to_string(dir.join("capacity"))
                .unwrap_or_default()
                .trim()
                .parse::<u8>()
                .ok();
            match level {
                Some(level) => {
                    tile.state = TileState::Ready;
                    tile.level = Some(level.min(100));
                    return tile;
                }
                None => {
                    tile.state = TileState::Error;
                    return tile;
                }
            }
        }
    }
    tile.state = if any {
        // Supplies exist but no battery: plugged-in machine.
        TileState::Ready
    } else {
        TileState::Disconnected
    };
    tile
}

/// Sound tile from socket presence only — existence checks, never a
/// connection, so a dead daemon cannot hang the probe.
fn probe_sound(runtime_dir: &Path) -> Tile {
    let mut tile = Tile::new(ServiceKind::Sound);
    for socket in ["pipewire-0", "pulse/native"] {
        if runtime_dir.join(socket).exists() {
            tile.state = TileState::Ready;
            return tile;
        }
    }
    tile.state = TileState::Disconnected;
    tile
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn roots() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path().join("net");
        let power = dir.path().join("power");
        let run = dir.path().join("run");
        std::fs::create_dir_all(&net).unwrap();
        std::fs::create_dir_all(&power).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        (dir, net, power, run)
    }

    fn iface(net: &Path, name: &str, state: &str) {
        let dir = net.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("operstate"), state).unwrap();
    }

    #[test]
    fn clock_is_always_ready_and_shaped_like_time() {
        let (_dir, net, power, run) = roots();
        let mut set = TileSet::with_roots("", &net, &power, &run);
        set.refresh();
        assert_eq!(set.clock.len(), 5);
        assert_eq!(set.clock.as_bytes()[2], b':');
        assert!(set.clock[..2].parse::<u32>().is_ok());
        assert!(set.clock[3..].parse::<u32>().is_ok());
    }

    #[test]
    fn twelve_hour_clock_drops_the_leading_zero() {
        let text = clock_text("", ClockFormat::Twelve, false);
        assert!(text.contains(':'));
        let (hour, minute) = text.split_once(':').unwrap();
        let hour: u32 = hour.parse().unwrap();
        assert!((1..=12).contains(&hour));
        assert_eq!(minute.len(), 2);
        assert!(minute.parse::<u32>().is_ok());
    }

    #[test]
    fn tile_set_starts_with_default_settings() {
        let (_dir, net, power, run) = roots();
        let set = TileSet::with_roots("", &net, &power, &run);
        assert_eq!(set.settings, ShellSettings::default());
    }

    #[test]
    fn clock_toggle_writes_then_rereads_the_fake_backend() {
        use crate::settings::{MapBackend, CLOCK_FORMAT_KEY, INTERFACE_SCHEMA};
        let (_dir, net, power, run) = roots();
        let mut set = TileSet::with_roots("", &net, &power, &run);
        let backend = MapBackend::with_values(&[(INTERFACE_SCHEMA, CLOCK_FORMAT_KEY, "24h")]);
        set.refresh_with(&backend);
        assert_eq!(set.settings.clock_format, ClockFormat::TwentyFour);
        assert!(set.toggle_clock_format_with(&backend));
        assert_eq!(
            backend
                .string(INTERFACE_SCHEMA, CLOCK_FORMAT_KEY)
                .as_deref(),
            Some("12h")
        );
        assert_eq!(set.settings.clock_format, ClockFormat::Twelve);
        assert!(!set.clock.is_empty());
        assert!(set.toggle_clock_format_with(&backend));
        assert_eq!(
            backend
                .string(INTERFACE_SCHEMA, CLOCK_FORMAT_KEY)
                .as_deref(),
            Some("24h")
        );
        assert_eq!(set.settings.clock_format, ClockFormat::TwentyFour);
    }

    #[test]
    fn refused_clock_write_keeps_the_last_good_snapshot() {
        struct Refusing;
        impl SettingsBackend for Refusing {
            fn string(&self, _schema: &str, _key: &str) -> Option<String> {
                None
            }
            fn set_string(&self, _schema: &str, _key: &str, _value: &str) -> bool {
                false
            }
        }
        let (_dir, net, power, run) = roots();
        let mut set = TileSet::with_roots("", &net, &power, &run);
        let before = set.settings.clone();
        assert!(!set.set_clock_format_with(ClockFormat::Twelve, &Refusing));
        assert_eq!(set.settings, before);
        assert!(!set.toggle_clock_format_with(&Refusing));
        assert_eq!(set.settings, before);
    }

    #[test]
    fn wallpaper_set_writes_then_rereads_the_fake_backend() {
        use crate::settings::{MapBackend, BACKGROUND_SCHEMA, PICTURE_URI_KEY};
        let (_dir, net, power, run) = roots();
        let mut set = TileSet::with_roots("", &net, &power, &run);
        let backend = MapBackend::default();
        set.refresh_with(&backend);
        assert_eq!(set.settings.wallpaper_uri, None);
        assert!(set.set_wallpaper_uri_with("file:///wall.png", &backend));
        assert_eq!(
            backend
                .string(BACKGROUND_SCHEMA, PICTURE_URI_KEY)
                .as_deref(),
            Some("file:///wall.png")
        );
        assert_eq!(
            set.settings.wallpaper_uri.as_deref(),
            Some("file:///wall.png")
        );
        // Paint still reads only the snapshot: a fresh refresh from
        // the same backend restores the URI, like a session restart.
        let mut restarted = TileSet::with_roots("", &net, &power, &run);
        restarted.refresh_with(&backend);
        assert_eq!(
            restarted.settings.wallpaper_uri.as_deref(),
            Some("file:///wall.png")
        );
    }

    #[test]
    fn refused_wallpaper_write_keeps_the_last_good_snapshot() {
        struct Refusing;
        impl SettingsBackend for Refusing {
            fn string(&self, _schema: &str, _key: &str) -> Option<String> {
                None
            }
            fn set_string(&self, _schema: &str, _key: &str, _value: &str) -> bool {
                false
            }
        }
        let (_dir, net, power, run) = roots();
        let mut set = TileSet::with_roots("", &net, &power, &run);
        let before = set.settings.clone();
        assert!(!set.set_wallpaper_uri_with("file:///wall.png", &Refusing));
        assert_eq!(set.settings, before);
        // Blank URIs are refused before the backend is touched.
        assert!(!set.set_wallpaper_uri_with("   ", &Refusing));
        assert_eq!(set.settings, before);
    }

    #[test]
    fn weekday_prefix_prepends_the_calendar_spelling() {
        let plain = clock_text("", ClockFormat::TwentyFour, false);
        assert_eq!(plain.len(), 5);
        let prefixed = clock_text("", ClockFormat::TwentyFour, true);
        let (day, time) = prefixed.split_once(' ').expect("weekday prefix");
        assert!(["mo", "tu", "we", "th", "fr", "sa", "su"].contains(&day));
        assert_eq!(time, plain);
    }

    #[test]
    fn weekday_toggle_updates_the_snapshot_and_persists() {
        let (dir, net, power, run) = roots();
        let path = dir.path().join(crate::prefs::PREFS_FILE);
        let mut set = TileSet::with_roots("", &net, &power, &run);
        set.restore_prefs(&path);
        assert!(!set.prefs.clock_show_weekday);
        assert!(set.toggle_clock_show_weekday());
        assert!(set.prefs.clock_show_weekday);
        assert!(set.clock.contains(' '));
        // The pinned file carries the flip without hand editing.
        assert!(crate::prefs::load(&path).clock_show_weekday);
        assert!(set.toggle_clock_show_weekday());
        assert!(!set.prefs.clock_show_weekday);
        assert!(!crate::prefs::load(&path).clock_show_weekday);
    }

    #[test]
    fn changed_tuna_option_survives_a_restart() {
        let (dir, net, power, run) = roots();
        let path = dir.path().join(crate::prefs::PREFS_FILE);
        let mut set = TileSet::with_roots("", &net, &power, &run);
        set.restore_prefs(&path);
        assert!(set.set_clock_show_weekday(true));
        // Fresh set, same file: the kept value comes back, and the
        // clock text carries it without another write.
        let mut restarted = TileSet::with_roots("", &net, &power, &run);
        assert!(!restarted.prefs.clock_show_weekday);
        restarted.restore_prefs(&path);
        assert!(restarted.prefs.clock_show_weekday);
        assert!(restarted.clock.contains(' '));
    }

    #[test]
    fn network_up_down_and_absent() {
        let (_dir, net, power, run) = roots();
        // No interfaces at all: disconnected, not error.
        let mut set = TileSet::with_roots("", &net, &power, &run);
        set.refresh();
        assert_eq!(set.network.state, TileState::Disconnected);
        // Loopback alone never counts.
        iface(&net, "lo", "unknown");
        set.refresh();
        assert_eq!(set.network.state, TileState::Disconnected);
        // A downed interface is still disconnected.
        iface(&net, "eth0", "down\n");
        set.refresh();
        assert_eq!(set.network.state, TileState::Disconnected);
        // One link up is ready.
        iface(&net, "eth0", "up\n");
        set.refresh();
        assert_eq!(set.network.state, TileState::Ready);
    }

    #[test]
    fn unreadable_tree_still_terminates() {
        // No-hang assertion (not a state assertion: outcomes differ for
        // root vs unprivileged users, but the probe must always return).
        let (_dir, net, power, run) = roots();
        std::fs::set_permissions(&net, std::fs::Permissions::from_mode(0o000)).unwrap();
        let wall = std::time::Instant::now();
        let mut set = TileSet::with_roots("", &net, &power, &run);
        set.refresh();
        assert!(wall.elapsed() < std::time::Duration::from_secs(2));
        std::fs::set_permissions(&net, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn power_battery_ac_and_absent() {
        let (_dir, net, power, run) = roots();
        let mut set = TileSet::with_roots("", &net, &power, &run);
        // Empty tree: disconnected.
        set.refresh();
        assert_eq!(set.power.state, TileState::Disconnected);
        // AC-only machine: ready without a level.
        let ac = power.join("AC");
        std::fs::create_dir_all(&ac).unwrap();
        std::fs::write(ac.join("type"), "Mains\n").unwrap();
        set.refresh();
        assert_eq!(set.power.state, TileState::Ready);
        assert_eq!(set.power.level, None);
        // Battery: ready with percent.
        let bat = power.join("BAT0");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::write(bat.join("type"), "Battery\n").unwrap();
        std::fs::write(bat.join("capacity"), "42\n").unwrap();
        set.refresh();
        assert_eq!(set.power.state, TileState::Ready);
        assert_eq!(set.power.level, Some(42));
        // Unparseable capacity is an error, not a guess.
        std::fs::write(bat.join("capacity"), "lots\n").unwrap();
        set.refresh();
        assert_eq!(set.power.state, TileState::Error);
    }

    #[test]
    fn sound_follows_socket_presence() {
        let (_dir, net, power, run) = roots();
        let mut set = TileSet::with_roots("", &net, &power, &run);
        set.refresh();
        assert_eq!(set.sound.state, TileState::Disconnected);
        std::fs::write(run.join("pipewire-0"), "").unwrap();
        set.refresh();
        assert_eq!(set.sound.state, TileState::Ready);
    }

    #[test]
    fn toggles_without_bus_fail_with_no_pending_face() {
        let (_dir, net, power, run) = roots();
        iface(&net, "eth0", "up\n");
        let mut set = TileSet::with_roots("", &net, &power, &run);
        set.refresh();
        assert_eq!(set.network.state, TileState::Ready);
        assert!(!set.set_wifi_enabled(false));
        assert!(!set.set_networking_enabled(false));
        assert_eq!(set.network_pending(), None);
        assert_eq!(set.radio_state(), None);
        assert_eq!(
            set.network.state,
            TileState::Ready,
            "refused toggles roll back to sysfs"
        );
    }

    #[test]
    fn sound_toggles_without_bus_fail_with_no_pending_face() {
        let (_dir, net, power, run) = roots();
        std::fs::write(run.join("pipewire-0"), "").unwrap();
        let mut set = TileSet::with_roots("", &net, &power, &run);
        set.refresh();
        assert_eq!(set.sound.state, TileState::Ready);
        assert!(!set.set_muted(true));
        assert!(!set.set_volume(0.5));
        assert!(!set.volume_down());
        assert!(!set.volume_up());
        assert_eq!(set.sound_pending(), None);
        assert_eq!(set.sound_state(), None);
        assert_eq!(
            set.sound.state,
            TileState::Ready,
            "refused toggles roll back to sysfs"
        );
    }

    /// Live stub sound service over a private bus: a stub
    /// sound-server control speaks the mute/volume contract to the
    /// real [`SoundMixer`] caller. Each test owns its daemon and
    /// address, so no session-bus name is touched and parallel tests
    /// never share a bus. Skips (loudly) when no bus daemon is
    /// available.
    mod sound_bus {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use super::super::{SoundMixer, SoundState, TileSet, TileState, SND_NAME, SND_PATH};
        use super::{iface, roots};

        /// Mutable stub output behind the served object.
        #[derive(Debug)]
        struct StubOutput {
            muted: bool,
            volume: f64,
        }

        /// Stub `org.tuna.Sound`: the mute flag plus output volume
        /// over the real bus identity.
        #[derive(Debug)]
        struct StubSound {
            output: Arc<Mutex<StubOutput>>,
        }

        #[zbus::interface(name = "org.tuna.Sound")]
        impl StubSound {
            #[zbus(property)]
            fn muted(&self) -> bool {
                self.output.lock().expect("stub lock").muted
            }

            #[zbus(property)]
            fn set_muted(&mut self, muted: bool) {
                self.output.lock().expect("stub lock").muted = muted;
            }

            #[zbus(property)]
            fn volume(&self) -> f64 {
                self.output.lock().expect("stub lock").volume
            }

            #[zbus(property)]
            fn set_volume(&mut self, volume: f64) {
                self.output.lock().expect("stub lock").volume = volume.clamp(0.0, 1.0);
            }
        }

        /// Private bus daemon plus its address, or `None` when no
        /// daemon is usable (the caller skips the test).
        fn private_bus() -> Option<(String, std::process::Child)> {
            let mut child = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let stdout = child.stdout.take()?;
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(stdout);
                use std::io::BufRead;
                let mut line = String::new();
                let line = reader.read_line(&mut line).ok().map(|_| line);
                let _ = tx.send(line);
            });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Some(line)) if !line.trim().is_empty() => Some((line.trim().to_owned(), child)),
                _ => {
                    let _ = child.kill();
                    None
                }
            }
        }

        /// Serve the stub on the private bus, retrying while the
        /// daemon starts listening. Returns the serving connection
        /// (kept alive by the caller) and the shared output.
        fn serve_stub(
            address: &str,
        ) -> Option<(zbus::blocking::Connection, Arc<Mutex<StubOutput>>)> {
            let output = Arc::new(Mutex::new(StubOutput {
                muted: false,
                volume: 1.0,
            }));
            for _ in 0..50 {
                let builder = zbus::blocking::connection::Builder::address(address).ok()?;
                let stub = StubSound {
                    output: output.clone(),
                };
                let builder = match builder.serve_at(SND_PATH, stub) {
                    Ok(builder) => builder,
                    Err(_) => return None,
                };
                match builder.build() {
                    Ok(conn) => {
                        match conn.request_name_with_flags(
                            SND_NAME,
                            zbus::fdo::RequestNameFlags::DoNotQueue.into(),
                        ) {
                            Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                                return Some((conn, output));
                            }
                            _ => return None,
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            None
        }

        /// Caller connected to the private bus, retrying while the
        /// daemon starts listening.
        fn owned_mixer(address: &str) -> Option<SoundMixer> {
            let mut mixer = SoundMixer::new();
            for _ in 0..50 {
                if mixer.connect(Some(address)) {
                    return Some(mixer);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            None
        }

        fn stub_state(output: &Arc<Mutex<StubOutput>>) -> SoundState {
            let output = output.lock().expect("stub lock");
            SoundState {
                muted: output.muted,
                volume: output.volume,
            }
        }

        #[test]
        fn mute_round_trip_silences_and_restores_output() {
            let Some((address, mut daemon)) = private_bus() else {
                eprintln!("SKIP live sound test: no usable dbus-daemon on PATH");
                return;
            };
            let Some((_server, output)) = serve_stub(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live sound test: private bus refused connections");
                return;
            };
            let Some(mixer) = owned_mixer(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live sound test: private bus refused connections");
                return;
            };
            let (_dir, net, power, run) = roots();
            std::fs::write(run.join("pipewire-0"), "").unwrap();
            iface(&net, "eth0", "up\n");
            let mut set = TileSet::with_roots("", &net, &power, &run);
            set.mixer = mixer;
            set.refresh();
            assert_eq!(set.sound.state, TileState::Ready);

            // Muting silences output on the real (stub) server.
            assert!(set.set_muted(true));
            assert!(stub_state(&output).muted);
            // Pending face until the re-probe confirms.
            assert_eq!(set.sound.state, TileState::Loading);
            assert!(set.sound_pending().is_some());
            set.refresh();
            assert_eq!(set.sound_pending(), None);
            assert_eq!(
                set.sound_state(),
                Some(SoundState {
                    muted: true,
                    volume: 1.0,
                })
            );
            assert_eq!(
                set.sound.state,
                TileState::Ready,
                "mute never drops the socket presence"
            );

            // Unmuting restores output on re-probe.
            assert!(set.set_muted(false));
            assert!(!stub_state(&output).muted);
            set.refresh();
            assert_eq!(set.sound_pending(), None);
            assert_eq!(
                set.sound_state(),
                Some(SoundState {
                    muted: false,
                    volume: 1.0,
                })
            );
            let _ = daemon.kill();
        }

        #[test]
        fn volume_nudge_round_trip_confirms_on_reprobe() {
            let Some((address, mut daemon)) = private_bus() else {
                eprintln!("SKIP live sound test: no usable dbus-daemon on PATH");
                return;
            };
            let Some((_server, output)) = serve_stub(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live sound test: private bus refused connections");
                return;
            };
            let Some(mixer) = owned_mixer(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live sound test: private bus refused connections");
                return;
            };
            let (_dir, net, power, run) = roots();
            std::fs::write(run.join("pipewire-0"), "").unwrap();
            let mut set = TileSet::with_roots("", &net, &power, &run);
            set.mixer = mixer;
            set.refresh();
            assert_eq!(set.sound.state, TileState::Ready);

            assert!(set.set_volume(0.5));
            assert_eq!(stub_state(&output).volume, 0.5);
            assert_eq!(set.sound.state, TileState::Loading);
            set.refresh();
            assert_eq!(set.sound_pending(), None);
            assert_eq!(
                set.sound_state(),
                Some(SoundState {
                    muted: false,
                    volume: 0.5,
                })
            );

            assert!(set.volume_down());
            assert_eq!(stub_state(&output).volume, 0.45);
            set.refresh();
            assert_eq!(set.sound_pending(), None);
            assert_eq!(set.sound_state().map(|state| state.volume), Some(0.45));

            assert!(set.volume_up());
            set.refresh();
            assert_eq!(set.sound_state().map(|state| state.volume), Some(0.5));
            let _ = daemon.kill();
        }

        #[test]
        fn outside_sound_change_updates_menu_state_within_one_tick() {
            let Some((address, mut daemon)) = private_bus() else {
                eprintln!("SKIP live sound test: no usable dbus-daemon on PATH");
                return;
            };
            let Some((_server, output)) = serve_stub(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live sound test: private bus refused connections");
                return;
            };
            let Some(mixer) = owned_mixer(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live sound test: private bus refused connections");
                return;
            };
            let (_dir, net, power, run) = roots();
            std::fs::write(run.join("pipewire-0"), "").unwrap();
            let mut set = TileSet::with_roots("", &net, &power, &run);
            set.mixer = mixer;
            set.refresh();
            assert_eq!(set.sound.state, TileState::Ready);

            // An outside change (another client muting and lowering
            // the volume, not this tile) reaches the cached menu
            // state on the very next re-probe: one poll tick, no
            // toggle armed.
            {
                let mut output = output.lock().expect("stub lock");
                output.muted = true;
                output.volume = 0.25;
            }
            set.refresh();
            assert_eq!(
                set.sound_state(),
                Some(SoundState {
                    muted: true,
                    volume: 0.25,
                })
            );
            assert!(set.sound_pending().is_none());
            assert_eq!(
                set.sound.state,
                TileState::Ready,
                "mute never drops the socket presence"
            );
            let _ = daemon.kill();
        }
    }

    /// Live stub platform service over a private bus: a stub
    /// NetworkManager speaks the radio-flag contract to the real
    /// [`NetworkRadio`] caller. Each test owns its daemon and address,
    /// so no session-bus name is touched and parallel tests never
    /// share a bus. Skips (loudly) when no bus daemon is available.
    mod radio_bus {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use super::super::{NetworkRadio, RadioState, TileSet, TileState, NM_NAME, NM_PATH};
        use super::{iface, roots};

        /// Mutable stub flags behind the served object.
        #[derive(Debug)]
        struct StubFlags {
            wireless: bool,
            networking: bool,
        }

        /// Stub `org.freedesktop.NetworkManager`: the two radio flags
        /// plus `Enable`, over the real bus identity.
        #[derive(Debug)]
        struct StubNm {
            flags: Arc<Mutex<StubFlags>>,
        }

        #[zbus::interface(name = "org.freedesktop.NetworkManager")]
        impl StubNm {
            #[zbus(property)]
            fn wireless_enabled(&self) -> bool {
                self.flags.lock().expect("stub lock").wireless
            }

            #[zbus(property)]
            fn set_wireless_enabled(&mut self, enabled: bool) {
                self.flags.lock().expect("stub lock").wireless = enabled;
            }

            #[zbus(property)]
            fn networking_enabled(&self) -> bool {
                self.flags.lock().expect("stub lock").networking
            }

            fn enable(&mut self, enabled: bool) {
                self.flags.lock().expect("stub lock").networking = enabled;
            }
        }

        /// Private bus daemon plus its address, or `None` when no
        /// daemon is usable (the caller skips the test).
        fn private_bus() -> Option<(String, std::process::Child)> {
            let mut child = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let stdout = child.stdout.take()?;
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(stdout);
                use std::io::BufRead;
                let mut line = String::new();
                let line = reader.read_line(&mut line).ok().map(|_| line);
                let _ = tx.send(line);
            });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Some(line)) if !line.trim().is_empty() => Some((line.trim().to_owned(), child)),
                _ => {
                    let _ = child.kill();
                    None
                }
            }
        }

        /// Serve the stub on the private bus, retrying while the
        /// daemon starts listening. Returns the serving connection
        /// (kept alive by the caller) and the shared flags.
        fn serve_stub(
            address: &str,
        ) -> Option<(zbus::blocking::Connection, Arc<Mutex<StubFlags>>)> {
            let flags = Arc::new(Mutex::new(StubFlags {
                wireless: true,
                networking: true,
            }));
            for _ in 0..50 {
                let builder = zbus::blocking::connection::Builder::address(address).ok()?;
                let stub = StubNm {
                    flags: flags.clone(),
                };
                let builder = match builder.serve_at(NM_PATH, stub) {
                    Ok(builder) => builder,
                    Err(_) => return None,
                };
                match builder.build() {
                    Ok(conn) => {
                        match conn.request_name_with_flags(
                            NM_NAME,
                            zbus::fdo::RequestNameFlags::DoNotQueue.into(),
                        ) {
                            Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                                return Some((conn, flags));
                            }
                            _ => return None,
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            None
        }

        /// Caller connected to the private bus, retrying while the
        /// daemon starts listening.
        fn owned_radio(address: &str) -> Option<NetworkRadio> {
            let mut radio = NetworkRadio::new();
            for _ in 0..50 {
                if radio.connect(Some(address)) {
                    return Some(radio);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            None
        }

        fn stub_state(flags: &Arc<Mutex<StubFlags>>) -> RadioState {
            let flags = flags.lock().expect("stub lock");
            RadioState {
                wireless: flags.wireless,
                networking: flags.networking,
            }
        }

        #[test]
        fn wifi_toggle_flips_stub_and_tile_follows_after_reprobe() {
            let Some((address, mut daemon)) = private_bus() else {
                eprintln!("SKIP live radio test: no usable dbus-daemon on PATH");
                return;
            };
            let Some((_server, flags)) = serve_stub(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live radio test: private bus refused connections");
                return;
            };
            let Some(radio) = owned_radio(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live radio test: private bus refused connections");
                return;
            };
            let (_dir, net, power, run) = roots();
            iface(&net, "wlan0", "up\n");
            let mut set = TileSet::with_roots("", &net, &power, &run);
            set.radio = radio;
            set.refresh();
            assert_eq!(set.network.state, TileState::Ready);

            // Toggling wifi off changes the real (stub) radio state.
            assert!(set.set_wifi_enabled(false));
            assert_eq!(
                stub_state(&flags),
                RadioState {
                    wireless: false,
                    networking: true,
                }
            );
            // Pending face until the re-probe confirms.
            assert_eq!(set.network.state, TileState::Loading);
            assert!(set.network_pending().is_some());
            set.refresh();
            assert_eq!(set.network_pending(), None);
            assert_eq!(
                set.network.state,
                TileState::Disconnected,
                "tile follows the wifi radio off"
            );

            // Toggling back on restores the link on re-probe.
            assert!(set.set_wifi_enabled(true));
            set.refresh();
            assert_eq!(set.network_pending(), None);
            assert_eq!(set.network.state, TileState::Ready);
            let _ = daemon.kill();
        }

        #[test]
        fn outside_wifi_change_updates_tile_within_one_tick() {
            let Some((address, mut daemon)) = private_bus() else {
                eprintln!("SKIP live radio test: no usable dbus-daemon on PATH");
                return;
            };
            let Some((_server, flags)) = serve_stub(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live radio test: private bus refused connections");
                return;
            };
            let Some(radio) = owned_radio(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live radio test: private bus refused connections");
                return;
            };
            let (_dir, net, power, run) = roots();
            iface(&net, "wlan0", "up\n");
            let mut set = TileSet::with_roots("", &net, &power, &run);
            set.radio = radio;
            set.refresh();
            assert_eq!(set.network.state, TileState::Ready);
            assert!(set.network_pending().is_none());

            // An outside change (another client flipping the radio,
            // not this tile) reaches the tile on the very next
            // re-probe: one poll tick, no toggle armed.
            flags.lock().expect("stub lock").wireless = false;
            set.refresh();
            assert_eq!(
                set.radio_state(),
                Some(RadioState {
                    wireless: false,
                    networking: true,
                })
            );
            assert!(set.network_pending().is_none());
            assert_eq!(
                set.network.state,
                TileState::Disconnected,
                "outside wifi change reaches the tile within one tick"
            );

            // And back on the same way.
            flags.lock().expect("stub lock").wireless = true;
            set.refresh();
            assert_eq!(set.network.state, TileState::Ready);
            assert!(set.network_pending().is_none());
            let _ = daemon.kill();
        }

        #[test]
        fn wired_link_survives_wifi_off() {
            let Some((address, mut daemon)) = private_bus() else {
                eprintln!("SKIP live radio test: no usable dbus-daemon on PATH");
                return;
            };
            let Some((_server, _flags)) = serve_stub(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live radio test: private bus refused connections");
                return;
            };
            let Some(radio) = owned_radio(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live radio test: private bus refused connections");
                return;
            };
            let (_dir, net, power, run) = roots();
            iface(&net, "eth0", "up\n");
            let mut set = TileSet::with_roots("", &net, &power, &run);
            set.radio = radio;
            assert!(set.set_wifi_enabled(false));
            set.refresh();
            assert_eq!(
                set.network.state,
                TileState::Ready,
                "a wifi toggle never drops a wired link"
            );
            let _ = daemon.kill();
        }

        #[test]
        fn networking_toggle_drops_and_restores_tile() {
            let Some((address, mut daemon)) = private_bus() else {
                eprintln!("SKIP live radio test: no usable dbus-daemon on PATH");
                return;
            };
            let Some((_server, flags)) = serve_stub(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live radio test: private bus refused connections");
                return;
            };
            let Some(radio) = owned_radio(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live radio test: private bus refused connections");
                return;
            };
            let (_dir, net, power, run) = roots();
            iface(&net, "eth0", "up\n");
            let mut set = TileSet::with_roots("", &net, &power, &run);
            set.radio = radio;
            set.refresh();
            assert_eq!(set.network.state, TileState::Ready);

            assert!(set.set_networking_enabled(false));
            assert!(!stub_state(&flags).networking);
            assert_eq!(set.network.state, TileState::Loading);
            set.refresh();
            assert_eq!(set.network_pending(), None);
            assert_eq!(set.network.state, TileState::Disconnected);

            assert!(set.set_networking_enabled(true));
            set.refresh();
            assert_eq!(set.network.state, TileState::Ready);
            let _ = daemon.kill();
        }
    }

    #[test]
    fn probes_never_hang_on_missing_roots() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let mut set = TileSet::with_roots(
            "",
            &missing.join("net"),
            &missing.join("power"),
            &missing.join("run"),
        );
        let wall = std::time::Instant::now();
        set.refresh();
        assert!(wall.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(set.network.state, TileState::Disconnected);
        assert_eq!(set.power.state, TileState::Disconnected);
        assert_eq!(set.sound.state, TileState::Disconnected);
    }
}
