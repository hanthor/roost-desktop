//! Compositor-side control channel (001 R4/R6, ADR 0002).
//!
//! Private local IPC between the compositor (authoritative) and the shell
//! host. Wire types and framing live in the `roost-shell-control` crate
//! (version handshake, length-prefixed frames, 1 MiB cap); this module owns
//! the compositor side: accept, handshake, snapshots, change deltas, and
//! command dispatch over a [`StateModel`].
//!
//! Nonblocking contract (spec R6): every socket here is nonblocking. No
//! method in this module blocks: when a read or write cannot proceed without
//! blocking it returns [`ControlError::WouldBlock`] immediately and the
//! caller is expected to wait for readiness (e.g. via poll) and retry, or —
//! for a peer that stays unready, per ADR 0002 backpressure — drop the
//! connection. A slow shell is disconnected; compositor input/frame paths
//! never wait on it. Note a `WouldBlock` from [`ControlConn::write_frame`]
//! may leave a partial frame behind, so the connection must be dropped
//! rather than retried once backpressure has been observed.
//!
//! Session rules (ADR 0002): full [`Message::Snapshot`] on every (re)connect
//! and after any revision gap (resnapshot rule — the shell discards state
//! and renders from the fresh snapshot); ordered [`Message::Changes`] deltas
//! otherwise; typed [`Message::Error`] for malformed, oversized,
//! stale-version, and unknown-kind frames, with the offending frame dropped.
//!
//! Activation policy: [`Session`] validates `ActivateWindow` commands through
//! a caller-supplied `Fn(&ActivationToken, Option<&str>) -> bool` (token,
//! target window `app_id`) so policy stays injectable. The default
//! ([`deny_all_tokens`]) denies everything (fail-closed); the live policy
//! (30 s expiry, one-use, seat binding, `app_id` match) is the
//! [`TokenStore`](crate::state::TokenStore) per ADR 0002, not protocol
//! behavior.
//!
//! Reference patterns (framing/versioning discipline, snapshot-on-connect)
//! follow the niri survey in `.spektacular/work/wave2/stream1-notes.md`;
//! all code below is original.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

use roost_shell_control::{
    decode_frame, encode_frame, ActivationToken, CommandKind, CommandStatus, DecodeError,
    ErrorKind, Message, ProtocolVersion, StateOp, SwitcherAction, WorkspaceInfo, CURRENT_VERSION,
    MAX_FRAME_BYTES,
};

use crate::state::{StateChange, StateModel, TokenStore, WindowEntry};

/// Failure of a control-channel operation.
#[derive(Debug)]
pub enum ControlError {
    /// Underlying socket I/O failure.
    Io(std::io::Error),
    /// Frame failed schema validation (oversize, malformed, stale major,
    /// over-long title). The offending frame is dropped.
    Decode(DecodeError),
    /// Nonblocking socket not ready (spec R6). Nothing was consumed from a
    /// read; a write may be partial, so drop the connection on write
    /// backpressure (see module docs).
    WouldBlock,
    /// Peer closed with no complete frame buffered.
    Closed,
    /// Peer violated the session protocol (e.g. first message is not
    /// `Hello`). A typed `Error` was sent where possible.
    Protocol(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "control socket I/O: {e}"),
            Self::Decode(e) => write!(f, "control decode: {e}"),
            Self::WouldBlock => write!(f, "control socket not ready"),
            Self::Closed => write!(f, "control peer closed"),
            Self::Protocol(e) => write!(f, "control protocol: {e}"),
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

/// Bound control socket. Wraps an already-bound [`UnixListener`] (tests pass
/// a bound socket or use a socketpair directly with [`ControlConn`]).
pub struct ControlServer {
    listener: UnixListener,
}

impl ControlServer {
    /// Take ownership of an already-bound listener and put it in
    /// nonblocking mode, so [`accept`](Self::accept) is a try-accept.
    pub fn new(listener: UnixListener) -> std::io::Result<Self> {
        listener.set_nonblocking(true)?;
        Ok(Self { listener })
    }

