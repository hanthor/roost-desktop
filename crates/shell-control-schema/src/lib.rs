//! Versioned shell control protocol (Wave 2 stream 1).
//!
//! Private local IPC between the compositor (authoritative) and the shell
//! host: explicit major/minor negotiation, full snapshots on (re)connect,
//! ordered incremental changes, request-ID-bearing commands, and
//! compositor-issued activation tokens.
//!
//! Wire format per ADR 0002: each frame is a u32 little-endian length prefix
//! followed by a postcard body. Frames larger than [`MAX_FRAME_BYTES`] are
//! rejected before any postcard decoding happens.

mod brightness;
pub use brightness::*;

use serde::{Deserialize, Serialize};

/// Maximum accepted frame body size in bytes (1 MiB, per ADR 0002).
///
/// Enforced in [`decode_frame`] before postcard decoding, so a hostile or
/// buggy peer cannot force a large allocation.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Maximum window title length in bytes.
///
/// Titles are untrusted text (spec 001); over-long titles are rejected in
/// [`decode_frame`] with [`DecodeError::TitleTooLong`].
pub const MAX_TITLE_LEN: usize = 512;

/// Layer-shell namespace for the overview surface (compositor ↔ shell contract).
/// The compositor matches on this to identify the overview; the shell advertises it
/// when creating the overview layer.
pub const OVERVIEW_NAMESPACE: &str = "roost-shell-overview";

/// Layer-shell namespace for the panel surface (compositor ↔ shell contract).
/// The shell advertises this when creating the main panel layer.
pub const PANEL_NAMESPACE: &str = "roost-shell-panel";

/// Layer-shell namespace for the banner/notifications surface (compositor ↔ shell contract).
/// The shell advertises this when creating the banner/notification layer.
pub const BANNER_NAMESPACE: &str = "roost-shell-banner";

/// Panel height in pixels (compositor ↔ shell contract).
///
/// The shell renders the panel at this height; the compositor uses this to inset
/// the work area and position the Activities trigger strip. Must stay in sync
/// across both implementations.
pub const PANEL_HEIGHT: u32 = 32;

/// Opaque window identifier minted by the compositor.
///
/// Never reused during a compositor session (spec 001); the shell must treat
/// it as an opaque key, never as a pointer, index, or capability.
pub type WindowId = u64;

/// Opaque workspace identifier minted by the compositor.
pub type WorkspaceId = u64;

/// Protocol version with explicit major/minor negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    /// Incompatible change counter. Any mismatch is a hard error.
    pub major: u16,
    /// Backwards-compatible extension counter within a major version.
    pub minor: u16,
}

impl ProtocolVersion {
    /// Version spoken by this crate.
    ///
    /// `0.2` adds a per-window `activation_token` to `WindowInfo`. The
    /// postcard body is positional, so the field addition is
    /// wire-incompatible with `0.1` despite the minor bump; both sides
    /// always ship from this workspace, and a mixed pair fails closed at
    /// decode time.
    ///
    /// `0.3` appends the compositor-to-shell `Overview` message (002
    /// overview triggers). Enum variants serialize by index and the new
    /// variant sits last, so earlier discriminants are untouched.
    ///
    /// `0.4` appends the compositor-to-shell `Switcher` message (002
    /// Alt-Tab drive), again last for the same reason.
    ///
    /// `0.5` appends the shell-to-compositor `CloseWindow` command
    /// (dock quit), again last for the same reason.
    ///
    /// `0.6` appends `locked` to the compositor-to-shell `Snapshot` and
    /// the shell-to-compositor `Lock` command (session-lock). The struct
    /// field sits last and the enum variant sits last, so earlier
    /// positions are untouched; like `0.2`, the body is still
    /// wire-incompatible with older peers despite the minor bump, and
    /// both sides always ship from this workspace.
    ///
    /// `0.7` appends the compositor-to-shell `Outputs` message
    /// (multi-monitor inventory), again last for the same reason.
    ///
    /// `0.8` appends the compositor-to-shell `Environment` message
    /// (session variables such as `DISPLAY` once XWayland is up), again
    /// last for the same reason.
    ///
    /// `0.9` appends the shell-to-compositor `SetIdleTimeout` command
    /// (GNOME's idle and lock settings, #63), again last.
    ///
    /// `0.10` appends the shell-to-compositor `SetOverviewSearch`
    /// command (search hides the workspace view, as in GNOME), again last.
    ///
    /// `0.11` appends the shell-to-compositor `SetInputSettings` command
    /// (GNOME's keyboard, touchpad and mouse settings), again last.
    ///
    /// `0.12` appends the shell-to-compositor `SetOverviewAppGrid`
    /// command (the app grid shrinks the workspaces to thumbnails along
    /// the top, as in GNOME), again last.
    ///
    /// `0.13` appends the compositor-to-shell `OverviewPreviews` message
    /// (where each window preview sits, so the shell can draw GNOME's
    /// app icons, captions and close buttons on them), again last.
    ///
    /// `0.14` appends the shell-to-compositor `Unlock` command (the lock
    /// screen's password, verified by the compositor), again last.
    ///
    /// `0.15` appends the shell-to-compositor `SetAccelerators` command
    /// and the compositor-to-shell `AcceleratorActivated` message
    /// (org.gnome.Shell's GrabAccelerators for gnome-settings-daemon's
    /// media keys), again last.
    ///
    /// `0.16` appends the compositor-to-shell `WindowMenu` message and
    /// the shell-to-compositor `WindowAction` command (GNOME's window
    /// menu), again last.
    ///
    /// `0.17` appends the compositor-to-shell `WorkspacePopup` message
    /// (GNOME's workspace switcher popup), again last.
    ///
    /// `0.18` appends `Maximize`, `ToggleTiledLeft` and `ToggleTiledRight`
    /// to `WindowAction` and the shell-to-compositor `SwitchInputSource`
    /// command (GNOME's rebindable window-manager keys), again last.
    ///
    /// `0.19` appends `StepWindow` and `Key` to `SwitcherAction` and the
    /// shell-to-compositor `SetSwitcherThumbnails` command (GNOME's
    /// window thumbnails in the Alt+Tab switcher), again last.
    ///
    /// `0.20` appends the shell-to-compositor `SetSwitcherKeys` command
    /// (GNOME's rebindable switcher keys), again last.
    ///
    /// `0.21` appends `enable_animations` to `InputSettings`. Like earlier
    /// positional struct extensions, the postcard body changes and both
    /// sides ship together from this workspace.
    ///
    /// `0.22` appends shortcut inhibition consent requests and responses.
    ///
    /// `0.23` appends window/cycle switcher actions and pointer output.
    ///
    /// `0.24` appends window-backed icon metadata to WindowInfo.
    /// Postcard positional structs require peers to upgrade together.
    ///
    /// `0.25` appends mouse/touchpad handedness to InputSettings.
    /// Both peers ship together because the postcard body is positional.
    /// `0.26` appends the shell text direction to InputSettings.
    /// Both positional postcard peers ship together.
    /// `0.27` appends hardware Screen Reader preference/status messages.
    /// `0.28` appends typed accelerator grabs and release notifications.
    /// Existing Accelerator positional bodies and message indexes are unchanged.
    /// `0.29` appends NativeOutputInventory without altering OutputInfo.
    /// `0.30` appends MonitorIdentityInventory.
    /// `0.31` appends five session-only brightness journal variants.
    pub const CURRENT: Self = Self {
        major: 0,
        minor: 31,
    };

    /// Build a version explicitly (handy for `Hello` probes in tests).
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Compatibility of a peer version against ours: same major line and a
    /// minor no newer than ours. A newer minor means the peer speaks
    /// messages we may not understand; a different major is always an
    /// error (acceptance criterion A6).
    pub fn is_compatible_with(&self, ours: &ProtocolVersion) -> bool {
        self.major == ours.major && self.minor <= ours.minor
    }
}

/// Version spoken by this crate.
pub const CURRENT_VERSION: ProtocolVersion = ProtocolVersion::CURRENT;

/// Window state owned by the compositor and mirrored to the shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Opaque compositor-minted id.
    pub id: WindowId,
    /// Untrusted client-supplied text, capped at [`MAX_TITLE_LEN`] bytes.
    pub title: String,
    /// Client-supplied application id, if known.
    pub app_id: Option<String>,
    /// Workspace currently containing the window.
    pub workspace: WorkspaceId,
    /// Whether the window currently holds keyboard focus.
    pub focused: bool,
    /// Fresh one-use activation Bearer [REDACTED] for this window, minted with the
    /// snapshot or delta that carries it. The shell presents it back in
    /// `ActivateWindow`; presenting it consumes it. Several live tokens
    /// for one window may coexist (each snapshot/delta mints anew); each
    /// allows exactly one activation and expires after 30 s. Never logged
    /// (redacted `Debug` on [`ActivationToken`]).
    pub activation_token: String,
    /// Compositor-owned PNG cache path or sanitized theme icon name.
    pub icon: Option<String>,
}

/// Workspace state owned by the compositor and mirrored to the shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// Opaque compositor-minted id.
    pub id: WorkspaceId,
    /// Human-readable name, if the compositor assigns one.
    pub name: Option<String>,
    /// Whether this workspace is currently active.
    pub active: bool,
}

