//! Panel status tiles: clock plus service presence (002 panel).
//!
//! Every tile is a [`TileState`] the strip renders honestly: `Ready`
//! only when the underlying source is actually present, `Disconnected`
//! when it is absent, `Error` when probing itself fails, `Loading`
//! before the first probe. Probes read local sysfs files or check
//! socket paths — never connect, never block, never spawn — so absent
//! services cannot hang the shell. Live D-Bus bindings (NetworkManager,
//! UPower, PipeWire) attach later behind this same state shape; the
//! sysfs/socket probes below are the presence detection they build on.

use std::path::{Path, PathBuf};

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
    /// Current clock text (`HH:MM`).
    pub clock: String,
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
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/run/user/0"));
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
            network: Tile::new(ServiceKind::Network),
            power: Tile::new(ServiceKind::Power),
            sound: Tile::new(ServiceKind::Sound),
        }
    }

    /// Refresh the clock text and re-probe every service. Pure local
    /// reads: bounded, nonblocking, spawn-free.
    pub fn refresh(&mut self) {
        self.clock = clock_text(&self.tz);
        self.network = probe_network(&self.net_root);
        self.power = probe_power(&self.power_root);
        self.sound = probe_sound(&self.runtime_dir);
    }

    /// Tiles in strip order (network, power, sound).
    pub fn tiles(&self) -> [&Tile; 3] {
        [&self.network, &self.power, &self.sound]
    }
}

/// Current local time as `HH:MM` (UTC fallback when the zone fails).
fn clock_text(tz: &str) -> String {
    let zone = if tz.is_empty() {
        jiff::tz::TimeZone::system()
    } else {
        jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC)
    };
    jiff::Zoned::now()
        .with_time_zone(zone)
        .strftime("%H:%M")
        .to_string()
}

/// Network tile from interface operstate: `Ready` when any non-loopback
/// interface reports `up`, `Disconnected` when interfaces exist but
/// none is up (or the tree is absent — normal in containers),
/// `Error` when the tree exists but cannot be read.
fn probe_network(net_root: &Path) -> Tile {
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