    /// Accept one pending connection. Returns [`ControlError::WouldBlock`]
    /// when no peer is waiting; never blocks.
    pub fn accept(&self) -> Result<ControlConn, ControlError> {
        match self.listener.accept() {
            Ok((stream, _)) => ControlConn::new(stream),
            Err(e) => Err(ControlError::from(e)),
        }
    }
}

/// One control connection: framed, nonblocking, schema-validated transport.
///
/// `read_frame` returns whole [`Message`]s (u32-LE length prefix plus the
/// schema crate's decode, including the 1 MiB cap and stale-major reject);
/// `write_frame` sends one. Both surface [`ControlError::WouldBlock`]
/// instead of blocking (spec R6).
pub struct ControlConn {
    stream: UnixStream,
    rbuf: Vec<u8>,
}

impl ControlConn {
    /// Wrap a connected stream and put it in nonblocking mode.
    pub fn new(stream: UnixStream) -> Result<Self, ControlError> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            rbuf: Vec::new(),
        })
    }

    /// Read one complete frame. Buffers short reads internally and returns
    /// [`ControlError::WouldBlock`] until a whole frame has arrived; the
    /// length cap is enforced from the prefix before any body is awaited.
    pub fn read_frame(&mut self) -> Result<Message, ControlError> {
        loop {
            if self.rbuf.len() >= 4 {
                let len =
                    u32::from_le_bytes([self.rbuf[0], self.rbuf[1], self.rbuf[2], self.rbuf[3]])
                        as usize;
                if len > MAX_FRAME_BYTES {
                    return Err(ControlError::Decode(DecodeError::Oversize {
                        len,
                        max: MAX_FRAME_BYTES,
                    }));
                }
                if self.rbuf.len() >= 4 + len {
                    let frame = self.rbuf[..4 + len].to_vec();
                    self.rbuf.drain(..4 + len);
                    return decode_frame(&frame).map_err(ControlError::Decode);
                }
            }
            let mut chunk = [0u8; 8192];
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    if self.rbuf.is_empty() {
                        return Err(ControlError::Closed);
                    }
                    return Err(ControlError::Decode(DecodeError::Truncated {
                        expected: self.rbuf.len(),
                        actual: self.rbuf.len(),
                    }));
                }
                Ok(n) => self.rbuf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(ControlError::from(e)),
            }
        }
    }

    /// Write one message as a single frame. Returns
    /// [`ControlError::WouldBlock`] on backpressure; per the module docs the
    /// connection must then be dropped (a partial frame may be on the wire).
    pub fn write_frame(&mut self, msg: &Message) -> Result<(), ControlError> {
        let frame = encode_frame(msg);
        let mut written = 0;
        while written < frame.len() {
            match self.stream.write(&frame[written..]) {
                Ok(0) => return Err(ControlError::Closed),
                Ok(n) => written += n,
                Err(e) => return Err(ControlError::from(e)),
            }
        }
        Ok(())
    }
}

/// Fail-closed activation-token validator: denies every token.
///
/// This is the [`Session`] default so that forgetting to wire real policy
/// denies rather than allows (ADR 0002 policy: 30 s expiry, one-use, seat
/// binding, `app_id` match — compositor policy code, injected here). The
/// expected `app_id` is the target window's, looked up from the model by
/// [`apply_command`].
pub fn deny_all_tokens(_: &ActivationToken, _: Option<&str>) -> bool {
    false
}

/// Outcome of [`Session::handle_next`]: what one inbound frame produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handled {
    /// A shell command was answered with `CommandResult`.
    CommandResult {
        /// Echo of the shell's request id.
        id: u64,
        /// Whether the command was applied (false = denied).
        applied: bool,
    },
    /// A typed `Error` was sent for a bad frame or unexpected message kind.
    ErrorSent {
        /// Category that was sent.
        kind: ErrorKind,
    },
    /// A mid-session `Hello` was treated as a reconnect: `Hello` plus a
    /// fresh snapshot were sent.
    HelloResync {
        /// Revision of the snapshot that was sent.
        revision: u64,
    },
}

/// Outcome of [`Session::emit_deltas`]: how the shell was caught up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emitted {
    /// Incremental `Changes` were sent.
    Changes {
        /// Shell revision the deltas apply from.
        from: u64,
        /// Revision the shell holds after applying them.
        to: u64,
        /// Number of ops sent.
        ops: usize,
    },
    /// A [`RevisionGap`](crate::state::RevisionGap) forced a fresh snapshot
    /// (resnapshot rule).
    Snapshot {
        /// Revision of the snapshot that was sent.
        revision: u64,
    },
    /// Shell was already current; cursor advanced, nothing sent.
    Idle {
        /// Current revision.
        revision: u64,
    },
}