/// One compositor-tracked output: name, size, and dock anchor. The
/// inventory crosses to the shell as these records; window migration
/// stays compositor-internal, so no other wire types change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputInfo {
    /// Output name, e.g. `roost-0`.
    pub name: String,
    /// Output width in physical pixels.
    pub width: i32,
    /// Output height in physical pixels.
    pub height: i32,
    /// Whether the dock anchors here.
    pub primary: bool,
}

/// Actual selected native DRM output authority. Missing inventory means no
/// authority, including nested sessions; connector text alone is insufficient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeOutputInfo {
    pub name: String,
    /// Actual char-device rdev from the compositor-owned KMS FD.
    pub drm_device: u64,
    /// Actual DRM connector object ID, checked against kernel sysfs.
    pub connector_id: u32,
    /// Canonical sysfs DRM connector directory under that owned device.
    pub connector_sysfs: String,
    pub connector_device: u64,
    pub connector_inode: u64,
}

/// Optional kernel-observed EDID metadata, never authority or restore admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdidIdentityInfo {
    pub vendor: String,
    pub product: String,
    pub serial: String,
    /// Intrinsic serial quality only; consumers must separately reject duplicates.
    pub meaningful_serial: bool,
    pub sha256: [u8; 32],
    pub blocks: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorIdentityInfo {
    pub owner: NativeOutputInfo,
    pub edid: Option<EdidIdentityInfo>,
}

/// One ordered state mutation between two snapshot revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateOp {
    /// A window appeared (or its full state is restated).
    WindowOpened(WindowInfo),
    /// A window is gone; its id may never be reused this session.
    WindowClosed {
        /// Id of the removed window.
        id: WindowId,
    },
    /// Keyboard focus moved to this window.
    WindowFocused {
        /// Id of the newly focused window.
        id: WindowId,
    },
    /// A window moved to another workspace.
    WindowMoved {
        /// Id of the moved window.
        id: WindowId,
        /// Destination workspace.
        workspace: WorkspaceId,
    },
    /// A workspace was added, removed, renamed, or (de)activated.
    WorkspaceChanged(WorkspaceInfo),
}

/// Shell-to-compositor command payload. Every command carries the shell's
/// request id (`Message::Command.id`); the compositor answers with
/// `Message::CommandResult` bearing the same id. Commands never block
/// compositor input/frame paths (spec R6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandKind {
    /// Focus and raise a window. Requires a compositor-issued activation
    /// token: a raw Wayland serial is not sufficient (spec 001, ADR 0002).
    ActivateWindow {
        /// Window to activate.
        window: WindowId,
        /// Compositor-minted Bearer [REDACTED] proving a recent user interaction.
        token: ActivationToken,
    },
    /// Make a workspace active.
    FocusWorkspace {
        /// Workspace to activate.
        workspace: WorkspaceId,
    },
    /// Toggle the shell overview open or closed.
    ToggleOverview,
    /// Ask a window's client to close (polite close request; the
    /// client unmaps itself and the compositor drops it on reconcile).
    /// No activation token: closing is not focus, and unknown ids are
    /// per-request denials like every other command.
    CloseWindow {
        /// Window to close.
        window: WindowId,
    },
    /// Lock the session immediately (manual lock from the shell).
    /// Idempotent and tokenless like `ToggleOverview`: the shell is a
    /// trusted local peer, and locking hides rather than reveals.
    /// Unlocking is never granted on request: see `Unlock`, which
    /// carries a password the compositor verifies through session auth.
    Lock,
    /// Lock after this much idle time; `0` never locks on idle (#63).
    /// The shell derives it from GNOME's `idle-delay`, `lock-enabled`
    /// and `lock-delay` keys and resends it on every change. Tokenless:
    /// it only changes when the session locks itself, never unlocks.
    SetIdleTimeout {
        /// Idle milliseconds before locking, `0` for never.
        ms: u64,
    },
    /// The overview's search is showing results (or not): while it is,
    /// the compositor hides the workspace card and window previews, as
    /// GNOME does. UI state; it resets whenever the overview closes.
    SetOverviewSearch {
        /// Whether search results are showing.
        active: bool,
    },
    /// GNOME's input settings, as the shell reads them from GSettings
    /// (#60): keymap, key repeat, pointer devices, hot corner. Sent at
    /// start and on every change; the compositor applies them live.
    SetInputSettings(InputSettings),
    /// The overview shows the app grid (or not): while it does, the
    /// compositor draws the workspaces as thumbnails along the top, as
    /// GNOME's app grid state does. UI state; it resets whenever the
    /// overview closes.
    SetOverviewAppGrid {
        /// Whether the app grid is showing.
        active: bool,
    },
    /// The lock screen's password: the compositor verifies it through
    /// session auth (PAM, or greetd's daemon) off its event loop and
    /// unlocks only on success. The result comes back as this command's
    /// `CommandResult` (`Applied` unlocked, `Denied` wrong password).
    Unlock {
        /// The typed password; never logged (`Debug` is redacted).
        password: Secret,
    },
    /// Every key combination grabbed through org.gnome.Shell's
    /// GrabAccelerators, the full set each time it changes. The
    /// compositor keeps matching presses from clients and reports them
    /// with `AcceleratorActivated`.
    SetAccelerators {
        /// The grabs, at most [`MAX_ACCELERATORS`].
        accelerators: Vec<Accelerator>,
    },
    /// One of GNOME's window-menu actions on a window. Unknown windows
    /// are denied, like every per-window command.
    WindowAction {
        /// The window.
        window: WindowId,
        /// What to do.
        action: WindowAction,
    },
    /// Switch to the next (or previous) keyboard input source (GNOME's
    /// `switch-input-source` keys). Tokenless: it only changes the
    /// keymap.
    SwitchInputSource {
        /// The previous source instead of the next.
        backward: bool,
    },
    /// Where the open switcher shows window thumbnails (GNOME's
    /// ThumbnailSwitcher): the compositor draws each window, fitted,
    /// into its frame above everything. Empty when none show. At most
    /// [`MAX_SWITCHER_THUMBNAILS`].
    SetSwitcherThumbnails {
        /// The frames, in order.
        thumbnails: Vec<SwitcherThumbnail>,
    },
    /// The switcher's keys (GNOME's `switch-applications`,
    /// `switch-group` and their backward keys), the full set each time
    /// they change. Once sent they replace the compositor's built-in
    /// Alt/Super+Tab and Above_Tab; an empty list unbinds the switcher.
    /// At most [`MAX_SWITCHER_KEYS`].
    SetSwitcherKeys {
        /// Every bound chord.
        keys: Vec<SwitcherKey>,
    },
    /// Trusted shell response to a compositor-issued consent request.
    ShortcutConsent {
        /// The pending request, never a window id.
        request: u64,
        /// Explicit Allow (or a remembered grant), otherwise Deny.
        allow: bool,
    },
    /// Preference from the authenticated supervised shell. Nested runtimes
    /// ignore it; the hardware compositor owns the actual Orca child.
    SetScreenReader { enabled: bool },
    /// Atomic full accelerator replacement, with supported GNOME grab flags.
    SetAcceleratorGrabs { grabs: Vec<AcceleratorGrab> },
}

/// Screen Reader process lifecycle, not proof of spoken usability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenReaderState {
    Disabled,
    Starting,
    Active,
    Unavailable,
    Conflict,
}

/// Switcher thumbnails held at once.
pub const MAX_SWITCHER_THUMBNAILS: usize = 64;

/// Switcher keys held at once.
pub const MAX_SWITCHER_KEYS: usize = 64;

/// Mutter's `Above_Tab` in [`SwitcherKey::keysym`]: the key above Tab,
/// whatever it types in the active layout.
pub const KEYSYM_ABOVE_TAB: u32 = 0x2000_0000;

/// One switcher chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitcherKey {
    /// The key's base-level xkb keysym (lower case for letters), or
    /// [`KEYSYM_ABOVE_TAB`].
    pub keysym: u32,
    /// Exact modifiers: a combination of the `MOD_*` bits. Released,
    /// all but Shift commit the switcher.
    pub mods: u32,
    pub kind: SwitcherKeyKind,
}

/// What a switcher chord does (GNOME's keybinding names).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwitcherKeyKind {
    /// `switch-applications`: the next app.
    Applications,
    /// `switch-applications-backward`.
    ApplicationsBackward,
    /// `switch-group`: the selected app's next window.
    Group,
    /// `switch-group-backward`.
    GroupBackward,
    /// WindowSwitcherPopup: one item for each window.
    Windows,
    WindowsBackward,
    /// Immediate cycling, without showing a popup.
    CycleWindows,
    CycleWindowsBackward,
    CycleGroup,
    CycleGroupBackward,
}

