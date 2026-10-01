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
    pub const CURRENT: Self = Self { major: 0, minor: 8 };

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

/// Version spoken by this crate (`0.2`).
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
    /// Unlock is never a command: it goes through session auth.
    Lock,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
fn validate_titles(msg: &Message) -> Result<(), DecodeError> {
    fn check_title(title: &str) -> Result<(), DecodeError> {
        if title.len() > MAX_TITLE_LEN {
            return Err(DecodeError::TitleTooLong {
                len: title.len(),
                max: MAX_TITLE_LEN,
            });
        }
        Ok(())
    }

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
        Message::Hello { .. }
        | Message::Command { .. }
        | Message::CommandResult { .. }
        | Message::Error { .. }
        | Message::Overview { .. }
        | Message::Switcher { .. }
        | Message::Outputs { .. } => {}
        // Names and values share the title bound: short by nature.
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

    fn sample_token() -> ActivationToken {
        ActivationToken::new("opaque-token-123".to_owned())
    }

    fn roundtrip(msg: &Message) {
        let frame = encode_frame(msg);
        let back = decode_frame(&frame).expect("roundtrip must decode");
        assert_eq!(&back, msg);
    }

    #[test]
    fn current_version_is_0_8() {
        assert_eq!(CURRENT_VERSION, ProtocolVersion::new(0, 8));
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
        assert!(!ProtocolVersion::new(0, 9).is_compatible_with(&ours));
        assert!(!ProtocolVersion::new(1, 4).is_compatible_with(&ours));
        assert!(!ProtocolVersion::new(1, 0).is_compatible_with(&ours));
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
}