/// Token-check hook for `ActivateWindow` commands.
pub type TokenValidator<'a> = Box<dyn Fn(&ActivationToken, Option<&str>) -> bool + 'a>;
/// Per-window activation-token minter for snapshots and deltas.
pub type TokenMinter = std::rc::Rc<dyn Fn(Option<&str>) -> String>;

/// Authenticated control session over a [`ControlConn`].
///
/// The session owns the connection, the token-check hook, the per-window
/// token minter, and the last revision the shell is known to hold. The
/// [`StateModel`] itself is passed per call (`&mut` where commands may
/// mutate, `&` otherwise) so callers can drive the model between session
/// calls. Build with [`handshake`](Self::handshake) (fail-closed tokens)
/// or [`handshake_with`](Self::handshake_with) (live policy); the
/// handshake sends our `Hello` plus a full snapshot, so every (re)connect
/// starts from complete state.
pub struct Session<'a> {
    conn: ControlConn,
    validator: TokenValidator<'a>,
    minter: TokenMinter,
    last_revision: u64,
    /// Shared overview intent with the hub (002 R1): the shell-facing
    /// open state flipped by `ToggleOverview` commands and runtime
    /// triggers, broadcast back as [`Message::Overview`].
    overview: std::rc::Rc<std::cell::Cell<bool>>,
}

impl std::fmt::Debug for Session<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("last_revision", &self.last_revision)
            .finish_non_exhaustive()
    }
}

impl<'a> Session<'a> {
    /// Server-side handshake: expect `Hello` first, reply `Hello` with
    /// [`CURRENT_VERSION`], then send a full snapshot. A stale major gets a
    /// typed `IncompatibleVersion` error and the connection is refused
    /// (caller drops this result's absence — there is no session to use).
    /// Uses [`deny_all_tokens`] (fail-closed).
    pub fn handshake(conn: ControlConn, model: &StateModel) -> Result<Self, ControlError> {
        Self::handshake_with(
            conn,
            model,
            deny_all_tokens,
            std::rc::Rc::new(|_| String::new()),
            std::rc::Rc::new(std::cell::Cell::new(false)),
        )
    }

    /// [`handshake`](Self::handshake) with an injectable activation-token
    /// policy for `ActivateWindow` commands plus the per-window token
    /// minter used for snapshots and deltas. In live sessions both come
    /// from one [`TokenStore`](crate::state::TokenStore)
    /// ([`minter`](crate::state::TokenStore::minter) /
    /// [`validator`](crate::state::TokenStore::validator)); the default
    /// minter issues empty tokens that can never validate. The shared
    /// `overview` cell joins the hub's overview intent so shell
    /// `ToggleOverview` commands flip state the hub broadcasts.
    pub fn handshake_with(
        conn: ControlConn,
        model: &StateModel,
        validator: impl Fn(&ActivationToken, Option<&str>) -> bool + 'a,
        minter: TokenMinter,
        overview: std::rc::Rc<std::cell::Cell<bool>>,
    ) -> Result<Self, ControlError> {
        let mut session = Self {
            conn,
            validator: Box::new(validator),
            minter,
            last_revision: 0,
            overview,
        };
        let msg = match session.conn.read_frame() {
            Ok(msg) => msg,
            Err(ControlError::Decode(e)) => {
                let (kind, text) = decode_error_to_wire(&e);
                let _ = session.conn.write_frame(&Message::Error {
                    kind,
                    message: text,
                });
                return Err(ControlError::Decode(e));
            }
            Err(e) => return Err(e),
        };
        let Message::Hello { version } = msg else {
            let _ = session.conn.write_frame(&Message::Error {
                kind: ErrorKind::UnknownCommand,
                message: "expected Hello as first message".to_owned(),
            });
            return Err(ControlError::Protocol(
                "first message is not Hello".to_owned(),
            ));
        };
        if version.major != CURRENT_VERSION.major {
            let _ = session.conn.write_frame(&Message::Error {
                kind: ErrorKind::IncompatibleVersion,
                message: format!(
                    "incompatible peer major {}.{} (ours {}.{})",
                    version.major, version.minor, CURRENT_VERSION.major, CURRENT_VERSION.minor
                ),
            });
            return Err(ControlError::Decode(DecodeError::IncompatibleVersion {
                got: version,
                current: CURRENT_VERSION,
            }));
        }
        // Same major line: accept any minor (lenient reader on our side).
        session.conn.write_frame(&Message::Hello {
            version: CURRENT_VERSION,
        })?;
        session.send_snapshot(model)?;
        Ok(session)
    }