/// One switcher thumbnail frame, in global logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitcherThumbnail {
    /// The window drawn in it.
    pub window: WindowId,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// GNOME's window-menu actions (windowMenu.js) the compositor carries out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowAction {
    /// Hide (minimize).
    Minimize,
    /// Maximize, or Restore when maximized.
    ToggleMaximize,
    /// Move with the pointer until a click.
    Move,
    /// Resize from the bottom-right corner with the pointer until a click.
    Resize,
    /// Move to the workspace on the left.
    MoveToWorkspaceLeft,
    /// Move to the workspace on the right.
    MoveToWorkspaceRight,
    /// Always on Top, on or off.
    ToggleAbove,
    /// Always on Visible Workspace (on every workspace), on or off.
    ToggleSticky,
    /// Open the window menu at the window's corner (Alt+Space).
    ShowMenu,
    /// Leave maximized (Alt+F5).
    Unmaximize,
    /// Move to this workspace (GNOME's move-to-workspace keys).
    MoveToWorkspace {
        /// The workspace.
        workspace: u32,
    },
    /// Maximize (GNOME's `maximize` key, Super+Up).
    Maximize,
    /// Tile the left half, or untile (Mutter's `toggle-tiled-left`).
    ToggleTiledLeft,
    /// Tile the right half, or untile (`toggle-tiled-right`).
    ToggleTiledRight,
}

/// Grabbed accelerators held at once.
pub const MAX_ACCELERATORS: usize = 512;

/// Modifier bits in [`Accelerator::mods`] (X11's masks).
pub const MOD_SHIFT: u32 = 1;
pub const MOD_CTRL: u32 = 4;
pub const MOD_ALT: u32 = 8;
pub const MOD_LOGO: u32 = 64;

/// GNOME Shell's action modes (Shell.ActionMode) in [`Accelerator::modes`]:
/// where a grab applies.
pub const MODE_NORMAL: u32 = 1;
pub const MODE_OVERVIEW: u32 = 2;
pub const MODE_LOCK_SCREEN: u32 = 4;
pub const MODE_UNLOCK_SCREEN: u32 = 8;
/// GNOME Shell 51 ActionMode.POPUP, used by its screenshot grab.
pub const MODE_POPUP: u32 = 1 << 7;

/// One grabbed key combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accelerator {
    /// The shell's action id, echoed in `AcceleratorActivated`.
    pub action: u32,
    /// The xkb keysym (lower case for letters).
    pub keysym: u32,
    /// Exact modifiers: a combination of the `MOD_*` bits.
    pub mods: u32,
    /// Action modes it applies in: a combination of the `MODE_*` bits.
    pub modes: u32,
}

/// GNOME MetaKeyBindingFlags supported for external accelerator grabs.
pub const GRAB_IGNORE_AUTOREPEAT: u32 = 1 << 4;
pub const GRAB_TRIGGER_RELEASE: u32 = 1 << 7;
pub const SUPPORTED_GRAB_FLAGS: u32 = GRAB_IGNORE_AUTOREPEAT | GRAB_TRIGGER_RELEASE;

/// Append-only replacement type; the legacy Accelerator wire body is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceleratorGrab {
    pub accelerator: Accelerator,
    pub flags: u32,
}

/// A secret on the wire (the lock screen's password). `Debug` never
/// shows it, so a logged message cannot leak it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Outcome of one shell command, matched by request id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandStatus {
    /// The compositor applied the command.
    Applied,
    /// The compositor refused the command; `reason` is diagnostics only.
    Denied {
        /// Human-readable denial reason (never sensitive content).
        reason: String,
    },
}

/// Machine-readable control-plane error category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    /// A frame failed length, encoding, or validation checks.
    MalformedFrame,
    /// Peer speaks an incompatible major version.
    IncompatibleVersion,
    /// A frame exceeded [`MAX_FRAME_BYTES`].
    OversizeFrame,
    /// The activation token was unknown, expired, or already used.
    StaleToken,
    /// `Changes.from_revision` does not match the shell's revision; the
    /// shell must discard state and await a fresh `Snapshot`.
    RevisionGap,
    /// The command id/kind was not understood.
    UnknownCommand,
}

/// Top-level protocol message. This enum is the postcard body of one frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    /// Version handshake; the first message in both directions.
    Hello {
        /// Peer's protocol version.
        version: ProtocolVersion,
    },
    /// Complete authoritative state. Always sent on (re)connect and after
    /// any revision gap; the shell renders from this, never from
    /// pre-crash assumptions.
    Snapshot {
        /// Monotonic revision of this state.
        revision: u64,
        /// All mapped windows.
        windows: Vec<WindowInfo>,
        /// All workspaces.
        workspaces: Vec<WorkspaceInfo>,
        /// Whether the session is locked. While set, `windows` carries
        /// no content (empty): the lock screen shows no titles, and a
        /// restartable shell must not retain any. The flag is the only
        /// lock state on the wire — never credentials.
        locked: bool,
    },
    /// Ordered incremental updates from `from_revision` to `to_revision`.
    /// A gap against the shell's revision forces a resnapshot.
    Changes {
        /// Revision the shell must currently hold.
        from_revision: u64,
        /// Revision the shell holds after applying `ops` in order.
        to_revision: u64,
        /// Mutations to apply in order.
        ops: Vec<StateOp>,
    },
    /// Shell request; answered by `CommandResult` with the same `id`.
    Command {
        /// Shell-chosen request id, echoed in the result.
        id: u64,
        /// Command payload.
        kind: CommandKind,
    },
    /// Compositor answer to one `Command`, matched by `id`.
    CommandResult {
        /// Echo of `Command.id`.
        id: u64,
        /// Outcome.
        status: CommandStatus,
    },
    /// Typed observable error (ADR 0002); the offending frame is dropped.
    Error {
        /// Category.
        kind: ErrorKind,
        /// Diagnostics; never sensitive application content.
        message: String,
    },
    /// Compositor-to-shell overview intent (002 R1). The compositor
    /// detects Super, Activities-strip, and hot-corner triggers — the
    /// shell never sees global input — and broadcasts the resulting
    /// open state; the shell renders from its model and answers with
    /// intent commands. Carries no revision: it is UI state, not model
    /// truth, and never replaces snapshot resync.
    Overview {
        /// Whether the overview should be open.
        open: bool,
    },
    /// Compositor-to-shell Alt-Tab switcher drive (002 workspaces).
    /// The compositor owns global key state (Alt held, Tab taps,
    /// Alt release, Escape) and the shell owns MRU order and
    /// rendering: each Tab tap broadcasts one `Step`, Alt release
    /// broadcasts `Commit`, Escape broadcasts `Cancel`. The shell
    /// answers a commit with `ActivateWindow` for its selected id.
    /// Carries no revision: UI state, like `Overview`.
    Switcher {
        /// What the switcher should do.
        action: SwitcherAction,
    },
    /// Compositor-to-shell output inventory (multi-monitor). The
    /// compositor broadcasts the full list whenever it changes and
    /// right after each handshake snapshot; the shell reconciles its
    /// per-output surfaces against it. Carries no revision: structural
    /// state, like `Overview` — and it sits last so earlier variant
    /// positions are untouched.
    Outputs {
        /// Every tracked output, primary first.
        outputs: Vec<OutputInfo>,
    },
    /// Compositor-to-shell session environment: variables apps launched
    /// from the shell must inherit, such as `DISPLAY` once XWayland's
    /// window manager is up (#59). The full set each time; sent after
    /// every handshake and whenever it changes. Sits last so earlier
    /// variant positions are untouched.
    Environment {
        /// `(name, value)` pairs, sorted by name.
        vars: Vec<(String, String)>,
    },
    /// Compositor-to-shell overview previews: where each window preview
    /// on the active workspace sits and which one the pointer is over,
    /// so the shell draws GNOME's preview chrome (app icon always;
    /// caption and close button on hover). Empty when the overview is
    /// closed. Sent whenever it changes. Sits last.
    OverviewPreviews {
        /// Previews in drawing order, bottom first.
        previews: Vec<PreviewInfo>,
        /// The preview under the pointer, if any.
        hovered: Option<u64>,
    },
    /// Compositor-to-shell: a grabbed accelerator was pressed (the
    /// press and its release never reached a client). Sits last.
    AcceleratorActivated {
        /// The grab's action id.
        action: u32,
        /// The key event's time, in milliseconds.
        time: u32,
        /// The action mode it fired in (one `MODE_*` bit).
        mode: u32,
    },
    /// Compositor-to-shell: a client asked for its window menu (a header
    /// bar right click); the shell draws GNOME's menu at `x`, `y`
    /// (logical pixels of the output). Sits last.
    WindowMenu {
        /// The window.
        window: WindowId,
        /// Where, in logical output pixels.
        x: i32,
        y: i32,
        /// Whether it is maximized (Restore rather than Maximize).
        maximized: bool,
        /// Whether it is kept above other windows (Always on Top).
        above: bool,
        /// Whether it shows on every workspace (Always on Visible
        /// Workspace).
        sticky: bool,
        /// Whether there is a workspace to its left, and to its right.
        workspace_left: bool,
        workspace_right: bool,
    },
    /// Compositor-to-shell: a workspace key switched workspaces outside
    /// the overview; the shell shows GNOME's switcher popup. Sits last.
    WorkspacePopup {
        /// The active workspace's position.
        index: u32,
        /// How many workspaces there are (GNOME's dynamic count).
        count: u32,
    },
    /// Show consent for this inhibitor; `None` dismisses a stale dialog.
    ShortcutConsent {
        /// Request id and stable application id (empty for unknown apps).
        request: Option<(u64, String)>,
    },
    /// The connector under the pointer, used by per-monitor brightness keys.
    PointerOutput {
        name: Option<String>,
    },
    /// Actual compositor-owned Screen Reader child status.
    ScreenReader {
        state: ScreenReaderState,
    },
    /// Release of an originally admitted accelerator press.
    AcceleratorDeactivated {
        action: u32,
        time: u32,
        mode: u32,
    },
    /// Full actual native authority; empty explicitly revokes prior authority.
    /// Append-only, sent only to peers advertising minor >=29.
    NativeOutputInventory {
        outputs: Vec<NativeOutputInfo>,
    },
    /// Since0.30: approved optional EDID metadata, unchanged original owner.
    MonitorIdentityInventory {
        outputs: Vec<MonitorIdentityInfo>,
    },
    /// Since0.31: original kernel snapshot; admission never overwrites a
    /// surviving journal's committed user intent with dimmed physical levels.
    BrightnessJournalInit {
        request: u64,
        readings: Vec<BrightnessReading>,
        idle: f64,
    },
    /// Grant must arrive AFTER pending is recorded and BEFORE any helper call.
    BrightnessJournalBegin {
        request: u64,
        source: BrightnessSource,
        targets: Vec<BrightnessTarget>,
    },
    /// Actual original helper callback plus independent original readback facts.
    BrightnessJournalComplete {
        request: u64,
        grant: BrightnessGrant,
        observations: Vec<BrightnessObservation>,
    },
    BrightnessJournalState {
        state: BrightnessJournalSnapshot,
    },
    BrightnessJournalReply {
        request: u64,
        grant: Option<BrightnessGrant>,
        error: Option<BrightnessJournalError>,
        state: Option<BrightnessJournalSnapshot>,
    },
}

