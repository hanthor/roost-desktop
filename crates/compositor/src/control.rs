//! Compositor-side control channel (001 R4/R6, ADR 0002).
//!
//! Private local IPC between the compositor (authoritative) and the shell
//! host. Wire types and framing live in the `rwd-shell-control` crate
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
//! a caller-supplied `Fn(&ActivationToken) -> bool` so policy stays
//! injectable. The default ([`deny_all_tokens`]) denies everything
//! (fail-closed); production policy (30 s expiry, one-use, seat binding,
//! `app_id` match) is compositor policy code per ADR 0002, not protocol
//! behavior.
//!
//! Reference patterns (framing/versioning discipline, snapshot-on-connect)
//! follow the niri survey in `.spektacular/work/wave2/stream1-notes.md`;
//! all code below is original.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

use rwd_shell_control::{
    decode_frame, encode_frame, ActivationToken, CommandKind, CommandStatus, DecodeError,
    ErrorKind, Message, ProtocolVersion, StateOp, WorkspaceInfo, CURRENT_VERSION, MAX_FRAME_BYTES,
};

use crate::state::{StateChange, StateModel, WindowEntry};

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
/// binding, `app_id` match — compositor policy code, injected here).
pub fn deny_all_tokens(_: &ActivationToken) -> bool {
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

/// Authenticated control session over a [`ControlConn`].
///
/// The session owns the connection, the token-check hook, and the last
/// revision the shell is known to hold. The [`StateModel`] itself is passed
/// per call (`&mut` where commands may mutate, `&` otherwise) so callers can
/// drive the model between session calls. Build with [`handshake`](Self::handshake)
/// (fail-closed tokens) or [`handshake_with`](Self::handshake_with) (custom
/// policy); the handshake sends our `Hello` plus a full snapshot, so every
/// (re)connect starts from complete state.
pub struct Session<'a> {
    conn: ControlConn,
    validator: Box<dyn Fn(&ActivationToken) -> bool + 'a>,
    last_revision: u64,
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
        Self::handshake_with(conn, model, deny_all_tokens)
    }

    /// [`handshake`](Self::handshake) with an injectable activation-token
    /// policy for `ActivateWindow` commands.
    pub fn handshake_with(
        conn: ControlConn,
        model: &StateModel,
        validator: impl Fn(&ActivationToken) -> bool + 'a,
    ) -> Result<Self, ControlError> {
        let mut session = Self {
            conn,
            validator: Box::new(validator),
            last_revision: 0,
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
    /// gap paths call this internally).
    pub fn send_snapshot(&mut self, model: &StateModel) -> Result<u64, ControlError> {
        let revision = model.revision();
        self.conn.write_frame(&snapshot_message(model))?;
        self.last_revision = revision;
        Ok(revision)
    }

    /// Catch the shell up from [`last_revision`](Self::last_revision):
    /// incremental `Changes` when the change log covers the gap, else a
    /// fresh snapshot (resnapshot rule).
    pub fn emit_deltas(&mut self, model: &StateModel) -> Result<Emitted, ControlError> {
        match model.changes_since(self.last_revision) {
            Ok(entries) => {
                let ops: Vec<StateOp> = entries.iter().filter_map(|(_, c)| state_op(c)).collect();
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
                let status = apply_command(model, &self.validator, &kind);
                let applied = matches!(status, CommandStatus::Applied);
                self.conn
                    .write_frame(&Message::CommandResult { id, status })?;
                Ok(Handled::CommandResult { id, applied })
            }
            Message::Snapshot { .. }
            | Message::Changes { .. }
            | Message::CommandResult { .. }
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
    }
}

/// Build the full-snapshot message for `model`.
fn snapshot_message(model: &StateModel) -> Message {
    let snap = model.snapshot();
    Message::Snapshot {
        revision: snap.revision,
        windows: snap.windows.iter().map(window_to_wire).collect(),
        workspaces: snap
            .workspaces
            .iter()
            .map(|id| WorkspaceInfo {
                id: *id as u64,
                name: None,
                active: false,
            })
            .collect(),
    }
}

/// Map one model window to its wire mirror (`app_id` is unknown to the
/// [`StateModel`], so it is always `None`).
fn window_to_wire(w: &WindowEntry) -> rwd_shell_control::WindowInfo {
    rwd_shell_control::WindowInfo {
        id: w.id,
        title: w.title.clone(),
        app_id: None,
        workspace: w.workspace as u64,
        focused: w.focused,
    }
}

/// Map one model change to its wire op. Clearing focus has no wire
/// counterpart (focus is carried on the window entries and the
/// `WindowFocused` op), so it maps to `None` and only advances the cursor.
fn state_op(change: &StateChange) -> Option<StateOp> {
    match change {
        StateChange::WindowInserted { window } => {
            Some(StateOp::WindowOpened(window_to_wire(window)))
        }
        StateChange::WindowRemoved { id } => Some(StateOp::WindowClosed { id: *id }),
        StateChange::WindowUpdated { window } => {
            Some(StateOp::WindowOpened(window_to_wire(window)))
        }
        StateChange::FocusChanged { focused: Some(id) } => Some(StateOp::WindowFocused { id: *id }),
        StateChange::FocusChanged { focused: None } => None,
    }
}

/// Apply one shell command against `model`, gating `ActivateWindow` on the
/// injected token hook. Unknown ids are per-request denials
/// (`CommandResult::Denied`), not protocol errors.
fn apply_command(
    model: &mut StateModel,
    validator: &dyn Fn(&ActivationToken) -> bool,
    kind: &CommandKind,
) -> CommandStatus {
    match kind {
        CommandKind::ActivateWindow { window, token } => {
            if !validator(token) {
                return CommandStatus::Denied {
                    reason: "activation token rejected".to_owned(),
                };
            }
            if model.window(*window).is_none() {
                return CommandStatus::Denied {
                    reason: "unknown window".to_owned(),
                };
            }
            let _ = model.set_focused(Some(*window));
            CommandStatus::Applied
        }
        CommandKind::FocusWorkspace { workspace } => {
            // The R4 model tracks workspace membership but no active
            // workspace, so a known workspace is accepted without mutation.
            let known = u32::try_from(*workspace)
                .map(|ws| model.workspaces().contains(&ws))
                .unwrap_or(false);
            if known {
                CommandStatus::Applied
            } else {
                CommandStatus::Denied {
                    reason: "unknown workspace".to_owned(),
                }
            }
        }
        CommandKind::ToggleOverview => CommandStatus::Applied,
    }
}

/// Our protocol version, for handshake replies.
pub fn our_version() -> ProtocolVersion {
    CURRENT_VERSION
}