    /// Last revision the shell is known to hold (sent snapshot or deltas).
    pub fn last_revision(&self) -> u64 {
        self.last_revision
    }

    /// Send a full snapshot of `model` (on-request path; the handshake and
    /// gap paths call this internally). Every window carries a freshly
    /// minted activation token from this session's minter.
    pub fn send_snapshot(&mut self, model: &StateModel) -> Result<u64, ControlError> {
        let revision = model.revision();
        self.conn
            .write_frame(&snapshot_message(model, &*self.minter))?;
        self.last_revision = revision;
        Ok(revision)
    }

    /// Send the current overview intent (002 R1). Best-effort like the
    /// rest of the nonblocking plane: a `WouldBlock` caller keeps the
    /// hub dirty flag set and retries next round.
    pub fn send_overview(&mut self, open: bool) -> Result<(), ControlError> {
        self.conn.write_frame(&Message::Overview { open })
    }

    /// Send one Alt-Tab switcher drive event (002 workspaces).
    pub fn send_switcher(&mut self, action: SwitcherAction) -> Result<(), ControlError> {
        self.conn.write_frame(&Message::Switcher { action })
    }

    /// Catch the shell up from [`last_revision`](Self::last_revision):
    /// incremental `Changes` when the change log covers the gap, else a
    /// fresh snapshot (resnapshot rule). New token-bearing entries mint
    /// through this session's minter.
    pub fn emit_deltas(&mut self, model: &StateModel) -> Result<Emitted, ControlError> {
        match model.changes_since(self.last_revision) {
            Ok(entries) => {
                let ops: Vec<StateOp> = entries
                    .iter()
                    .filter_map(|(_, c)| state_op(c, &*self.minter))
                    .collect();
                let to = model.revision();
                let from = self.last_revision;
                if ops.is_empty() {
                    self.last_revision = to;
                    return Ok(Emitted::Idle { revision: to });
                }
                let count = ops.len();
                self.conn.write_frame(&Message::Changes {
                    from_revision: from,
                    to_revision: to,
                    ops,
                })?;
                self.last_revision = to;
                Ok(Emitted::Changes {
                    from,
                    to,
                    ops: count,
                })
            }
            Err(_) => {
                let revision = self.send_snapshot(model)?;
                Ok(Emitted::Snapshot { revision })
            }
        }
    }

    /// Read one inbound frame and act on it:
    /// - `Command` → apply against `model` (activation commands go through
    ///   the token hook) and reply `CommandResult` with the same id. Command
    ///   kinds outside the schema enum cannot decode; they arrive here as
    ///   `MalformedFrame` errors, and message kinds the shell must never
    ///   send (`Snapshot`, `Changes`, `CommandResult`, `Error`) get a typed
    ///   `UnknownCommand` error.
    /// - `Hello` → reconnect on a live connection: resend our `Hello` plus
    ///   a fresh snapshot.
    /// - Decode failures → typed `Error` for the offending frame; oversize
    ///   and stale-version failures additionally close the session (an
    ///   `Err` is returned after the `Error` is sent).
    pub fn handle_next(&mut self, model: &mut StateModel) -> Result<Handled, ControlError> {
        let msg = match self.conn.read_frame() {
            Ok(msg) => msg,
            Err(ControlError::Decode(e)) => {
                let (kind, text) = decode_error_to_wire(&e);
                let drop_conn = matches!(
                    e,
                    DecodeError::Oversize { .. } | DecodeError::IncompatibleVersion { .. }
                );
                let _ = self.conn.write_frame(&Message::Error {
                    kind,
                    message: text,
                });
                if drop_conn {
                    return Err(ControlError::Decode(e));
                }
                return Ok(Handled::ErrorSent { kind });
            }
            Err(e) => return Err(e),
        };
        match msg {
            Message::Hello { version } => {
                if version.major != CURRENT_VERSION.major {
                    let _ = self.conn.write_frame(&Message::Error {
                        kind: ErrorKind::IncompatibleVersion,
                        message: format!(
                            "incompatible peer major {}.{} (ours {}.{})",
                            version.major,
                            version.minor,
                            CURRENT_VERSION.major,
                            CURRENT_VERSION.minor
                        ),
                    });
                    return Err(ControlError::Decode(DecodeError::IncompatibleVersion {
                        got: version,
                        current: CURRENT_VERSION,
                    }));
                }
                self.conn.write_frame(&Message::Hello {
                    version: CURRENT_VERSION,
                })?;
                let revision = self.send_snapshot(model)?;
                Ok(Handled::HelloResync { revision })
            }
            Message::Command { id, kind } => {
                let status = apply_command(model, &self.validator, &self.overview, &kind);
                let applied = matches!(status, CommandStatus::Applied);
                self.conn
                    .write_frame(&Message::CommandResult { id, status })?;
                Ok(Handled::CommandResult { id, applied })
            }
            Message::Snapshot { .. }
            | Message::Changes { .. }
            | Message::CommandResult { .. }
            | Message::Overview { .. }
            | Message::Switcher { .. }
            | Message::Error { .. } => {
                let _ = self.conn.write_frame(&Message::Error {
                    kind: ErrorKind::UnknownCommand,
                    message: format!("unexpected {} from shell", message_kind(&msg)),
                });
                Ok(Handled::ErrorSent {
                    kind: ErrorKind::UnknownCommand,
                })
            }
        }
    }
}