/// GNOME's dynamic workspace count: one empty workspace always follows
/// the last occupied one, and the active one counts too.
pub fn dynamic_workspace_count(max_occupied: Option<u32>, active: u32) -> u32 {
    max_occupied.map_or(1, |m| m + 2).max(active + 1)
}

/// One window preview in the overview, in output logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewInfo {
    /// The window's model id.
    pub window: u64,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// GNOME's input settings (`org.gnome.desktop.input-sources` and
/// `org.gnome.desktop.peripherals.*`), flattened for the compositor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputSettings {
    /// xkb layouts, comma separated, in input-source order (`us,de`).
    pub xkb_layout: String,
    /// xkb variants, one per layout (`,nodeadkeys`).
    pub xkb_variant: String,
    /// xkb options, comma separated (`compose:ralt`).
    pub xkb_options: String,
    /// Key repeat on, its delay and interval in milliseconds.
    pub repeat: bool,
    pub repeat_delay_ms: u32,
    pub repeat_interval_ms: u32,
    /// Touchpad.
    pub tap_to_click: bool,
    pub touchpad_natural_scroll: bool,
    /// Pointer speed in thousandths, -1000..=1000 (GNOME's -1..1).
    pub touchpad_speed_milli: i32,
    pub disable_while_typing: bool,
    /// Mouse.
    pub mouse_natural_scroll: bool,
    pub mouse_speed_milli: i32,
    /// `org.gnome.desktop.interface enable-hot-corners`.
    pub hot_corners: bool,
    /// `org.gnome.desktop.interface enable-animations` (default enabled).
    #[serde(default = "animations_default")]
    pub enable_animations: bool,
    /// GNOME mouse primary button and resolved touchpad orientation.
    #[serde(default)]
    pub mouse_left_handed: bool,
    #[serde(default)]
    pub touchpad_left_handed: bool,
    /// GTK shell default text direction; corners and Activities follow it.
    #[serde(default)]
    pub right_to_left: bool,
}

fn animations_default() -> bool {
    true
}

impl Default for InputSettings {
    /// GNOME 51's defaults.
    fn default() -> Self {
        Self {
            xkb_layout: "us".into(),
            xkb_variant: String::new(),
            xkb_options: String::new(),
            repeat: true,
            repeat_delay_ms: 500,
            repeat_interval_ms: 30,
            tap_to_click: false,
            touchpad_natural_scroll: true,
            touchpad_speed_milli: 0,
            disable_while_typing: true,
            mouse_natural_scroll: false,
            mouse_speed_milli: 0,
            hot_corners: true,
            enable_animations: true,
            mouse_left_handed: false,
            touchpad_left_handed: false,
            right_to_left: false,
        }
    }
}

/// One Alt-Tab switcher drive event (compositor to shell).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwitcherAction {
    /// Advance the switcher selection (`forward`: Tab vs Shift+Tab).
    /// Opens the switcher when closed.
    Step {
        /// True for Tab, false for Shift+Tab.
        forward: bool,
    },
    /// Activate the current selection; closes the switcher.
    Commit,
    /// Close the switcher without activating.
    Cancel,
    /// GNOME's `switch-group` (Alt+Above_Tab): step through the selected
    /// app's windows, showing their thumbnails. Opens the switcher when
    /// closed.
    StepWindow {
        /// True for Above_Tab, false with Shift.
        forward: bool,
    },
    /// Another key pressed while the switcher is open (its keysym): the
    /// switcher holds the keyboard, as GNOME's does (arrows, Q, W, F4).
    Key {
        /// The xkb keysym.
        keysym: u32,
    },
    /// Window switcher: every window is a separate entry.
    StepAllWindows { forward: bool },
    /// Cycle immediately without a popup, optionally within the focused app.
    Cycle { forward: bool, group: bool },
}

/// Unified error type for the control protocol, spoken by both compositor
/// and shell-host endpoints.
///
/// This type spans wire-level failures (Io, Decode, Unexpected), semantic
/// protocol violations (Remote, WouldBlock), and application-level failures
/// (NoActiveWindow). Both peers use this type so errors are consistently
/// named across the channel boundary and callers can handle either side's
/// failures uniformly.
///
/// Nonblocking contract (spec R6): `WouldBlock` surfaces immediately when
/// a socket is not ready; on write backpressure, the connection should be
/// dropped rather than retried (a partial frame may remain buffered).
/// Not `Clone`/`PartialEq`: the `Io` variant carries [`std::io::Error`],
/// which implements neither. Match on `Io(e) if e.kind() == ...` instead of
/// comparing errors for equality.
#[derive(Debug)]
pub enum ControlError {
    /// Underlying socket I/O failure.
    Io(std::io::Error),
    /// Frame failed schema validation (oversize, malformed, stale major,
    /// over-long title). The offending frame is dropped.
    Decode(DecodeError),
    /// Nonblocking socket not ready (spec R6). On a write that returns
    /// this, the connection holds a partial frame and must be dropped.
    WouldBlock,
    /// Peer closed the connection or sent a message the current step
    /// cannot use (unexpected or out-of-order).
    Unexpected(String),
    /// The peer (compositor when called from shell-host) answered with a
    /// typed [`Message::Error`] where a response was required.
    Remote {
        /// Machine-readable category.
        kind: ErrorKind,
        /// Compositor diagnostics (never sensitive content).
        message: String,
    },
    /// Application-level constraint violated: e.g., a shell-host command
    /// like `send_activation` needs a selected window but none exists.
    AppConstraint(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "control socket I/O: {e}"),
            Self::Decode(e) => write!(f, "control decode: {e}"),
            Self::WouldBlock => write!(f, "control socket not ready"),
            Self::Unexpected(e) => write!(f, "unexpected message or state: {e}"),
            Self::Remote { kind, message } => {
                write!(f, "remote error ({kind:?}): {message}")
            }
            Self::AppConstraint(e) => write!(f, "constraint: {e}"),
        }
    }
}

impl std::error::Error for ControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Decode(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ControlError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::WouldBlock {
            Self::WouldBlock
        } else {
            Self::Io(e)
        }
    }
}

impl From<DecodeError> for ControlError {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}

/// Compositor-minted activation Bearer [REDACTED] authorizing one privileged window action.
///
/// Opaque to the shell: minting, expiry (30 s), one-use removal, seat
/// binding, and `app_id` matching are compositor policy (ADR 0002), not
/// protocol behavior. `Debug` is redacted so tokens never land in logs
/// (spec R7).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationToken(pub String);

impl ActivationToken {
    /// Wrap a compositor-minted token string.
    pub fn new(token: String) -> Self {
        Self(token)
    }
}

impl std::fmt::Debug for ActivationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActivationToken([redacted])")
    }
}

/// Compositor-side bookkeeping attached to a minted token (ADR 0002).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenMeta {
    /// Why the token was minted (e.g. `"activate-window"`).
    pub purpose: String,
    /// Expected client `app_id`; activation is denied on mismatch.
    pub app_id: Option<String>,
    /// Mint time in milliseconds since the Unix epoch; expiry is 30 s.
    pub issued_at_ms: u64,
}

