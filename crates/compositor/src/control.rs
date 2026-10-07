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
    ErrorKind, Message, OutputInfo, ProtocolVersion, StateOp, SwitcherAction, WorkspaceInfo,
    CURRENT_VERSION, MAX_FRAME_BYTES,
};

use crate::state::{StateChange, StateModel, TokenStore, WindowEntry};

/// Transport and protocol failures, shared with the shell-host endpoint.
///
/// Defined in [`roost_shell_control`] so both endpoints name the same type;
/// re-exported here because `roost_compositor::control::ControlError` is the
/// path the rest of this crate (and its tests) use.
pub use roost_shell_control::ControlError;

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
                        return Err(ControlError::Unexpected("peer closed".into()));
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
                Ok(0) => return Err(ControlError::Unexpected("peer closed on write".into())),
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
    /// Compositor-owned session-lock flag shared with the hub: the
    /// runtime (idle timeout) and `Lock` commands flip it, and every
    /// snapshot/delta path reads it. The flag lives here in the
    /// compositor, never in the restartable shell.
    locked: std::rc::Rc<std::cell::Cell<bool>>,
    /// Lock value the shell last received. A flip forces a fresh
    /// snapshot (resnapshot rule for lock transitions): locking strips
    /// window content, unlocking restores it.
    locked_sent: bool,
    peer_minor: u16,
    /// Window ids the shell asked to close this round (`CloseWindow`
    /// commands the mirror applied). Drained by [`ControlHub::poll`]
    /// so the runtime can send the polite client close.
    closed: Vec<u64>,
    /// Latest `SetIdleTimeout` from the shell, drained by the hub.
    idle_timeout: Option<u64>,
    /// The latest SetAccelerators the shell sent, until drained.
    accelerators: Option<Vec<roost_shell_control::Accelerator>>,
    /// Window-menu actions the shell asked for, until drained.
    window_actions: Vec<(u64, roost_shell_control::WindowAction)>,
    shortcut_consent: Vec<(u64, bool)>,
    /// Latest `SetOverviewSearch` from the shell, drained by the hub.
    overview_search: Option<bool>,
    /// Latest `SetOverviewAppGrid` from the shell, drained by the hub.
    overview_app_grid: Option<bool>,
    /// A lock-screen password awaiting verification, with its request
    /// id: the reply waits for the result (see `ControlHub::finish_unlock`).
    unlock_request: Option<(u64, roost_shell_control::Secret)>,
    /// The request id whose `CommandResult` is still owed.
    unlock_pending: Option<u64>,
    /// Latest `SetInputSettings` from the shell, drained by the hub.
    input_settings: Option<roost_shell_control::InputSettings>,
    screen_reader: Option<bool>,
    /// Input-source switches the shell asked for (`true` backward).
    input_source_switches: Vec<bool>,
    /// The switcher's thumbnail frames, when the shell sent new ones.
    switcher_thumbnails: Option<Vec<roost_shell_control::SwitcherThumbnail>>,
    /// The shell's switcher keys, until the runtime drains them.
    switcher_keys: Option<Vec<roost_shell_control::SwitcherKey>>,
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
    /// `ToggleOverview` commands flip state the hub broadcasts; the
    /// shared `locked` cell joins the hub's session-lock flag so every
    /// (re)connect snapshot carries the current lock state (a shell
    /// restart while locked stays locked).
    pub fn handshake_with(
        conn: ControlConn,
        model: &StateModel,
        validator: impl Fn(&ActivationToken, Option<&str>) -> bool + 'a,
        minter: TokenMinter,
        overview: std::rc::Rc<std::cell::Cell<bool>>,
        locked: std::rc::Rc<std::cell::Cell<bool>>,
    ) -> Result<Self, ControlError> {
        let locked_sent = locked.get();
        let mut session = Self {
            conn,
            validator: Box::new(validator),
            minter,
            last_revision: 0,
            overview,
            locked,
            locked_sent,
            peer_minor: 0,
            closed: Vec::new(),
            idle_timeout: None,
            accelerators: None,
            window_actions: Vec::new(),
            shortcut_consent: Vec::new(),
            overview_search: None,
            overview_app_grid: None,
            unlock_request: None,
            unlock_pending: None,
            input_settings: None,
            screen_reader: None,
            input_source_switches: Vec::new(),
            switcher_thumbnails: None,
            switcher_keys: None,
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
            return Err(ControlError::Unexpected(
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
        session.peer_minor = version.minor;
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
    /// minted activation token from this session's minter. Carries the
    /// session-lock flag (stripping window content while locked) and
    /// records it as sent for the lock-transition resnapshot rule.
    pub fn send_snapshot(&mut self, model: &StateModel) -> Result<u64, ControlError> {
        let revision = model.revision();
        let locked = self.locked.get();
        self.conn
            .write_frame(&snapshot_message(model, &*self.minter, locked))?;
        self.last_revision = revision;
        self.locked_sent = locked;
        Ok(revision)
    }

    /// Send the current overview intent (002 R1). Best-effort like the
    /// rest of the nonblocking plane: a `WouldBlock` caller keeps the
    /// hub dirty flag set and retries next round.
    pub fn send_overview(&mut self, open: bool) -> Result<(), ControlError> {
        self.conn.write_frame(&Message::Overview { open })
    }

    /// Send the current output inventory (multi-monitor). Best-effort
    /// like overview: the hub rebroadcasts until every live session
    /// holds it, and newcomers get it right after the handshake.
    pub fn send_native_outputs(
        &mut self,
        outputs: &[roost_shell_control::NativeOutputInfo],
    ) -> Result<(), ControlError> {
        if self.peer_minor < 29 {
            return Ok(());
        }
        self.conn.write_frame(&Message::NativeOutputInventory {
            outputs: outputs.to_vec(),
        })
    }

    pub fn send_outputs(&mut self, outputs: &[OutputInfo]) -> Result<(), ControlError> {
        self.conn.write_frame(&Message::Outputs {
            outputs: outputs.to_vec(),
        })
    }

    /// The latest `SetIdleTimeout` the shell sent, once (#63).
    pub fn take_idle_timeout(&mut self) -> Option<u64> {
        self.idle_timeout.take()
    }

    /// Window-menu actions the shell sent since the last call.
    pub fn take_window_actions(&mut self) -> Vec<(u64, roost_shell_control::WindowAction)> {
        std::mem::take(&mut self.window_actions)
    }

    /// Ask the shell to draw GNOME's window menu.
    pub fn send_window_menu(&mut self, menu: &Message) -> Result<(), ControlError> {
        self.conn.write_frame(menu)
    }

    /// The latest `SetAccelerators` the shell sent, once.
    pub fn take_accelerators(&mut self) -> Option<Vec<roost_shell_control::Accelerator>> {
        self.accelerators.take()
    }

    /// Report a grabbed accelerator's press.
    pub fn send_accelerator(
        &mut self,
        action: u32,
        time: u32,
        mode: u32,
    ) -> Result<(), ControlError> {
        self.conn
            .write_frame(&Message::AcceleratorActivated { action, time, mode })
    }

    /// Send where the overview's window previews sit.
    pub fn send_overview_previews(
        &mut self,
        previews: &[roost_shell_control::PreviewInfo],
        hovered: Option<u64>,
    ) -> Result<(), ControlError> {
        self.conn.write_frame(&Message::OverviewPreviews {
            previews: previews.to_vec(),
            hovered,
        })
    }

    /// Send the session environment (#59).
    pub fn send_environment(&mut self, vars: &[(String, String)]) -> Result<(), ControlError> {
        self.conn.write_frame(&Message::Environment {
            vars: vars.to_vec(),
        })
    }

    /// Send one Alt-Tab switcher drive event (002 workspaces).
    pub fn send_switcher(&mut self, action: SwitcherAction) -> Result<(), ControlError> {
        self.conn.write_frame(&Message::Switcher { action })
    }

    /// Catch the shell up from [`last_revision`](Self::last_revision):
    /// incremental `Changes` when the change log covers the gap, else a
    /// fresh snapshot (resnapshot rule). New token-bearing entries mint
    /// through this session's minter. A lock transition since the last
    /// send forces a fresh snapshot (locking strips window content,
    /// unlocking restores it); while locked with no transition, deltas
    /// are suppressed (never leak titles) and only the cursor advances.
    pub fn emit_deltas(&mut self, model: &StateModel) -> Result<Emitted, ControlError> {
        if self.locked.get() != self.locked_sent {
            let revision = self.send_snapshot(model)?;
            return Ok(Emitted::Snapshot { revision });
        }
        if self.locked.get() {
            let to = model.revision();
            self.last_revision = to;
            return Ok(Emitted::Idle { revision: to });
        }
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
            Message::Command {
                id,
                kind: CommandKind::SetScreenReader { enabled },
            } => {
                self.screen_reader = Some(enabled);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetInputSettings(settings),
            } => {
                // Session-level settings for the runtime (seat, libinput).
                self.input_settings = Some(settings);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetSwitcherThumbnails { thumbnails },
            } => {
                self.switcher_thumbnails = Some(thumbnails);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetSwitcherKeys { keys },
            } => {
                self.switcher_keys = Some(keys);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SwitchInputSource { backward },
            } => {
                self.input_source_switches.push(backward);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::Unlock { password },
            } => {
                // Verified off the event loop by the runtime; the result
                // goes back as this id's CommandResult.
                self.unlock_request = Some((id, password));
                self.unlock_pending = Some(id);
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetOverviewAppGrid { active },
            } => {
                // UI state for the runtime's overview drawing.
                self.overview_app_grid = Some(active);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetOverviewSearch { active },
            } => {
                // UI state for the runtime's overview drawing.
                self.overview_search = Some(active);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::ShortcutConsent { request, allow },
            } => {
                if !self.locked.get() {
                    self.shortcut_consent.push((request, allow));
                }
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::WindowAction { window, action },
            } => {
                // The window manager carries it out (drained by the hub);
                // unknown windows are denied here, like CloseWindow.
                let known = model.windows().any(|w| w.id == window);
                let status = if known {
                    self.window_actions.push((window, action));
                    CommandStatus::Applied
                } else {
                    CommandStatus::Denied {
                        reason: format!("unknown window {window}"),
                    }
                };
                let applied = matches!(status, CommandStatus::Applied);
                self.conn
                    .write_frame(&Message::CommandResult { id, status })?;
                Ok(Handled::CommandResult { id, applied })
            }
            Message::Command {
                id,
                kind: CommandKind::SetAccelerators { accelerators },
            } => {
                // Session-level grabs, applied by the window manager's
                // key filter (drained by the hub).
                self.accelerators = Some(accelerators);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetIdleTimeout { ms },
            } => {
                // Session-level setting, not model state: the runtime
                // applies it to the idle lock (drained by the hub).
                self.idle_timeout = Some(ms);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command { id, kind } => {
                let (status, closed) =
                    apply_command(model, &self.validator, &self.overview, &self.locked, &kind);
                if let Some(window) = closed {
                    self.closed.push(window);
                }
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
            | Message::NativeOutputInventory { .. }
            | Message::Outputs { .. }
            | Message::Environment { .. }
            | Message::OverviewPreviews { .. }
            | Message::AcceleratorActivated { .. }
            | Message::WindowMenu { .. }
            | Message::WorkspacePopup { .. }
            | Message::PointerOutput { .. }
            | Message::ScreenReader { .. }
            | Message::ShortcutConsent { .. }
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
        | DecodeError::CollectionTooLong { .. }
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
        Message::NativeOutputInventory { .. } => "NativeOutputInventory",
        Message::Outputs { .. } => "Outputs",
        Message::Environment { .. } => "Environment",
        Message::OverviewPreviews { .. } => "OverviewPreviews",
        Message::AcceleratorActivated { .. } => "AcceleratorActivated",
        Message::WindowMenu { .. } => "WindowMenu",
        Message::WorkspacePopup { .. } => "WorkspacePopup",
        Message::PointerOutput { .. } => "PointerOutput",
        Message::ShortcutConsent { .. } => "ShortcutConsent",
        Message::ScreenReader { .. } => "ScreenReader",
    }
}

/// Build the full-snapshot message for `model`, minting one activation
/// token per window through `mint`. While `locked` the snapshot carries
/// the flag with no window content (empty list): the lock screen shows
/// no titles, and the restartable shell must not retain any.
/// Workspaces are structural ids only and pass through unchanged.
fn snapshot_message(
    model: &StateModel,
    mint: &dyn Fn(Option<&str>) -> String,
    locked: bool,
) -> Message {
    let snap = model.snapshot();
    Message::Snapshot {
        revision: snap.revision,
        windows: if locked {
            Vec::new()
        } else {
            snap.windows
                .iter()
                .map(|w| window_to_wire(w, mint))
                .collect()
        },
        workspaces: snap
            .workspaces
            .iter()
            .map(|id| WorkspaceInfo {
                id: *id as u64,
                name: None,
                active: *id == snap.active,
            })
            .collect(),
        locked,
    }
}

/// Map one model window to its wire mirror, minting its activation token
/// through `mint` (bound to the window's `app_id`).
fn window_to_wire(
    w: &WindowEntry,
    mint: &dyn Fn(Option<&str>) -> String,
) -> roost_shell_control::WindowInfo {
    roost_shell_control::WindowInfo {
        icon: w.icon.clone(),
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
///
/// Returns the status plus the closed window id when a `CloseWindow`
/// applied: the hub drains those so the runtime can send the polite
/// client close (the mirror keeps the window until the client unmaps).
fn apply_command(
    model: &mut StateModel,
    validator: &dyn Fn(&ActivationToken, Option<&str>) -> bool,
    overview: &std::cell::Cell<bool>,
    locked: &std::cell::Cell<bool>,
    kind: &CommandKind,
) -> (CommandStatus, Option<u64>) {
    match kind {
        CommandKind::ActivateWindow { window, token } => {
            let Some(entry) = model.window(*window) else {
                return (
                    CommandStatus::Denied {
                        reason: "unknown window".to_owned(),
                    },
                    None,
                );
            };
            if !validator(token, entry.app_id.as_deref()) {
                return (
                    CommandStatus::Denied {
                        reason: "activation token rejected".to_owned(),
                    },
                    None,
                );
            }
            let _ = model.set_focused(Some(*window));
            (CommandStatus::Applied, None)
        }
        CommandKind::FocusWorkspace { workspace } => {
            // Dynamic workspaces: any representable id switches
            // (registering if new); focus lands on the tick's reconcile.
            let Ok(workspace) = u32::try_from(*workspace) else {
                return (
                    CommandStatus::Denied {
                        reason: "workspace id out of range".to_owned(),
                    },
                    None,
                );
            };
            model.set_active_workspace(workspace);
            (CommandStatus::Applied, None)
        }
        CommandKind::ToggleOverview => {
            // Flip the hub-shared intent; the hub broadcasts the new
            // state as `Message::Overview` (002 R1).
            overview.set(!overview.get());
            (CommandStatus::Applied, None)
        }
        CommandKind::CloseWindow { window } => {
            if model.window(*window).is_none() {
                return (
                    CommandStatus::Denied {
                        reason: "unknown window".to_owned(),
                    },
                    None,
                );
            }
            (CommandStatus::Applied, Some(*window))
        }
        // Intercepted by the session before it gets here.
        CommandKind::SetIdleTimeout { .. }
        | CommandKind::SetOverviewSearch { .. }
        | CommandKind::SetOverviewAppGrid { .. }
        | CommandKind::Unlock { .. }
        | CommandKind::SetAccelerators { .. }
        | CommandKind::WindowAction { .. }
        | CommandKind::ShortcutConsent { .. }
        | CommandKind::SwitchInputSource { .. }
        | CommandKind::SetSwitcherThumbnails { .. }
        | CommandKind::SetSwitcherKeys { .. }
        | CommandKind::SetScreenReader { .. }
        | CommandKind::SetInputSettings(_) => (CommandStatus::Applied, None),
        CommandKind::Lock => {
            // Manual lock from the shell (session-lock set path):
            // engage the compositor-owned flag; idempotent, always
            // applied. The next poll snapshots the stripped state.
            locked.set(true);
            (CommandStatus::Applied, None)
        }
    }
}

/// Our protocol version, for handshake replies.
pub fn our_version() -> ProtocolVersion {
    CURRENT_VERSION
}

/// Outcome of one [`ControlHub::poll`] round: model ids the shell
/// activated (runtime applies Wayland-side focus) and asked to close
/// (runtime sends the polite client close).
#[derive(Debug, Default)]
pub struct PollOutcome {
    /// Windows to focus and raise.
    pub activated: Vec<u64>,
    /// Windows to ask to close.
    pub closed: Vec<u64>,
    /// Idle-lock timeout the shell asked for (`0` never), if any.
    pub idle_timeout: Option<u64>,
    /// Whether overview search is showing results, if the shell said.
    pub overview_search: Option<bool>,
    /// Whether the overview shows the app grid, if the shell said.
    pub overview_app_grid: Option<bool>,
    /// A lock-screen password to verify, with its request id.
    pub unlock: Option<(u64, roost_shell_control::Secret)>,
    /// GNOME input settings the shell sent, if any.
    pub input_settings: Option<roost_shell_control::InputSettings>,
    pub screen_reader: Option<bool>,
    /// Accelerator grabs the shell sent, if they changed.
    pub accelerators: Option<Vec<roost_shell_control::Accelerator>>,
    /// Window-menu actions to carry out.
    pub window_actions: Vec<(u64, roost_shell_control::WindowAction)>,
    /// Trusted shell decisions for pending inhibitors.
    pub shortcut_consent: Vec<(u64, bool)>,
    /// Input-source switches to make (`true` backward), in order.
    pub input_source_switches: Vec<bool>,
    /// New switcher thumbnail frames, if the shell sent any.
    pub switcher_thumbnails: Option<Vec<roost_shell_control::SwitcherThumbnail>>,
    /// The shell's switcher keys, if it sent new ones.
    pub switcher_keys: Option<Vec<roost_shell_control::SwitcherKey>>,
}

/// Peers accepted but not yet handshaken are capped so a same-user
/// connection flood cannot grow memory or starve the real shell (#30).
pub const MAX_PENDING_PEERS: usize = 8;

/// Kernel-reported credentials of a connected control peer
/// (`SO_PEERCRED`): what the peer *is*, not what it claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerCred {
    /// Peer process id at connect time.
    pub pid: u32,
    /// Peer effective user id.
    pub uid: u32,
}

/// Who may hold a control session (#30).
///
/// The control socket is a privileged channel: a session can focus and
/// close windows, drive the overview, and engage the lock. Filesystem
/// permissions keep other users out; this gate also keeps other
/// same-user processes out once the compositor supervises its shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerGate {
    /// Any peer running as our user (tests, unsupervised development).
    SameUser,
    /// Only the supervised shell process with this pid, as our user.
    Pid(u32),
    /// Nobody: no shell is running (between restarts, or none spawned).
    Closed,
}

impl PeerGate {
    /// Whether a peer with `peer` credentials may hold a session.
    /// Unknown credentials are refused: fail closed.
    pub fn admits(self, peer: Option<PeerCred>, our_uid: u32) -> bool {
        let Some(peer) = peer else {
            return false;
        };
        if peer.uid != our_uid {
            return false;
        }
        match self {
            Self::SameUser => true,
            Self::Pid(pid) => peer.pid == pid,
            Self::Closed => false,
        }
    }
}

/// Read the kernel credentials of a connected Unix stream peer.
pub fn peer_cred(stream: &UnixStream) -> Option<PeerCred> {
    let cred = rustix::net::sockopt::socket_peercred(stream).ok()?;
    Some(PeerCred {
        pid: u32::try_from(cred.pid.as_raw_nonzero().get()).ok()?,
        uid: cred.uid.as_raw(),
    })
}

/// Our effective uid, for [`PeerGate::admits`].
pub fn our_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Whether `dir` is a directory owned by us with no group or other
/// permission bits: the only place a privileged socket may live.
pub fn is_private_dir(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir)
        .map(|meta| meta.is_dir() && meta.uid() == our_uid() && meta.mode() & 0o077 == 0)
        .unwrap_or(false)
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
    /// Compositor-owned session-lock flag (session-lock): flipped by
    /// the runtime (idle timeout) or `Lock` commands, shared with every
    /// live session so snapshots carry it; the shell never owns it.
    locked: std::rc::Rc<std::cell::Cell<bool>>,
    /// Alt-Tab drive events awaiting broadcast. Unlike the overview
    /// intent (level), steps are discrete events: each one is sent to
    /// every live session exactly once, retained until all sends land.
    switcher_queue: Vec<SwitcherAction>,
    /// Accelerator presses waiting for the next poll.
    accelerator_queue: Vec<(u32, u32, u32)>,
    /// Window-menu requests waiting for the next poll.
    menu_queue: Vec<Message>,
    /// Output inventory last handed to [`set_outputs`](Self::set_outputs)
    /// (multi-monitor): the runtime refreshes this every tick from the
    /// compositor's tracking, and [`poll`](Self::poll) broadcasts it
    /// whenever it differs from `outputs_sent`.
    outputs: Vec<OutputInfo>,
    native_outputs: Vec<roost_shell_control::NativeOutputInfo>,
    pointer_output: Option<String>,
    /// Inventory value every live session holds; a mismatch means a
    /// broadcast is still owed (or a newcomer joined mid-state).
    outputs_sent: Vec<OutputInfo>,
    native_outputs_sent: Vec<roost_shell_control::NativeOutputInfo>,
    /// Session environment for launched apps (#59), and the last value
    /// every live session holds.
    environment: Vec<(String, String)>,
    environment_sent: Vec<(String, String)>,
    /// Overview previews for the shell's chrome, and the last value
    /// every session holds.
    previews: (Vec<roost_shell_control::PreviewInfo>, Option<u64>),
    previews_sent: (Vec<roost_shell_control::PreviewInfo>, Option<u64>),
    /// Peer admission rule (#30); the runtime narrows it to the
    /// supervised shell's pid every tick.
    gate: PeerGate,
    /// Peer pid per live session, index-aligned with `sessions`, so a
    /// gate change evicts sessions the new gate no longer admits.
    session_peers: Vec<Option<PeerCred>>,
    /// Peer credentials of `pending` / `fresh`, index-aligned.
    pending_peers: Vec<Option<PeerCred>>,
    fresh_peers: Vec<Option<PeerCred>>,
    /// Connections refused by the gate or the pending cap, for
    /// diagnostics (a count only; never peer details).
    refused: u64,
}

impl ControlHub {
    /// Bind `socket_path` (removing a stale file first) and start
    /// nonblocking. Fails fast when the path cannot be bound, or when
    /// its directory is not private to us (#30: never a shared dir such
    /// as `/tmp`). The socket file itself is made owner-only.
    pub fn bind(
        socket_path: std::path::PathBuf,
        store: std::rc::Rc<TokenStore>,
        seat: &str,
    ) -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let parent = socket_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| std::io::Error::other("control socket path has no directory"))?;
        if !is_private_dir(parent) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "control socket directory is not private to this user",
            ));
        }
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            socket_path,
            sessions: Vec::new(),
            pending: Vec::new(),
            fresh: Vec::new(),
            overview: std::rc::Rc::new(std::cell::Cell::new(false)),
            overview_sent: false,
            locked: std::rc::Rc::new(std::cell::Cell::new(false)),
            switcher_queue: Vec::new(),
            accelerator_queue: Vec::new(),
            menu_queue: Vec::new(),
            outputs: Vec::new(),
            native_outputs: Vec::new(),
            pointer_output: None,
            outputs_sent: Vec::new(),
            native_outputs_sent: Vec::new(),
            environment: Vec::new(),
            environment_sent: Vec::new(),
            previews: (Vec::new(), None),
            previews_sent: (Vec::new(), None),
            store,
            seat: seat.to_owned(),
            gate: PeerGate::SameUser,
            session_peers: Vec::new(),
            pending_peers: Vec::new(),
            fresh_peers: Vec::new(),
            refused: 0,
        })
    }

    /// Narrow (or widen) who may hold a session. Live sessions the new
    /// gate refuses are dropped on the next [`poll`](Self::poll).
    pub fn set_peer_gate(&mut self, gate: PeerGate) {
        self.gate = gate;
    }

    /// Current admission rule.
    pub fn peer_gate(&self) -> PeerGate {
        self.gate
    }

    /// Connections refused so far (gate or pending cap).
    pub fn refused_count(&self) -> u64 {
        self.refused
    }

    /// Accepted peers still waiting for their handshake round.
    pub fn pending_count(&self) -> usize {
        self.pending.len() + self.fresh.len()
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

    /// Refresh the output inventory; the next [`poll`](Self::poll)
    /// broadcasts it to every live session as [`Message::Outputs`]
    /// when it differs from the last broadcast. Idempotent: setting
    /// the current value sends nothing. The runtime calls this every
    /// tick from the compositor's tracking — the single inventory the
    /// spec requires, never a parallel database.
    pub fn set_native_outputs(&mut self, outputs: Vec<roost_shell_control::NativeOutputInfo>) {
        self.native_outputs = outputs;
    }

    pub fn set_outputs(&mut self, outputs: Vec<OutputInfo>) {
        self.outputs = outputs;
    }

    /// Set the session environment (#59); the next [`poll`](Self::poll)
    /// broadcasts it when it differs from the last broadcast, and every
    /// newcomer gets it right after the handshake.
    /// Where the overview's previews sit and which one is hovered; sent
    /// to the shell when it changes (empty while the overview is closed).
    pub fn set_overview_previews(
        &mut self,
        previews: Vec<roost_shell_control::PreviewInfo>,
        hovered: Option<u64>,
    ) {
        self.previews = (previews, hovered);
    }

    /// Answer a lock-screen password: `Applied` when it unlocked the
    /// session, `Denied` when it did not.
    pub fn finish_unlock(&mut self, request: u64, unlocked: bool) {
        let status = if unlocked {
            CommandStatus::Applied
        } else {
            CommandStatus::Denied {
                reason: "authentication failed".to_owned(),
            }
        };
        for session in &mut self.sessions {
            if session.unlock_pending == Some(request) {
                session.unlock_pending = None;
                let _ = session.conn.write_frame(&Message::CommandResult {
                    id: request,
                    status: status.clone(),
                });
            }
        }
    }

    pub fn set_environment(&mut self, mut vars: Vec<(String, String)>) {
        vars.sort();
        self.environment = vars;
    }

    /// Whether the session is locked (compositor-owned flag).
    pub fn is_locked(&self) -> bool {
        self.locked.get()
    }

    /// Set the session-lock flag; the next [`poll`](Self::poll) carries
    /// it to every live session as a (possibly content-stripped)
    /// [`Message::Snapshot`]. Idempotent. Unlock clears through session
    /// auth (see [`crate::unlock`]), never through this path from the
    /// shell.
    pub fn set_locked(&self, locked: bool) {
        self.locked.set(locked);
    }

    /// Send a compositor-to-shell message (window menu, workspace popup)
    /// on the next poll.
    pub fn set_pointer_output(&mut self, name: Option<String>) {
        if self.pointer_output != name {
            self.pointer_output = name.clone();
            self.queue_message(Message::PointerOutput { name });
        }
    }

    pub fn queue_message(&mut self, message: Message) {
        self.menu_queue.push(message);
    }

    /// Report a grabbed accelerator's press to the shell on the next poll.
    pub fn queue_accelerator(&mut self, action: u32, time: u32, mode: u32) {
        self.accelerator_queue.push((action, time, mode));
    }

    /// Queue one Alt-Tab drive event; the next [`poll`](Self::poll)
    /// broadcasts it to every live session as [`Message::Switcher`].
    pub fn queue_switcher(&mut self, action: SwitcherAction) {
        self.switcher_queue.push(action);
    }

    /// Drain one session's applied close requests (called by
    /// [`ControlHub::poll`]).
    fn take_closed(session: &mut Session<'_>) -> Vec<u64> {
        std::mem::take(&mut session.closed)
    }

    /// One nonblocking round: accept waiting peers, advance pending
    /// handshakes, catch live sessions up, and apply one command frame
    /// per session. Returns the model ids of windows the shell activated
    /// this round (for Wayland-side focus) and asked to close (for the
    /// polite client close), so the runtime can apply both.
    pub fn poll(&mut self, model: &mut StateModel) -> PollOutcome {
        let uid = our_uid();
        while let Ok((stream, _)) = self.listener.accept() {
            let cred = peer_cred(&stream);
            if !self.gate.admits(cred, uid) || self.pending_count() >= MAX_PENDING_PEERS {
                // Dropping the stream closes it: the peer sees EOF
                // before any state crosses the socket.
                self.refused += 1;
                continue;
            }
            let _ = stream.set_nonblocking(true);
            self.fresh.push(stream);
            self.fresh_peers.push(cred);
        }
        // A narrowed gate evicts sessions it no longer admits.
        let mut i = 0;
        while i < self.sessions.len() {
            if self.gate.admits(self.session_peers[i], uid) {
                i += 1;
            } else {
                self.sessions.swap_remove(i);
                self.session_peers.swap_remove(i);
            }
        }
        self.advance_pending(model);
        self.pending = std::mem::take(&mut self.fresh);
        self.pending_peers = std::mem::take(&mut self.fresh_peers);
        let mut outcome = PollOutcome::default();
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
                        outcome.activated.extend(model.focused());
                    }
                    outcome.closed.extend(Self::take_closed(session));
                    if let Some(ms) = session.take_idle_timeout() {
                        outcome.idle_timeout = Some(ms);
                    }
                    if let Some(list) = session.take_accelerators() {
                        outcome.accelerators = Some(list);
                    }
                    outcome.window_actions.extend(session.take_window_actions());
                    outcome
                        .shortcut_consent
                        .extend(std::mem::take(&mut session.shortcut_consent));
                    outcome
                        .input_source_switches
                        .extend(std::mem::take(&mut session.input_source_switches));
                    if let Some(thumbnails) = session.switcher_thumbnails.take() {
                        outcome.switcher_thumbnails = Some(thumbnails);
                    }
                    if let Some(keys) = session.switcher_keys.take() {
                        outcome.switcher_keys = Some(keys);
                    }
                    if let Some(active) = session.overview_search.take() {
                        outcome.overview_search = Some(active);
                    }
                    if let Some(active) = session.overview_app_grid.take() {
                        outcome.overview_app_grid = Some(active);
                    }
                    if let Some(request) = session.unlock_request.take() {
                        outcome.unlock = Some(request);
                    }
                    if let Some(settings) = session.input_settings.take() {
                        outcome.input_settings = Some(settings);
                    }
                    if let Some(enabled) = session.screen_reader.take() {
                        outcome.screen_reader = Some(enabled);
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
                self.session_peers.swap_remove(i);
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
        // Broadcast the output inventory (multi-monitor): best-effort
        // per session like overview, staying dirty until every live
        // session holds it.
        if self.environment != self.environment_sent {
            let mut all_sent = true;
            for session in &mut self.sessions {
                if session.send_environment(&self.environment).is_err() {
                    all_sent = false;
                }
            }
            if all_sent {
                self.environment_sent = self.environment.clone();
            }
        }
        if self.previews != self.previews_sent {
            let mut all_sent = true;
            for session in &mut self.sessions {
                if session
                    .send_overview_previews(&self.previews.0, self.previews.1)
                    .is_err()
                {
                    all_sent = false;
                }
            }
            if all_sent {
                self.previews_sent = self.previews.clone();
            }
        }
        if self.native_outputs != self.native_outputs_sent {
            let mut all_sent = true;
            for session in &mut self.sessions {
                if session.send_native_outputs(&self.native_outputs).is_err() {
                    all_sent = false;
                }
            }
            if all_sent {
                self.native_outputs_sent = self.native_outputs.clone();
            }
        }
        if self.outputs != self.outputs_sent {
            let mut all_sent = true;
            for session in &mut self.sessions {
                if session.send_outputs(&self.outputs).is_err() {
                    all_sent = false;
                }
            }
            if all_sent {
                self.outputs_sent = self.outputs.clone();
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
        // Accelerator presses go to every live session once (the shell
        // signals the grabbing D-Bus caller).
        for (action, time, mode) in std::mem::take(&mut self.accelerator_queue) {
            for session in &mut self.sessions {
                let _ = session.send_accelerator(action, time, mode);
            }
        }
        for menu in std::mem::take(&mut self.menu_queue) {
            for session in &mut self.sessions {
                let _ = session.send_window_menu(&menu);
            }
        }
        outcome
    }

    /// Try each aged peer's handshake once. Peers were accepted a full
    /// round ago, so a well-behaved peer's `Hello` is already waiting; a
    /// still-silent peer is dropped and must reconnect. One attempt per
    /// connection (`ControlConn` owns its stream, so a half-open
    /// handshake cannot be resumed) keeps silent peers from pinning slots.
    fn advance_pending(&mut self, model: &StateModel) {
        let pending = std::mem::take(&mut self.pending);
        let peers = std::mem::take(&mut self.pending_peers);
        let uid = our_uid();
        for (stream, peer) in pending.into_iter().zip(peers) {
            // The gate may have narrowed since accept (shell restart).
            if !self.gate.admits(peer, uid) {
                self.refused += 1;
                continue;
            }
            let conn = match ControlConn::new(stream) {
                Ok(conn) => conn,
                Err(_) => continue,
            };
            let validate = self.store.clone().validator(self.seat.clone());
            let validator =
                move |token: &ActivationToken, app_id: Option<&str>| validate(&token.0, app_id);
            let minter = std::rc::Rc::new(self.store.clone().minter(self.seat.clone()));
            let overview = self.overview.clone();
            let locked = self.locked.clone();
            let open = overview.get();
            if let Ok(mut session) =
                Session::handshake_with(conn, model, validator, minter, overview, locked)
            {
                // Newcomers join mid-state: tell them the intent and the
                // inventory now (best-effort; the poll broadcast covers
                // the rest). An empty inventory means unknown — never
                // send it; the shell keeps its legacy surfaces until a
                // real one arrives.
                let _ = session.send_overview(open);
                let _ = session.send_native_outputs(&self.native_outputs);
                if !self.outputs.is_empty() {
                    let _ = session.send_outputs(&self.outputs);
                }
                if !self.environment.is_empty() {
                    let _ = session.send_environment(&self.environment);
                    let _ = session.send_overview_previews(&self.previews.0, self.previews.1);
                }
                if self.pointer_output.is_some() {
                    let _ = session.send_window_menu(&Message::PointerOutput {
                        name: self.pointer_output.clone(),
                    });
                }
                self.sessions.push(session);
                self.session_peers.push(peer);
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