/// Map a frame-decoding failure to the typed error sent for it.
fn decode_error_to_wire(e: &DecodeError) -> (ErrorKind, String) {
    match e {
        DecodeError::Oversize { .. } => (ErrorKind::OversizeFrame, e.to_string()),
        DecodeError::IncompatibleVersion { .. } => (ErrorKind::IncompatibleVersion, e.to_string()),
        DecodeError::Truncated { .. }
        | DecodeError::Malformed(_)
        | DecodeError::TitleTooLong { .. } => (ErrorKind::MalformedFrame, e.to_string()),
    }
}

/// Short kind name for diagnostics (never sensitive content).
fn message_kind(msg: &Message) -> &'static str {
    match msg {
        Message::Hello { .. } => "Hello",
        Message::Snapshot { .. } => "Snapshot",
        Message::Changes { .. } => "Changes",
        Message::Command { .. } => "Command",
        Message::CommandResult { .. } => "CommandResult",
        Message::Error { .. } => "Error",
        Message::Overview { .. } => "Overview",
        Message::Switcher { .. } => "Switcher",
    }
}

/// Build the full-snapshot message for `model`, minting one activation
/// token per window through `mint`.
fn snapshot_message(model: &StateModel, mint: &dyn Fn(Option<&str>) -> String) -> Message {
    let snap = model.snapshot();
    Message::Snapshot {
        revision: snap.revision,
        windows: snap
            .windows
            .iter()
            .map(|w| window_to_wire(w, mint))
            .collect(),
        workspaces: snap
            .workspaces
            .iter()
            .map(|id| WorkspaceInfo {
                id: *id as u64,
                name: None,
                active: *id == snap.active,
            })
            .collect(),
    }
}

/// Map one model window to its wire mirror, minting its activation token
/// through `mint` (bound to the window's `app_id`).
fn window_to_wire(
    w: &WindowEntry,
    mint: &dyn Fn(Option<&str>) -> String,
) -> roost_shell_control::WindowInfo {
    roost_shell_control::WindowInfo {
        id: w.id,
        title: w.title.clone(),
        app_id: w.app_id.clone(),
        workspace: w.workspace as u64,
        focused: w.focused,
        activation_token: mint(w.app_id.as_deref()),
    }
}

/// Map one model change to its wire op, minting tokens for restated
/// windows through `mint`. Clearing focus has no wire counterpart (focus
/// is carried on the window entries and the `WindowFocused` op), so it
/// maps to `None` and only advances the cursor.
fn state_op(change: &StateChange, mint: &dyn Fn(Option<&str>) -> String) -> Option<StateOp> {
    match change {
        StateChange::WindowInserted { window } => {
            Some(StateOp::WindowOpened(window_to_wire(window, mint)))
        }
        StateChange::WindowRemoved { id } => Some(StateOp::WindowClosed { id: *id }),
        StateChange::WindowUpdated { window } => {
            Some(StateOp::WindowOpened(window_to_wire(window, mint)))
        }
        StateChange::FocusChanged { focused: Some(id) } => Some(StateOp::WindowFocused { id: *id }),
        StateChange::FocusChanged { focused: None } => None,
        // Active is exclusive shell-side (applying active:true clears
        // the rest), so one op carries the switch.
        StateChange::ActiveWorkspaceChanged { active, .. } => {
            Some(StateOp::WorkspaceChanged(WorkspaceInfo {
                id: *active as u64,
                name: None,
                active: true,
            }))
        }
    }
}