/// Framing/decoding failure. Every variant is observable and typed so both
/// sides can log, count, and drop the offending frame (ADR 0002).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// A typed collection exceeds its admission bound.
    CollectionTooLong { len: usize, max: usize },
    /// Fewer than 4 prefix bytes, or a body shorter than the prefix claims.
    Truncated {
        /// Bytes required.
        expected: usize,
        /// Bytes present.
        actual: usize,
    },
    /// Declared body length exceeds [`MAX_FRAME_BYTES`]. Checked before any
    /// postcard decoding, so the declared length alone triggers it.
    Oversize {
        /// Declared body length.
        len: usize,
        /// The cap ([`MAX_FRAME_BYTES`]).
        max: usize,
    },
    /// postcard decode failure or trailing bytes after the declared body.
    Malformed(postcard::Error),
    /// `Hello` major differs from [`CURRENT_VERSION`].major.
    IncompatibleVersion {
        /// Version the peer offered.
        got: ProtocolVersion,
        /// Version we speak.
        current: ProtocolVersion,
    },
    /// A window title exceeded [`MAX_TITLE_LEN`] bytes.
    TitleTooLong {
        /// Title length in bytes.
        len: usize,
        /// The cap ([`MAX_TITLE_LEN`]).
        max: usize,
    },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CollectionTooLong { len, max } => {
                write!(f, "collection of {len} records exceeds {max}")
            }
            Self::Truncated { expected, actual } => {
                write!(f, "truncated frame: need {expected} bytes, have {actual}")
            }
            Self::Oversize { len, max } => {
                write!(f, "frame of {len} bytes exceeds cap of {max} bytes")
            }
            Self::Malformed(e) => write!(f, "malformed frame body: {e}"),
            Self::IncompatibleVersion { got, current } => write!(
                f,
                "incompatible peer major {}.{} (ours {}.{})",
                got.major, got.minor, current.major, current.minor
            ),
            Self::TitleTooLong { len, max } => {
                write!(f, "title of {len} bytes exceeds cap of {max} bytes")
            }
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Malformed(e) => Some(e),
            _ => None,
        }
    }
}

impl From<postcard::Error> for DecodeError {
    fn from(e: postcard::Error) -> Self {
        Self::Malformed(e)
    }
}

/// Encode one message as a length-prefixed frame: u32-LE body length
/// followed by the postcard body.
///
/// Panics only if the message cannot be encoded or exceeds
/// [`MAX_FRAME_BYTES`]; both indicate a local programming error, since the
/// cap bounds every message this crate can legally emit.
pub fn encode_frame(msg: &Message) -> Vec<u8> {
    // Grow-on-demand so small control messages avoid the 1 MiB buffer.
    // postcard::to_slice reports SerializeBufferFull when the buffer is
    // too small; that is the only recoverable encode error here.
    let mut cap = 1024usize;
    loop {
        let mut buf = vec![0u8; cap];
        match postcard::to_slice(msg, &mut buf) {
            Ok(used) => {
                let body_len = used.len();
                let mut frame = Vec::with_capacity(4 + body_len);
                frame.extend_from_slice(&(body_len as u32).to_le_bytes());
                frame.extend_from_slice(used);
                return frame;
            }
            Err(postcard::Error::SerializeBufferFull) => {
                cap *= 4;
                assert!(
                    cap <= MAX_FRAME_BYTES,
                    "encoded message exceeds {MAX_FRAME_BYTES}-byte frame cap"
                );
            }
            Err(e) => panic!("shell control encode failed: {e}"),
        }
    }
}

/// Decode one length-prefixed frame into a message.
///
/// Checks run in wire order: prefix present, declared length within
/// [`MAX_FRAME_BYTES`] (before any postcard work), body complete, body
/// well-formed with no trailing bytes, titles within [`MAX_TITLE_LEN`],
/// and `Hello` major matching [`CURRENT_VERSION`].major.
pub fn decode_frame(bytes: &[u8]) -> Result<Message, DecodeError> {
    if bytes.len() < 4 {
        return Err(DecodeError::Truncated {
            expected: 4,
            actual: bytes.len(),
        });
    }
    let mut prefix = [0u8; 4];
    prefix.copy_from_slice(&bytes[..4]);
    let len = u32::from_le_bytes(prefix) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(DecodeError::Oversize {
            len,
            max: MAX_FRAME_BYTES,
        });
    }
    let body = &bytes[4..];
    if body.len() < len {
        return Err(DecodeError::Truncated {
            expected: len,
            actual: body.len(),
        });
    }
    if body.len() > len {
        return Err(DecodeError::Malformed(
            postcard::Error::DeserializeBadEncoding,
        ));
    }
    let msg: Message = postcard::from_bytes(&body[..len])?;
    validate_titles(&msg)?;
    if let Message::Hello { version } = &msg {
        if version.major != CURRENT_VERSION.major {
            return Err(DecodeError::IncompatibleVersion {
                got: *version,
                current: CURRENT_VERSION,
            });
        }
    }
    Ok(msg)
}

/// Reject messages carrying titles over [`MAX_TITLE_LEN`] bytes.
fn check_title(title: &str) -> Result<(), DecodeError> {
    if title.len() > MAX_TITLE_LEN {
        return Err(DecodeError::TitleTooLong {
            len: title.len(),
            max: MAX_TITLE_LEN,
        });
    }
    Ok(())
}

fn check_brightness_count(len: usize) -> Result<(), DecodeError> {
    if len > 64 {
        Err(DecodeError::CollectionTooLong { len, max: 64 })
    } else {
        Ok(())
    }
}
fn validate_brightness_binding(binding: &BrightnessBinding) -> Result<(), DecodeError> {
    check_title(&binding.output.name)?;
    check_title(&binding.output.connector_sysfs)?;
    check_title(&binding.backlight)
}
fn validate_brightness_readings(readings: &[BrightnessReading]) -> Result<(), DecodeError> {
    check_brightness_count(readings.len())?;
    for row in readings {
        validate_brightness_binding(&row.binding)?;
    }
    Ok(())
}
fn validate_brightness_targets(targets: &[BrightnessTarget]) -> Result<(), DecodeError> {
    check_brightness_count(targets.len())?;
    for row in targets {
        validate_brightness_binding(&row.binding)?;
    }
    Ok(())
}
fn validate_brightness_state(state: &BrightnessJournalSnapshot) -> Result<(), DecodeError> {
    if let Some(provider) = &state.provider {
        check_title(&provider.unique)?;
        check_title(&provider.start)?;
    }
    validate_brightness_readings(&state.readings)?;
    validate_brightness_readings(&state.retired_readings)?;
    if let Some(pending) = &state.pending {
        validate_brightness_targets(&pending.targets)?;
    }
    Ok(())
}