/// Apply one shell command against `model`, gating `ActivateWindow` on the
/// injected token hook. Unknown ids are per-request denials
/// (`CommandResult::Denied`), not protocol errors. The validator sees the
/// target window's `app_id` from the model, so tokens bind to the window
/// they were minted for.
fn apply_command(
    model: &mut StateModel,
    validator: &dyn Fn(&ActivationToken, Option<&str>) -> bool,
    overview: &std::cell::Cell<bool>,
    kind: &CommandKind,
) -> CommandStatus {
    match kind {
        CommandKind::ActivateWindow { window, token } => {
            let Some(entry) = model.window(*window) else {
                return CommandStatus::Denied {
                    reason: "unknown window".to_owned(),
                };
            };
            if !validator(token, entry.app_id.as_deref()) {
                return CommandStatus::Denied {
                    reason: "activation token rejected".to_owned(),
                };
            }
            let _ = model.set_focused(Some(*window));
            CommandStatus::Applied
        }
        CommandKind::FocusWorkspace { workspace } => {
            // Dynamic workspaces: any representable id switches
            // (registering if new); focus lands on the tick's reconcile.
            let Ok(workspace) = u32::try_from(*workspace) else {
                return CommandStatus::Denied {
                    reason: "workspace id out of range".to_owned(),
                };
            };
            model.set_active_workspace(workspace);
            CommandStatus::Applied
        }
        CommandKind::ToggleOverview => {
            // Flip the hub-shared intent; the hub broadcasts the new
            // state as `Message::Overview` (002 R1).
            overview.set(!overview.get());
            CommandStatus::Applied
        }
    }
}

/// Our protocol version, for handshake replies.
pub fn our_version() -> ProtocolVersion {
    CURRENT_VERSION
}

/// Live control-plane driver: accepts shell connections, handshakes them
/// into [`Session`]s against one [`TokenStore`], emits deltas, and applies
/// commands — one nonblocking round per [`poll`](Self::poll), so slow or
/// dead shells never stall compositor input/frame paths (spec R6).
///
/// Sessions hold `'static` policy closures over an `Rc<TokenStore>`, so the
/// hub owns the store outright with no borrow of the runtime. Windows the
/// shell activated this round are returned for Wayland-side focus; the hub
/// only mutates the model.
pub struct ControlHub {
    listener: UnixListener,
    socket_path: std::path::PathBuf,
    sessions: Vec<Session<'static>>,
    /// Peers accepted in an earlier round: their `Hello` is due now.
    pending: Vec<UnixStream>,
    /// Peers accepted this round: handshake next round, once their
    /// `Hello` has had a full tick to arrive.
    fresh: Vec<UnixStream>,
    store: std::rc::Rc<TokenStore>,
    seat: String,
    /// Shell-facing overview intent (002 R1), shared with every live
    /// session: shell `ToggleOverview` commands and runtime triggers
    /// flip it, and [`poll`](Self::poll) broadcasts the result.
    overview: std::rc::Rc<std::cell::Cell<bool>>,
    /// Intent value last broadcast to all sessions; a mismatch means a
    /// flip is still owed (or a newcomer joined mid-state).
    overview_sent: bool,
    /// Alt-Tab drive events awaiting broadcast. Unlike the overview
    /// intent (level), steps are discrete events: each one is sent to
    /// every live session exactly once, retained until all sends land.
    switcher_queue: Vec<SwitcherAction>,
}