fn validate_titles(msg: &Message) -> Result<(), DecodeError> {
    fn check_window(w: &WindowInfo) -> Result<(), DecodeError> {
        check_title(&w.title)
    }

    match msg {
        Message::Snapshot { windows, .. } => {
            for w in windows {
                check_window(w)?;
            }
        }
        Message::Changes { ops, .. } => {
            for op in ops {
                if let StateOp::WindowOpened(w) = op {
                    check_window(w)?;
                }
            }
        }
        Message::Command {
            kind: CommandKind::SetInputSettings(settings),
            ..
        } => {
            check_title(&settings.xkb_layout)?;
            check_title(&settings.xkb_variant)?;
            check_title(&settings.xkb_options)?;
        }
        Message::Command {
            kind: CommandKind::SetSwitcherThumbnails { thumbnails },
            ..
        } if thumbnails.len() > MAX_SWITCHER_THUMBNAILS => {
            return Err(DecodeError::Malformed(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        Message::Command {
            kind: CommandKind::SetSwitcherKeys { keys },
            ..
        } if keys.len() > MAX_SWITCHER_KEYS => {
            return Err(DecodeError::Malformed(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        Message::Command {
            kind: CommandKind::SetAccelerators { accelerators },
            ..
        } if accelerators.len() > MAX_ACCELERATORS => {
            return Err(DecodeError::Malformed(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        Message::Command {
            kind: CommandKind::SetAcceleratorGrabs { grabs },
            ..
        } if grabs.len() > MAX_ACCELERATORS
            || grabs.iter().any(|g| g.flags & !SUPPORTED_GRAB_FLAGS != 0) =>
        {
            return Err(DecodeError::Malformed(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        Message::Hello { .. }
        | Message::AcceleratorDeactivated { .. }
        | Message::AcceleratorActivated { .. }
        | Message::WindowMenu { .. }
        | Message::WorkspacePopup { .. }
        | Message::PointerOutput { .. }
        | Message::ScreenReader { .. }
        | Message::ShortcutConsent { .. }
        | Message::Command { .. }
        | Message::CommandResult { .. }
        | Message::Error { .. }
        | Message::Overview { .. }
        | Message::Switcher { .. }
        | Message::Outputs { .. }
        | Message::OverviewPreviews { .. } => {}
        // Names and values share the title bound: short by nature.
        Message::MonitorIdentityInventory { outputs } => {
            if outputs.len() > 64 {
                return Err(DecodeError::CollectionTooLong {
                    len: outputs.len(),
                    max: 64,
                });
            }
            for row in outputs {
                check_title(&row.owner.name)?;
                check_title(&row.owner.connector_sysfs)?;
                if let Some(edid) = &row.edid {
                    if edid.vendor.len() != 3
                        || !edid.vendor.bytes().all(|v| v.is_ascii_uppercase())
                        || edid.product.is_empty()
                        || edid.product.len() > 13
                        || edid.serial.is_empty()
                        || edid.serial.len() > 13
                        || !(1..=256).contains(&edid.blocks)
                        || !edid
                            .product
                            .bytes()
                            .chain(edid.serial.bytes())
                            .all(|v| (32..=126).contains(&v))
                    {
                        return Err(DecodeError::Malformed(
                            postcard::Error::DeserializeBadEncoding,
                        ));
                    }
                }
            }
        }
        Message::BrightnessJournalInit { readings, .. } => validate_brightness_readings(readings)?,
        Message::BrightnessJournalBegin {
            source, targets, ..
        } => {
            if let BrightnessSource::Power { sender, .. } = source {
                check_title(sender)?;
            }
            validate_brightness_targets(targets)?;
        }
        Message::BrightnessJournalComplete { observations, .. } => {
            check_brightness_count(observations.len())?;
            for row in observations {
                validate_brightness_binding(&row.binding)?;
            }
        }
        Message::BrightnessJournalState { state } => validate_brightness_state(state)?,
        Message::BrightnessJournalReply { state, .. } => {
            if let Some(state) = state {
                validate_brightness_state(state)?;
            }
        }
        Message::NativeOutputInventory { outputs } => {
            if outputs.len() > 64 {
                return Err(DecodeError::CollectionTooLong {
                    len: outputs.len(),
                    max: 64,
                });
            }
            for output in outputs {
                check_title(&output.name)?;
                check_title(&output.connector_sysfs)?;
            }
        }
        Message::MonitorIdentityInventory { outputs } => {
            if outputs.len() > 64 {
                return Err(DecodeError::CollectionTooLong {
                    len: outputs.len(),
                    max: 64,
                });
            }
            for output in outputs {
                check_title(&output.owner.name)?;
                check_title(&output.owner.connector_sysfs)?;
                if let Some(edid) = &output.edid {
                    if edid.vendor.len() != 3
                        || !edid.vendor.bytes().all(|v| v.is_ascii_uppercase())
                        || edid.product.is_empty()
                        || edid.product.len() > 13
                        || edid.serial.is_empty()
                        || edid.serial.len() > 13
                        || !(1..=256).contains(&edid.blocks)
                        || !edid
                            .product
                            .bytes()
                            .chain(edid.serial.bytes())
                            .all(|v| (32..=126).contains(&v))
                    {
                        return Err(DecodeError::Malformed(
                            postcard::Error::DeserializeBadEncoding,
                        ));
                    }
                }
            }
        }
        Message::Environment { vars } => {
            for (name, value) in vars {
                check_title(name)?;
                check_title(value)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_window(id: WindowId) -> WindowInfo {
        WindowInfo {
            icon: None,
            id,
            title: format!("Terminal {id}"),
            app_id: Some("org.example.Terminal".to_owned()),
            workspace: 1,
            focused: id == 7,
            activation_token: format!("token-{id}"),
        }
    }

    fn sample_workspace(id: WorkspaceId) -> WorkspaceInfo {
        WorkspaceInfo {
            id,
            name: Some(format!("ws{id}")),
            active: id == 1,
        }
    }

    #[test]
    fn native_inventory_is_appended_and_bounds_authority_collection() {
        let legacy = Message::Outputs {
            outputs: vec![OutputInfo {
                name: "n".into(),
                width: 1,
                height: 1,
                primary: true,
            }],
        };
        // Existing Outputs index8 and unchanged positional OutputInfo body.
        assert_eq!(&encode_frame(&legacy)[4..], &[8, 1, 1, b'n', 2, 2, 1]);
        let owner = NativeOutputInfo {
            name: "eDP-1".into(),
            drm_device: 226,
            connector_id: 39,
            connector_sysfs: "/sys/owned/connector".into(),
            connector_device: 1,
            connector_inode: 2,
        };
        roundtrip(&Message::NativeOutputInventory {
            outputs: vec![owner.clone()],
        });
        assert!(matches!(
            decode_frame(&encode_frame(&Message::NativeOutputInventory {
                outputs: vec![owner; 65]
            })),
            Err(DecodeError::CollectionTooLong { len: 65, max: 64 })
        ));
    }

    #[test]
    fn optional_monitor_identity_is_bounded_without_changing_native_owner_body() {
        let owner = NativeOutputInfo {
            name: "n".into(),
            drm_device: 1,
            connector_id: 2,
            connector_sysfs: "p".into(),
            connector_device: 3,
            connector_inode: 4,
        };
        // Skip the independently integrated append-only variant ordinal only;
        // every positional0.29 inventory/owner body byte stays identical.
        let native = encode_frame(&Message::NativeOutputInventory {
            outputs: vec![owner.clone()],
        });
        assert_eq!(&native[5..], &[1, 1, b'n', 1, 2, 1, b'p', 3, 4]);
        let mut output = MonitorIdentityInfo {
            owner,
            edid: Some(EdidIdentityInfo {
                vendor: "DEL".into(),
                product: "Panel".into(),
                serial: "0x00000000".into(),
                meaningful_serial: false,
                sha256: [1; 32],
                blocks: 1,
            }),
        };
        roundtrip(&Message::MonitorIdentityInventory {
            outputs: vec![output.clone()],
        });
        output.edid = None;
        roundtrip(&Message::MonitorIdentityInventory {
            outputs: vec![output.clone()],
        });
        assert!(matches!(
            decode_frame(&encode_frame(&Message::MonitorIdentityInventory {
                outputs: vec![output.clone(); 65]
            })),
            Err(DecodeError::CollectionTooLong { len: 65, max: 64 })
        ));
        output.edid = Some(EdidIdentityInfo {
            vendor: "DEL".into(),
            product: "x".repeat(14),
            serial: "S".into(),
            meaningful_serial: true,
            sha256: [2; 32],
            blocks: 1,
        });
        assert!(
            decode_frame(&encode_frame(&Message::MonitorIdentityInventory {
                outputs: vec![output.clone()]
            }))
            .is_err()
        );
        output.edid.as_mut().unwrap().product = "Panel".into();
        output.edid.as_mut().unwrap().serial = "bad\nserial".into();
        assert!(
            decode_frame(&encode_frame(&Message::MonitorIdentityInventory {
                outputs: vec![output.clone()]
            }))
            .is_err()
        );
        output.edid.as_mut().unwrap().serial = "S".into();
        output.edid.as_mut().unwrap().blocks = 257;
        assert!(
            decode_frame(&encode_frame(&Message::MonitorIdentityInventory {
                outputs: vec![output]
            }))
            .is_err()
        );
    }

    fn sample_token() -> ActivationToken {
        ActivationToken::new("opaque-token-123".to_owned())
    }

    fn roundtrip(msg: &Message) {
        let frame = encode_frame(msg);
        let back = decode_frame(&frame).expect("roundtrip must decode");
        assert_eq!(&back, msg);
    }

    #[test]
    fn secrets_never_reach_debug_output() {
        let kind = CommandKind::Unlock {
            password: Secret("hunter2".into()),
        };
        let shown = format!("{kind:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn current_version_is_0_31() {
        assert_eq!(CURRENT_VERSION, ProtocolVersion::new(0, 31));
    }

    #[test]
    fn screen_reader_lifecycle_survives_control_wire() {
        for enabled in [false, true] {
            roundtrip(&Message::Command {
                id: 73,
                kind: CommandKind::SetScreenReader { enabled },
            });
        }
        for state in [
            ScreenReaderState::Disabled,
            ScreenReaderState::Starting,
            ScreenReaderState::Active,
            ScreenReaderState::Unavailable,
            ScreenReaderState::Conflict,
        ] {
            roundtrip(&Message::ScreenReader { state });
        }
    }

    #[test]
    fn shell_text_direction_survives_the_positional_control_wire() {
        for right_to_left in [false, true] {
            roundtrip(&Message::Command {
                id: 1,
                kind: CommandKind::SetInputSettings(InputSettings {
                    right_to_left,
                    ..InputSettings::default()
                }),
            });
        }
    }

    #[test]
    fn input_handedness_survives_the_positional_control_wire() {
        for mouse in [false, true] {
            for touchpad in [false, true] {
                roundtrip(&Message::Command {
                    id: 1,
                    kind: CommandKind::SetInputSettings(InputSettings {
                        mouse_left_handed: mouse,
                        touchpad_left_handed: touchpad,
                        ..InputSettings::default()
                    }),
                });
            }
        }
    }

    #[test]
    fn version_compat_same_major_minor_not_newer() {
        let ours = ProtocolVersion::CURRENT;
        assert!(ours.is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 0).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 1).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 2).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 3).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 4).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 5).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 6).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 7).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 8).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 9).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 10).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 11).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 12).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 13).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 14).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 15).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 16).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 17).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 18).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 19).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 20).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 21).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 22).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 23).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 24).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 25).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 26).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 27).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 28).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 29).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 30).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 31).is_compatible_with(&ours));
        assert!(!ProtocolVersion::new(0, 32).is_compatible_with(&ours));
        assert!(!ProtocolVersion::new(1, 4).is_compatible_with(&ours));
        assert!(!ProtocolVersion::new(1, 0).is_compatible_with(&ours));
    }

    #[test]
    fn dynamic_workspaces_end_with_one_empty() {
        assert_eq!(dynamic_workspace_count(None, 0), 1);
        assert_eq!(dynamic_workspace_count(Some(0), 0), 2);
        assert_eq!(dynamic_workspace_count(Some(0), 1), 2);
        assert_eq!(dynamic_workspace_count(Some(2), 0), 4);
    }

    #[test]
    fn roundtrip_environment() {
        roundtrip(&Message::Environment {
            vars: vec![("DISPLAY".to_owned(), ":0".to_owned())],
        });
        roundtrip(&Message::Environment { vars: Vec::new() });
    }

    #[test]
    fn oversize_environment_values_are_rejected() {
        let frame = encode_frame(&Message::Environment {
            vars: vec![("DISPLAY".to_owned(), "x".repeat(MAX_TITLE_LEN + 1))],
        });
        assert!(matches!(
            decode_frame(&frame),
            Err(DecodeError::TitleTooLong { .. })
        ));
    }

    #[test]
    fn roundtrip_switcher() {
        for action in [
            SwitcherAction::Step { forward: true },
            SwitcherAction::Step { forward: false },
            SwitcherAction::Commit,
            SwitcherAction::Cancel,
        ] {
            roundtrip(&Message::Switcher { action });
        }
    }

    #[test]
    fn roundtrip_hello() {
        roundtrip(&Message::Hello {
            version: ProtocolVersion::CURRENT,
        });
    }

    #[test]
    fn roundtrip_snapshot() {
        roundtrip(&Message::Snapshot {
            revision: 42,
            windows: vec![sample_window(7), sample_window(8)],
            workspaces: vec![sample_workspace(1), sample_workspace(2)],
            locked: false,
        });
        // Locked snapshots carry the flag with no window content.
        roundtrip(&Message::Snapshot {
            revision: 43,
            windows: vec![],
            workspaces: vec![sample_workspace(1)],
            locked: true,
        });
    }

    #[test]
    fn roundtrip_changes() {
        roundtrip(&Message::Changes {
            from_revision: 42,
            to_revision: 45,
            ops: vec![
                StateOp::WindowOpened(sample_window(9)),
                StateOp::WindowFocused { id: 9 },
                StateOp::WindowMoved {
                    id: 7,
                    workspace: 2,
                },
                StateOp::WindowClosed { id: 8 },
                StateOp::WorkspaceChanged(sample_workspace(2)),
            ],
        });
    }

    #[test]
    fn roundtrip_command() {
        for kind in [
            CommandKind::ActivateWindow {
                window: 7,
                token: sample_token(),
            },
            CommandKind::FocusWorkspace { workspace: 2 },
            CommandKind::ToggleOverview,
            CommandKind::CloseWindow { window: 7 },
            CommandKind::Lock,
        ] {
            roundtrip(&Message::Command { id: 99, kind });
        }
    }

    #[test]
    fn roundtrip_command_result() {
        roundtrip(&Message::CommandResult {
            id: 99,
            status: CommandStatus::Applied,
        });
        roundtrip(&Message::CommandResult {
            id: 100,
            status: CommandStatus::Denied {
                reason: "stale token".to_owned(),
            },
        });
    }

    #[test]
    fn roundtrip_error() {
        roundtrip(&Message::Error {
            kind: ErrorKind::RevisionGap,
            message: "have 3, need 5; resnapshot".to_owned(),
        });
    }

    #[test]
    fn rejects_oversize_before_decode() {
        // Declared length over the cap with no body at all: must still be
        // Oversize (never Truncated or Malformed), proving the cap runs
        // before postcard decoding.
        let mut bytes = ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes().to_vec();
        assert!(matches!(
            decode_frame(&bytes),
            Err(DecodeError::Oversize { .. })
        ));
        // Garbage body under a huge declared length: still Oversize.
        bytes.extend_from_slice(&[0xFF; 8]);
        assert!(matches!(
            decode_frame(&bytes),
            Err(DecodeError::Oversize { .. })
        ));
    }

    #[test]
    fn rejects_truncated_frames() {
        let frame = encode_frame(&Message::Hello {
            version: ProtocolVersion::CURRENT,
        });
        assert!(frame.len() > 4);
        // Short prefix.
        assert!(matches!(
            decode_frame(&frame[..2]),
            Err(DecodeError::Truncated { .. })
        ));
        // Empty input.
        assert!(matches!(
            decode_frame(&[]),
            Err(DecodeError::Truncated { .. })
        ));
        // Body cut short.
        assert!(matches!(
            decode_frame(&frame[..frame.len() - 1]),
            Err(DecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn rejects_trailing_bytes_as_malformed() {
        let mut frame = encode_frame(&Message::Command {
            id: 1,
            kind: CommandKind::ToggleOverview,
        });
        frame.push(0x00);
        assert!(matches!(
            decode_frame(&frame),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_stale_major() {
        for version in [ProtocolVersion::new(1, 1), ProtocolVersion::new(9, 0)] {
            let frame = encode_frame(&Message::Hello { version });
            match decode_frame(&frame) {
                Err(DecodeError::IncompatibleVersion { got, current }) => {
                    assert_eq!(got, version);
                    assert_eq!(current, CURRENT_VERSION);
                }
                other => panic!("expected IncompatibleVersion, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_overlong_title_at_decode() {
        let mut window = sample_window(7);
        window.title = "x".repeat(MAX_TITLE_LEN + 1);
        let msg = Message::Snapshot {
            revision: 1,
            windows: vec![window],
            workspaces: vec![],
            locked: false,
        };
        let frame = encode_frame(&msg);
        match decode_frame(&frame) {
            Err(DecodeError::TitleTooLong { len, max }) => {
                assert_eq!(len, MAX_TITLE_LEN + 1);
                assert_eq!(max, MAX_TITLE_LEN);
            }
            other => panic!("expected TitleTooLong, got {other:?}"),
        }
    }

    #[test]
    fn accepts_title_at_exact_cap() {
        let mut window = sample_window(7);
        window.title = "x".repeat(MAX_TITLE_LEN);
        let msg = Message::Snapshot {
            revision: 1,
            windows: vec![window],
            workspaces: vec![],
            locked: false,
        };
        roundtrip(&msg);
    }

    #[test]
    fn rejects_overlong_title_in_changes_op() {
        let mut window = sample_window(7);
        window.title = "y".repeat(MAX_TITLE_LEN + 64);
        let msg = Message::Changes {
            from_revision: 1,
            to_revision: 2,
            ops: vec![StateOp::WindowOpened(window)],
        };
        let frame = encode_frame(&msg);
        assert!(matches!(
            decode_frame(&frame),
            Err(DecodeError::TitleTooLong { .. })
        ));
    }

    #[test]
    fn token_debug_is_redacted() {
        let shown = format!("{:?}", sample_token());
        assert!(!shown.contains("opaque-token-123"), "token leaked: {shown}");
    }

    #[test]
    fn control_error_display_io() {
        let err = ControlError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        let shown = format!("{err}");
        assert!(shown.contains("control socket I/O"));
    }

    #[test]
    fn control_error_display_decode() {
        let decode_err = DecodeError::Truncated {
            expected: 100,
            actual: 50,
        };
        let err = ControlError::Decode(decode_err);
        let shown = format!("{err}");
        assert!(shown.contains("control decode"));
    }

    #[test]
    fn control_error_display_would_block() {
        let err = ControlError::WouldBlock;
        let shown = format!("{err}");
        assert!(shown.contains("control socket not ready"));
    }

    #[test]
    fn control_error_display_unexpected() {
        let err = ControlError::Unexpected("out of order".to_owned());
        let shown = format!("{err}");
        assert!(shown.contains("unexpected message or state"));
        assert!(shown.contains("out of order"));
    }

    #[test]
    fn control_error_display_remote() {
        let err = ControlError::Remote {
            kind: ErrorKind::RevisionGap,
            message: "revision 5 needed".to_owned(),
        };
        let shown = format!("{err}");
        assert!(shown.contains("remote error"));
        assert!(shown.contains("revision 5 needed"));
    }

    #[test]
    fn control_error_display_app_constraint() {
        let err = ControlError::AppConstraint("no active window".to_owned());
        let shown = format!("{err}");
        assert!(shown.contains("constraint"));
        assert!(shown.contains("no active window"));
    }

    #[test]
    fn control_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
        let err: ControlError = io_err.into();
        assert!(matches!(err, ControlError::Io(_)));
    }

    #[test]
    fn control_error_from_would_block_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::WouldBlock, "would block");
        let err: ControlError = io_err.into();
        assert!(matches!(err, ControlError::WouldBlock));
    }

    #[test]
    fn control_error_from_decode_error() {
        let decode_err = DecodeError::Oversize {
            len: 2_000_000,
            max: MAX_FRAME_BYTES,
        };
        let err: ControlError = decode_err.into();
        assert!(matches!(err, ControlError::Decode(_)));
    }

    #[test]
    fn control_error_source_io() {
        use std::error::Error;
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe");
        let err = ControlError::Io(io_err);
        assert!(err.source().is_some());
    }

    #[test]
    fn control_error_source_decode() {
        use std::error::Error;
        let decode_err = DecodeError::Malformed(postcard::Error::DeserializeBadEncoding);
        let err = ControlError::Decode(decode_err);
        assert!(err.source().is_some());
    }

    #[test]
    fn control_error_source_other_variants() {
        use std::error::Error;
        let err = ControlError::WouldBlock;
        assert!(err.source().is_none());
        let err = ControlError::Unexpected("test".to_owned());
        assert!(err.source().is_none());
        let err = ControlError::AppConstraint("test".to_owned());
        assert!(err.source().is_none());
    }

    #[test]
    fn activation_token_debug_is_redacted() {
        let token = ActivationToken::new("secret-token-xyz".to_owned());
        let shown = format!("{:?}", token);
        assert!(!shown.contains("secret-token-xyz"), "token leaked: {shown}");
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn activation_token_equality() {
        let token1 = ActivationToken::new("token123".to_owned());
        let token2 = ActivationToken::new("token123".to_owned());
        let token3 = ActivationToken::new("other".to_owned());
        assert_eq!(token1, token2);
        assert_ne!(token1, token3);
    }
}

#[cfg(test)]
mod brightness_wire_tests {
    use super::*;
    fn binding(id: u32) -> BrightnessBinding {
        BrightnessBinding {
            output: NativeOutputInfo {
                name: "n".repeat(512),
                drm_device: u64::MAX,
                connector_id: id,
                connector_sysfs: "s".repeat(512),
                connector_device: u64::MAX,
                connector_inode: u64::MAX,
            },
            ownership_generation: u64::MAX,
            backlight: "b".repeat(128),
            device: u64::MAX,
            inode: u64::MAX,
            minimum: 0,
            maximum: u32::MAX,
        }
    }
    #[test]
    fn maximum_full_journal_snapshot_fits_actual_one_mib_frame() {
        let readings: Vec<BrightnessReading> = (1..=64)
            .map(|id| BrightnessReading {
                binding: binding(id),
                user: u32::MAX,
                user_known: true,
                measured_candidate: Some(BrightnessMeasuredCandidate {
                    level: u32::MAX,
                    sampled_revision: u64::MAX,
                    provider_epoch: Some(u64::MAX),
                }),
                measured_restoration: Some(BrightnessMeasuredRestoration {
                    level: u32::MAX,
                    sampled_revision: u64::MAX,
                    provider_epoch: u64::MAX,
                    establishing_grant: BrightnessGrant {
                        transaction: u64::MAX,
                        revision: u64::MAX,
                    },
                }),
                applied: u32::MAX,
                ratio: 1.0,
            })
            .collect();
        let targets = (1..=64)
            .map(|id| BrightnessTarget {
                binding: binding(id),
                effective: u32::MAX,
                user: Some(u32::MAX),
                ratio: Some(1.0),
            })
            .collect();
        let message = Message::BrightnessJournalReply {
            request: u64::MAX,
            grant: Some(BrightnessGrant {
                transaction: u64::MAX,
                revision: u64::MAX,
            }),
            error: None,
            state: Some(BrightnessJournalSnapshot {
                revision: u64::MAX,
                native_generation: Some(u64::MAX),
                authority: Some(BrightnessAuthority {
                    child_generation: u64::MAX,
                    connection_generation: u64::MAX,
                }),
                provider: Some(BrightnessProvider {
                    unique: "u".repeat(512),
                    epoch: u64::MAX,
                    uid: u32::MAX,
                    pid: u32::MAX,
                    start: "s".repeat(512),
                }),
                retired_readings: readings.clone(),
                readings,
                policy: BrightnessPolicy {
                    dimming: None,
                    automatic: None,
                    idle: 0.3,
                },
                pending: Some(BrightnessPending {
                    grant: BrightnessGrant {
                        transaction: u64::MAX,
                        revision: u64::MAX,
                    },
                    targets,
                    interrupted: true,
                }),
            }),
        };
        let frame = encode_frame(&message);
        assert!(frame.len() - 4 < MAX_FRAME_BYTES);
        assert!(
            frame.len() < 256 * 1024,
            "full bounded bindings need no artificial 4096-byte chunking"
        );
        assert_eq!(decode_frame(&frame).unwrap(), message);
        let Message::BrightnessJournalReply {
            state: Some(state), ..
        } = message
        else {
            panic!("maximum reply lost state");
        };
        let state_message = Message::BrightnessJournalState { state };
        let state_frame = encode_frame(&state_message);
        assert!(state_frame.len() - 4 < MAX_FRAME_BYTES);
        assert_eq!(decode_frame(&state_frame).unwrap(), state_message);
    }
    #[test]
    fn journal_target_and_string_bounds_are_enforced_on_decode() {
        let target = BrightnessTarget {
            binding: binding(1),
            effective: 0,
            user: None,
            ratio: None,
        };
        let message = Message::BrightnessJournalBegin {
            request: 1,
            source: BrightnessSource::User,
            targets: vec![target; 65],
        };
        assert!(matches!(
            decode_frame(&encode_frame(&message)),
            Err(DecodeError::CollectionTooLong { len: 65, max: 64 })
        ));
        let mut bad = binding(1);
        bad.output.connector_sysfs.push('s');
        let message = Message::BrightnessJournalInit {
            request: 1,
            idle: 0.3,
            readings: vec![BrightnessReading {
                binding: bad,
                user: 0,
                user_known: false,
                measured_candidate: None,
                measured_restoration: None,
                applied: 0,
                ratio: 1.0,
            }],
        };
        assert!(decode_frame(&encode_frame(&message)).is_err());
    }
}

#[cfg(test)]
mod accelerator_release_wire_tests {
    use super::*;
    fn grab(flags: u32) -> AcceleratorGrab {
        AcceleratorGrab {
            accelerator: Accelerator {
                action: 7,
                keysym: 114,
                mods: 0,
                modes: MODE_NORMAL,
            },
            flags,
        }
    }
    #[test]
    fn typed_grab_and_release_roundtrip_without_changing_legacy() {
        for message in [
            Message::Command {
                id: 9,
                kind: CommandKind::SetAcceleratorGrabs {
                    grabs: vec![grab(GRAB_TRIGGER_RELEASE | GRAB_IGNORE_AUTOREPEAT)],
                },
            },
            Message::AcceleratorDeactivated {
                action: 7,
                time: 200,
                mode: MODE_NORMAL,
            },
            Message::Command {
                id: 10,
                kind: CommandKind::SetAccelerators {
                    accelerators: vec![grab(0).accelerator],
                },
            },
        ] {
            assert_eq!(decode_frame(&encode_frame(&message)).unwrap(), message);
        }
    }
    #[test]
    fn typed_grab_decode_rejects_unknown_flags_and_oversized_lists() {
        for grabs in [vec![grab(1 << 3)], vec![grab(0); MAX_ACCELERATORS + 1]] {
            assert!(decode_frame(&encode_frame(&Message::Command {
                id: 11,
                kind: CommandKind::SetAcceleratorGrabs { grabs }
            }))
            .is_err());
        }
    }
}

#[cfg(test)]
mod integrated_append_order_tests {
    use super::*;

    #[test]
    fn minor_28_through_31_preserve_original_postcard_variant_positions() {
        let state = BrightnessJournalSnapshot {
            revision: 0,
            native_generation: None,
            authority: None,
            provider: None,
            readings: Vec::new(),
            retired_readings: Vec::new(),
            policy: BrightnessPolicy {
                dimming: None,
                automatic: None,
                idle: 0.3,
            },
            pending: None,
        };
        let grant = BrightnessGrant {
            transaction: 1,
            revision: 1,
        };
        let messages = [
            (
                16,
                Message::ScreenReader {
                    state: ScreenReaderState::Disabled,
                },
            ),
            (
                17,
                Message::AcceleratorDeactivated {
                    action: 7,
                    time: 200,
                    mode: MODE_NORMAL,
                },
            ),
            (
                18,
                Message::NativeOutputInventory {
                    outputs: Vec::new(),
                },
            ),
            (
                19,
                Message::MonitorIdentityInventory {
                    outputs: Vec::new(),
                },
            ),
            (
                20,
                Message::BrightnessJournalInit {
                    request: 1,
                    readings: Vec::new(),
                    idle: 0.3,
                },
            ),
            (
                21,
                Message::BrightnessJournalBegin {
                    request: 2,
                    source: BrightnessSource::User,
                    targets: Vec::new(),
                },
            ),
            (
                22,
                Message::BrightnessJournalComplete {
                    request: 3,
                    grant,
                    observations: Vec::new(),
                },
            ),
            (
                23,
                Message::BrightnessJournalState {
                    state: state.clone(),
                },
            ),
            (
                24,
                Message::BrightnessJournalReply {
                    request: 4,
                    grant: None,
                    error: None,
                    state: Some(state),
                },
            ),
        ];
        for (original_index, message) in messages {
            let frame = encode_frame(&message);
            assert_eq!(
                frame[4], original_index,
                "append-only postcard message position changed"
            );
            assert_eq!(decode_frame(&frame).unwrap(), message);
        }
        for (original_index, command) in [
            (16, CommandKind::SetScreenReader { enabled: false }),
            (17, CommandKind::SetAcceleratorGrabs { grabs: Vec::new() }),
        ] {
            let body = postcard::to_allocvec(&command).unwrap();
            assert_eq!(
                body[0], original_index,
                "append-only postcard command position changed"
            );
        }
        assert_eq!(CURRENT_VERSION.minor, 31);
    }
}