impl ControlHub {
    /// Bind `socket_path` (removing a stale file first) and start
    /// nonblocking. Fails fast when the path cannot be bound.
    pub fn bind(
        socket_path: std::path::PathBuf,
        store: std::rc::Rc<TokenStore>,
        seat: &str,
    ) -> std::io::Result<Self> {
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            socket_path,
            sessions: Vec::new(),
            pending: Vec::new(),
            fresh: Vec::new(),
            overview: std::rc::Rc::new(std::cell::Cell::new(false)),
            overview_sent: false,
            switcher_queue: Vec::new(),
            store,
            seat: seat.to_owned(),
        })
    }

    /// Bound control socket path (hand this to the shell child).
    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    /// Live session count, for diagnostics (never sensitive content).
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Shell-facing overview intent (002 R1).
    pub fn overview_open(&self) -> bool {
        self.overview.get()
    }

    /// Set the overview intent; the next [`poll`](Self::poll) broadcasts
    /// the flip to every live session as [`Message::Overview`].
    /// Idempotent: setting the current value sends nothing.
    pub fn set_overview(&self, open: bool) {
        self.overview.set(open);
    }

    /// Queue one Alt-Tab drive event; the next [`poll`](Self::poll)
    /// broadcasts it to every live session as [`Message::Switcher`].
    pub fn queue_switcher(&mut self, action: SwitcherAction) {
        self.switcher_queue.push(action);
    }

    /// One nonblocking round: accept waiting peers, advance pending
    /// handshakes, catch live sessions up, and apply one command frame
    /// per session. Returns the model ids of windows the shell activated
    /// this round, so the runtime can apply Wayland-side focus.
    pub fn poll(&mut self, model: &mut StateModel) -> Vec<u64> {
        while let Ok((stream, _)) = self.listener.accept() {
            let _ = stream.set_nonblocking(true);
            self.fresh.push(stream);
        }
        self.advance_pending(model);
        self.pending = std::mem::take(&mut self.fresh);
        let mut activated = Vec::new();
        let mut i = 0;
        while i < self.sessions.len() {
            let alive = {
                let session = &mut self.sessions[i];
                let before = model.focused();
                let deltas_ok = session.emit_deltas(model).is_ok();
                let cmd_ok = match session.handle_next(model) {
                    Ok(_) => true,
                    Err(ControlError::WouldBlock) => true,
                    Err(_) => false,
                };
                if deltas_ok && cmd_ok {
                    if model.focused() != before {
                        activated.extend(model.focused());
                    }
                    true
                } else {
                    false
                }
            };
            if alive {
                i += 1;
            } else {
                self.sessions.swap_remove(i);
            }
        }
        // Broadcast overview flips (002 R1): best-effort per session,
        // staying dirty until every live session holds the intent.
        if self.overview.get() != self.overview_sent {
            let open = self.overview.get();
            let mut all_sent = true;
            for session in &mut self.sessions {
                if session.send_overview(open).is_err() {
                    all_sent = false;
                }
            }
            if all_sent {
                self.overview_sent = open;
            }
        }
        // Broadcast switcher drive events (002 workspaces): each queued
        // action goes to every live session exactly once; actions that
        // miss a session stay queued for the next poll.
        if !self.switcher_queue.is_empty() {
            let pending = std::mem::take(&mut self.switcher_queue);
            let mut unsent = Vec::with_capacity(pending.len());
            for action in pending {
                let mut all_sent = true;
                for session in &mut self.sessions {
                    if session.send_switcher(action).is_err() {
                        all_sent = false;
                    }
                }
                if !all_sent {
                    unsent.push(action);
                }
            }
            self.switcher_queue = unsent;
        }
        activated
    }

    /// Try each aged peer's handshake once. Peers were accepted a full
    /// round ago, so a well-behaved peer's `Hello` is already waiting; a
    /// still-silent peer is dropped and must reconnect. One attempt per
    /// connection (`ControlConn` owns its stream, so a half-open
    /// handshake cannot be resumed) keeps silent peers from pinning slots.
    fn advance_pending(&mut self, model: &StateModel) {
        let pending = std::mem::take(&mut self.pending);
        for stream in pending {
            let conn = match ControlConn::new(stream) {
                Ok(conn) => conn,
                Err(_) => continue,
            };
            let validate = self.store.clone().validator(self.seat.clone());
            let validator =
                move |token: &ActivationToken, app_id: Option<&str>| validate(&token.0, app_id);
            let minter = std::rc::Rc::new(self.store.clone().minter(self.seat.clone()));
            let overview = self.overview.clone();
            let open = overview.get();
            if let Ok(mut session) =
                Session::handshake_with(conn, model, validator, minter, overview)
            {
                // Newcomers join mid-state: tell them the intent now
                // (best-effort; the poll broadcast covers the rest).
                let _ = session.send_overview(open);
                self.sessions.push(session);
            }
        }
    }
}

impl Drop for ControlHub {
    /// Best-effort socket cleanup; a stale file would block the next bind
    /// (which also removes it first).
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}
