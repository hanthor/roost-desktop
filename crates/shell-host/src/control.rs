//! Shell-side control client for the versioned shell control protocol.
//!
//! 001 spec R3 + ADR 0002: the shell host talks to the compositor over a
//! dedicated Unix socket whose frames are `u32-LE length + postcard body`
//! (see `roost-shell-control`). This client owns the shell side of that
//! conversation: the `Hello` handshake, full-`Snapshot` application into
//! [`ShellModel`], ordered `Changes` application with revision-gap
//! resnapshot, and activation commands carrying compositor-minted tokens.
//!
//! Transport behavior mirrors the server side: the stream is put in
//! nonblocking mode at construction and `WouldBlock` is always surfaced
//! to the caller (as [`ControlError::Io`]), never hidden behind a retry
//! loop, so the panel event loop stays responsive.
//!
//! Resnapshot rule (ADR 0002): the protocol has no dedicated
//! snapshot-request opcode, so re-sending `Hello` via
//! [`ControlClient::request_snapshot`] is the resync signal. Any revision
//! gap — a `Changes.from_revision` that does not match the held revision,
//! or a typed [`ErrorKind::RevisionGap`] frame — sets
//! [`ControlClient::needs_snapshot`]; the caller re-hellos and the next
//! `Snapshot` clears the flag. A fresh client (or a reconnected one)
//! starts with no revision and the flag set: it must handshake, then wait
//! for its first full snapshot before trusting any delta.
//!
//! Activation (GNOME overview parallel: activating a window from the
//! overview dismisses it) stops at the wire here on purpose: this client
//! sends the token-bearing `Command` and hands the caller the request id
//! for `CommandResult` correlation. Mapping the result back into
//! [`ShellModel::select_window`] and overview dismissal is later UI
//! wiring, not transport.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

use roost_shell_control::{
    ActivationToken, CommandKind, CommandStatus, DecodeError, ErrorKind, Message, SwitcherAction,
    WindowInfo, WorkspaceInfo, CURRENT_VERSION, MAX_FRAME_BYTES,
};

use crate::model::{ShellModel, SnapshotView, WindowEntry};

/// First shell-chosen command request id.
pub const INITIAL_REQUEST_ID: u64 = 1;

/// Ways the control conversation can fail.
///
/// `WouldBlock` arrives here as `Io` with
/// `kind() == ErrorKind::WouldBlock`: no complete frame is available yet
/// and the caller should retry once the socket is readable/writable.
#[derive(Debug)]
pub enum ControlError {
    /// Socket I/O failure, including surfaced `WouldBlock`.
    Io(io::Error),
    /// Frame failed schema decoding/validation (oversize, malformed,
    /// over-long title, stale major).
    Decode(DecodeError),
    /// The compositor answered with a typed [`Message::Error`] where a
    /// handshake reply was required.
    Remote {
        /// Machine-readable category.
        kind: ErrorKind,
        /// Compositor diagnostics (never sensitive content).
        message: String,
    },
    /// The peer closed the connection or sent a message the current step
    /// cannot use.
    Unexpected(String),
    /// [`ControlClient::send_activation`] needs a selected window to name
    /// as the activation target and the model has none.
    NoActiveWindow,
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "control socket I/O: {e}"),
            Self::Decode(e) => write!(f, "control decode: {e}"),
            Self::Remote { kind, message } => {
                write!(f, "compositor error {kind:?}: {message}")
            }
            Self::Unexpected(detail) => write!(f, "unexpected control message: {detail}"),
            Self::NoActiveWindow => {
                write!(f, "no selected window to activate")
            }
        }
    }
}

impl std::error::Error for ControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Decode(e) => Some(e),
            Self::Remote { .. } | Self::Unexpected(_) | Self::NoActiveWindow => None,
        }
    }
}

impl From<io::Error> for ControlError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<DecodeError> for ControlError {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}

/// What one handled inbound message meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handled {
    /// Compositor greeted back (e.g. after a resnapshot re-hello); the
    /// awaited full snapshot still follows.
    Hello,
    /// Full state replaced the model; [`ControlClient::needs_snapshot`]
    /// is now clear.
    Snapshot {
        /// Revision now held.
        revision: u64,
    },
    /// Ordered deltas applied on top of the held revision.
    Changes {
        /// Revision now held.
        to_revision: u64,
    },
    /// Compositor answered one command; match `id` against the value
    /// [`ControlClient::send_activation`] returned.
    CommandResult {
        /// Echo of the command's request id.
        id: u64,
        /// Outcome.
        status: CommandStatus,
    },
    /// Typed compositor error that is not a revision gap (logged/counted
    /// by the caller; the connection stays usable).
    ServerError {
        /// Category.
        kind: ErrorKind,
        /// Diagnostics.
        message: String,
    },
    /// Held state is stale: discard nothing yet, but re-request a full
    /// snapshot before trusting further deltas. The offending delta was
    /// not applied.
    Gap {
        /// Revision held when the gap was detected (`None` = no snapshot
        /// received yet on this connection).
        have: Option<u64>,
    },
    /// Compositor overview intent (002 R1): the shell-facing open state.
    /// Carries no revision — UI state, not model truth.
    Overview {
        /// Whether the overview should be open.
        open: bool,
    },
    /// Compositor Alt-Tab drive applied to the shell switcher (002
    /// workspaces). `selection` is the switcher selection after
    /// applying (the commit target, or `None` when closed/empty).
    /// Carries no revision — UI state, not model truth.
    Switcher {
        /// Drive event that was applied.
        action: SwitcherAction,
        /// Current switcher selection, if any.
        selection: Option<u64>,
    },
}

/// Shell-side control client over a connected Unix socket.
pub struct ControlClient {
    stream: UnixStream,
    read_buf: Vec<u8>,
    model: ShellModel,
    revision: Option<u64>,
    needs_snapshot: bool,
    next_request_id: u64,
    shadow_windows: Vec<WindowInfo>,
    shadow_workspaces: Vec<WorkspaceInfo>,
}

impl ControlClient {
    /// Connect to the compositor's control socket and wrap the stream.
    /// Sends nothing: call [`hello`](Self::hello) first, then await the
    /// full snapshot before trusting any delta.
    pub fn connect(path: &std::path::Path) -> io::Result<Self> {
        Self::new(UnixStream::connect(path)?)
    }

    /// Wrap a connected stream, switching it to nonblocking mode.
    ///
    /// Starts with no revision and [`ControlClient::needs_snapshot`] set:
    /// handshake first, then await the initial full snapshot.
    pub fn new(stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            read_buf: Vec::new(),
            model: ShellModel::new(),
            revision: None,
            needs_snapshot: true,
            next_request_id: INITIAL_REQUEST_ID,
            shadow_windows: Vec::new(),
            shadow_workspaces: Vec::new(),
        })
    }

    /// Shell-side view state (window list, overview flag).
    pub fn model(&self) -> &ShellModel {
        &self.model
    }

    /// Revision of the last applied snapshot or delta (`None` before the
    /// first snapshot).
    pub fn revision(&self) -> Option<u64> {
        self.revision
    }

    /// Whether a full snapshot must be (re)requested before deltas can be
    /// trusted. Set at construction, on every gap signal, and never
    /// cleared except by applying a `Snapshot`.
    pub fn needs_snapshot(&self) -> bool {
        self.needs_snapshot
    }

    /// Send our `Hello`. Pair with [`ControlClient::await_hello`], or use
    /// [`ControlClient::hello`] for both halves at once.
    pub fn send_hello(&mut self) -> Result<(), ControlError> {
        self.write_message(&Message::Hello {
            version: CURRENT_VERSION,
        })
    }

    /// Read one frame and require it to be a compatible `Hello` reply (or
    /// fail on the compositor's typed `Error`).
    ///
    /// A stale-major `Hello` never arrives as a message: schema decoding
    /// rejects it first, surfacing [`ControlError::Decode`].
    pub fn await_hello(&mut self) -> Result<(), ControlError> {
        match self.read_frame()? {
            Message::Hello { version } => {
                if version.is_compatible_with(&CURRENT_VERSION) {
                    Ok(())
                } else {
                    Err(ControlError::Remote {
                        kind: ErrorKind::IncompatibleVersion,
                        message: format!(
                            "peer {}.{} is newer than ours {}.{}",
                            version.major,
                            version.minor,
                            CURRENT_VERSION.major,
                            CURRENT_VERSION.minor
                        ),
                    })
                }
            }
            Message::Error { kind, message } => Err(ControlError::Remote { kind, message }),
            other => Err(ControlError::Unexpected(format!(
                "expected Hello reply, got {}",
                message_label(&other)
            ))),
        }
    }

    /// Handshake: send `Hello`, expect a compatible `Hello` reply.
    ///
    /// On a blocking stream (or a peer that already replied) this
    /// completes in one call; on a nonblocking stream with no reply
    /// waiting it surfaces `WouldBlock` from the read half — retry
    /// [`ControlClient::await_hello`] once the socket is readable.
    pub fn hello(&mut self) -> Result<(), ControlError> {
        self.send_hello()?;
        self.await_hello()
    }

    /// Ask for a full snapshot: re-send `Hello`, the resync signal (the
    /// protocol has no dedicated snapshot-request opcode). The flag stays
    /// set until the answering `Snapshot` is applied.
    pub fn request_snapshot(&mut self) -> Result<(), ControlError> {
        self.send_hello()
    }

    /// Activate one window by id with its latest token (002 T2).
    ///
    /// Selects in the shadow model first, so the activation target is
    /// explicit — callers holding their own view state (the host model)
    /// must not assume the shadow shares their selection. Unknown ids
    /// fail before anything is sent. Returns the request id for
    /// `CommandResult` correlation.
    pub fn activate_window(&mut self, window: u64) -> Result<u64, ControlError> {
        if !self.model.select_window(window) {
            return Err(ControlError::Unexpected(format!(
                "window {window} not in snapshot"
            )));
        }
        self.activate_selected()
    }

    /// Activate the currently selected window with its latest token.
    ///
    /// Overview flow: [`select_window`](crate::model::ShellModel::select_window)
    /// first, then this. The token comes from the shadow `WindowInfo` of
    /// the last snapshot or delta — never cached across resyncs, since
    /// every snapshot mints fresh one-use tokens. Returns the request id
    /// for `CommandResult` correlation.
    pub fn activate_selected(&mut self) -> Result<u64, ControlError> {
        let window = self.model.selected().ok_or(ControlError::NoActiveWindow)?;
        let token = self
            .shadow_windows
            .iter()
            .find(|w| w.id == window)
            .map(|w| ActivationToken::new(w.activation_token.clone()))
            .ok_or_else(|| {
                ControlError::Unexpected(format!("no activation token for window {window}"))
            })?;
        self.send_activation(token)
    }

    /// Ask the compositor to flip the overview intent (002 T2).
    ///
    /// The overview answers Enter-on-empty and post-activation dismissal
    /// through this: the shell never flips its flag locally and waits
    /// for the compositor's `Overview` broadcast instead. Returns the
    /// request id for `CommandResult` correlation.
    pub fn toggle_overview(&mut self) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::ToggleOverview,
        })?;
        Ok(id)
    }

    /// Ask the compositor to close a window (dock quit). Like
    /// `toggle_overview` this carries no activation token: closing is
    /// not focus, and unknown ids come back `Denied`. Callers must
    /// already hold the id from their own view state. Returns the
    /// request id for `CommandResult` correlation.
    pub fn close_window(&mut self, window: u64) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::CloseWindow { window },
        })?;
        Ok(id)
    }

    /// Request activation of the currently selected window.
    ///
    /// Builds the schema `Command` carrying the compositor-minted
    /// `token` (a raw Wayland serial is never sufficient) and returns the
    /// request id so the caller can correlate the later `CommandResult`.
    /// The target is the model's selected window — the overview-list
    /// behavior this mirrors always activates out of an explicit
    /// selection. Prefer [`activate_selected`](Self::activate_selected),
    /// which sources the token from the latest shadow state.
    pub fn send_activation(&mut self, token: ActivationToken) -> Result<u64, ControlError> {
        let window = self.model.selected().ok_or(ControlError::NoActiveWindow)?;
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::ActivateWindow { window, token },
        })?;
        Ok(id)
    }

    /// Read one frame and apply it, surfacing `WouldBlock` when no
    /// complete frame is available yet.
    pub fn poll(&mut self) -> Result<Handled, ControlError> {
        let msg = self.read_frame()?;
        self.handle_message(msg)
    }

    /// Apply one already-decoded message to client state.
    fn handle_message(&mut self, msg: Message) -> Result<Handled, ControlError> {
        match msg {
            Message::Hello { version } => {
                if version.is_compatible_with(&CURRENT_VERSION) {
                    Ok(Handled::Hello)
                } else {
                    // Same-major decode already passed, so this is a newer
                    // minor we cannot speak: report it as typed, keep the
                    // connection state untouched.
                    Ok(Handled::ServerError {
                        kind: ErrorKind::IncompatibleVersion,
                        message: format!(
                            "peer {}.{} is newer than ours {}.{}",
                            version.major,
                            version.minor,
                            CURRENT_VERSION.major,
                            CURRENT_VERSION.minor
                        ),
                    })
                }
            }
            Message::Snapshot {
                revision,
                windows,
                workspaces,
            } => {
                self.shadow_windows = windows;
                self.shadow_workspaces = workspaces;
                self.model.apply_snapshot_view(snapshot_view(
                    &self.shadow_windows,
                    &self.shadow_workspaces,
                ));
                self.revision = Some(revision);
                self.needs_snapshot = false;
                Ok(Handled::Snapshot { revision })
            }
            Message::Changes {
                from_revision,
                to_revision,
                ops,
            } => {
                if self.revision != Some(from_revision) {
                    // Resnapshot rule: never apply a delta onto the wrong
                    // base. Flag a fresh snapshot; the delta is dropped.
                    self.needs_snapshot = true;
                    return Ok(Handled::Gap {
                        have: self.revision,
                    });
                }
                apply_ops(&mut self.shadow_windows, &mut self.shadow_workspaces, &ops);
                self.model.apply_snapshot_view(snapshot_view(
                    &self.shadow_windows,
                    &self.shadow_workspaces,
                ));
                self.revision = Some(to_revision);
                Ok(Handled::Changes { to_revision })
            }
            Message::CommandResult { id, status } => Ok(Handled::CommandResult { id, status }),
            Message::Overview { open } => {
                // 002 R1: overview intent is UI state, not model truth —
                // never touches revision, `needs_snapshot`, or the window
                // list.
                self.model.set_overview_open(open);
                Ok(Handled::Overview { open })
            }
            Message::Switcher { action } => {
                // 002 workspaces: switcher drive is UI state too — never
                // touches revision or `needs_snapshot`. A commit selects
                // and activates through the token path, like the
                // overview's Enter path.
                let selection = match action {
                    SwitcherAction::Step { forward } => self.model.switcher_step(forward),
                    SwitcherAction::Cancel => {
                        self.model.switcher_cancel();
                        None
                    }
                    SwitcherAction::Commit => self.model.switcher_commit(),
                };
                if matches!(action, SwitcherAction::Commit) {
                    if let Some(id) = selection {
                        self.activate_window(id)?;
                    }
                }
                Ok(Handled::Switcher { action, selection })
            }
            Message::Error { kind, message } => {
                if kind == ErrorKind::RevisionGap {
                    // Compositor-side gap signal: same handling as a local
                    // `from_revision` mismatch.
                    self.needs_snapshot = true;
                    Ok(Handled::Gap {
                        have: self.revision,
                    })
                } else {
                    Ok(Handled::ServerError { kind, message })
                }
            }
            Message::Command { id, .. } => Err(ControlError::Unexpected(format!(
                "compositor sent shell-side Command id {id}"
            ))),
        }
    }

    /// Encode and write one frame. `WouldBlock` propagates to the caller.
    fn write_message(&mut self, msg: &Message) -> Result<(), ControlError> {
        let frame = roost_shell_control::encode_frame(msg);
        self.stream.write_all(&frame)?;
        Ok(())
    }

    /// Read one complete frame, buffering partial reads. Returns
    /// `WouldBlock` (as [`ControlError::Io`]) when no complete frame is
    /// available yet; oversize/validation failures surface immediately
    /// without waiting for more bytes.
    fn read_frame(&mut self) -> Result<Message, ControlError> {
        loop {
            if self.read_buf.len() >= 4 {
                let mut prefix = [0u8; 4];
                prefix.copy_from_slice(&self.read_buf[..4]);
                let len = u32::from_le_bytes(prefix) as usize;
                if len > MAX_FRAME_BYTES {
                    return Err(ControlError::Decode(DecodeError::Oversize {
                        len,
                        max: MAX_FRAME_BYTES,
                    }));
                }
                if self.read_buf.len() >= 4 + len {
                    let msg = roost_shell_control::decode_frame(&self.read_buf[..4 + len])?;
                    self.read_buf.drain(..4 + len);
                    return Ok(msg);
                }
            }
            let mut chunk = [0u8; 8192];
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(ControlError::Unexpected(
                        "control connection closed".to_owned(),
                    ));
                }
                Ok(n) => self.read_buf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(ControlError::Io(e)),
            }
        }
    }

    /// Hand out the next shell-chosen request id (never zero).
    fn alloc_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        if self.next_request_id == 0 {
            self.next_request_id = INITIAL_REQUEST_ID;
        }
        id
    }
}

/// Short label for unexpected-message diagnostics.
fn message_label(msg: &Message) -> &'static str {
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

/// Map authoritative schema state into the plain shell-host view.
///
/// `focused` becomes the list `active` flag (single-selection is
/// re-normalized by the model); workspace ids narrow `u64 -> u32` with
/// saturation to `u32::MAX` on overflow — ids are compositor-opaque, and
/// saturating keeps an absurd id visible-but-harmless instead of silently
/// colliding with a real workspace. Titles pass through byte-for-byte
/// (length already capped at decode); rendering must still treat them as
/// untrusted text.
fn snapshot_view(windows: &[WindowInfo], workspaces: &[WorkspaceInfo]) -> SnapshotView {
    SnapshotView {
        windows: windows
            .iter()
            .map(|w| {
                WindowEntry::new(w.id, w.title.clone(), w.focused)
                    .with_workspace(u32::try_from(w.workspace).unwrap_or(u32::MAX))
                    .with_app_id(w.app_id.clone())
            })
            .collect(),
        workspaces: workspaces
            .iter()
            .map(|ws| u32::try_from(ws.id).unwrap_or(u32::MAX))
            .collect(),
        active_workspace: workspaces
            .iter()
            .find(|ws| ws.active)
            .map(|ws| u32::try_from(ws.id).unwrap_or(u32::MAX))
            .unwrap_or(0),
    }
}

/// Apply ordered state ops onto the shadow copies the view is rebuilt
/// from. `WindowOpened` restates (same id replaces, unknown id appends);
/// focus is single; moves retarget the shadow workspace; workspace
/// changes restate by id. `ShellModel` carries no per-window workspace
/// field, so moves surface in the model only via the workspace set — the
/// shadow keeps the full per-window truth for the next rebuild.
fn apply_ops(
    shadow_windows: &mut Vec<WindowInfo>,
    shadow_workspaces: &mut Vec<WorkspaceInfo>,
    ops: &[roost_shell_control::StateOp],
) {
    use roost_shell_control::StateOp;
    for op in ops {
        match op {
            StateOp::WindowOpened(info) => {
                if let Some(slot) = shadow_windows.iter_mut().find(|w| w.id == info.id) {
                    *slot = info.clone();
                } else {
                    shadow_windows.push(info.clone());
                }
            }
            StateOp::WindowClosed { id } => {
                shadow_windows.retain(|w| w.id != *id);
            }
            StateOp::WindowFocused { id } => {
                for w in shadow_windows.iter_mut() {
                    w.focused = w.id == *id;
                }
            }
            StateOp::WindowMoved { id, workspace } => {
                if let Some(w) = shadow_windows.iter_mut().find(|w| w.id == *id) {
                    w.workspace = *workspace;
                }
            }
            StateOp::WorkspaceChanged(info) => {
                // Active is exclusive: a newly-active workspace clears
                // the flag everywhere else, so a single op per switch
                // converges the shadow without a resnapshot.
                if info.active {
                    for ws in shadow_workspaces.iter_mut() {
                        ws.active = ws.id == info.id;
                    }
                }
                if let Some(slot) = shadow_workspaces.iter_mut().find(|ws| ws.id == info.id) {
                    *slot = info.clone();
                } else {
                    shadow_workspaces.push(info.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roost_shell_control::{decode_frame, encode_frame, ProtocolVersion, WindowId, WorkspaceId};

    fn pair() -> (ControlClient, UnixStream) {
        let (ours, peer) = UnixStream::pair().expect("socketpair");
        let client = ControlClient::new(ours).expect("nonblocking client");
        (client, peer)
    }

    /// Blocking one-frame read for the in-test fake server.
    fn server_read(peer: &mut UnixStream) -> Message {
        let mut prefix = [0u8; 4];
        peer.read_exact(&mut prefix).expect("server prefix");
        let len = u32::from_le_bytes(prefix) as usize;
        let mut body = vec![0u8; len];
        peer.read_exact(&mut body).expect("server body");
        let mut frame = prefix.to_vec();
        frame.extend_from_slice(&body);
        decode_frame(&frame).expect("server decode")
    }

    fn server_write(peer: &mut UnixStream, msg: &Message) {
        peer.write_all(&encode_frame(msg)).expect("server write");
    }

    fn window(id: WindowId, title: &str, workspace: WorkspaceId, focused: bool) -> WindowInfo {
        WindowInfo {
            id,
            title: title.to_owned(),
            app_id: Some("org.example.App".to_owned()),
            workspace,
            focused,
            activation_token: format!("token-{id}"),
        }
    }

    fn workspace(id: WorkspaceId, active: bool) -> WorkspaceInfo {
        WorkspaceInfo {
            id,
            name: Some(format!("ws{id}")),
            active,
        }
    }

    /// Handshake against the fake server; returns the client plus the
    /// server end for further scripted traffic.
    fn handshook() -> (ControlClient, UnixStream) {
        let (mut client, mut peer) = pair();
        assert!(client.needs_snapshot());
        assert_eq!(client.revision(), None);
        client.send_hello().expect("send hello");
        assert_eq!(
            server_read(&mut peer),
            Message::Hello {
                version: CURRENT_VERSION
            }
        );
        server_write(
            &mut peer,
            &Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        client.await_hello().expect("hello reply");
        (client, peer)
    }

    fn snapshot_rev7() -> Message {
        Message::Snapshot {
            revision: 7,
            windows: vec![
                window(7, "Terminal", 1, true),
                window(8, "Browser", 1, false),
            ],
            workspaces: vec![workspace(1, true), workspace(2, false)],
        }
    }

    #[test]
    fn hello_roundtrip_against_fake_server() {
        let (client, _peer) = handshook();
        // Handshake alone brings no state: still awaiting first snapshot.
        assert!(client.needs_snapshot());
        assert_eq!(client.revision(), None);
        assert!(client.model().windows().is_empty());
    }

    #[test]
    fn hello_convenience_succeeds_when_reply_is_waiting() {
        let (ours, mut peer) = UnixStream::pair().expect("socketpair");
        // Fake server answers eagerly so one blocking-style call works.
        let mut client = ControlClient::new(ours).expect("client");
        client.send_hello().expect("send");
        assert_eq!(
            server_read(&mut peer),
            Message::Hello {
                version: CURRENT_VERSION
            }
        );
        server_write(
            &mut peer,
            &Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        client.await_hello().expect("await");
    }

    #[test]
    fn hello_stale_major_reports_typed_error() {
        // Typed Error reply carrying the version category.
        let (mut client, mut peer) = pair();
        client.send_hello().expect("send");
        assert!(matches!(server_read(&mut peer), Message::Hello { .. }));
        server_write(
            &mut peer,
            &Message::Error {
                kind: ErrorKind::IncompatibleVersion,
                message: "need major 9".to_owned(),
            },
        );
        match client.await_hello() {
            Err(ControlError::Remote { kind, .. }) => {
                assert_eq!(kind, ErrorKind::IncompatibleVersion);
            }
            other => panic!("expected typed Remote error, got {other:?}"),
        }

        // A stale-major Hello frame itself never decodes: the schema
        // rejects it before any message is produced.
        let (mut client, mut peer) = pair();
        client.send_hello().expect("send");
        let _ = server_read(&mut peer);
        server_write(
            &mut peer,
            &Message::Hello {
                version: ProtocolVersion::new(9, 0),
            },
        );
        match client.await_hello() {
            Err(ControlError::Decode(DecodeError::IncompatibleVersion { got, .. })) => {
                assert_eq!(got, ProtocolVersion::new(9, 0));
            }
            other => panic!("expected decode IncompatibleVersion, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_applies_into_model() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        match client.poll().expect("poll snapshot") {
            Handled::Snapshot { revision } => assert_eq!(revision, 7),
            other => panic!("expected Snapshot, got {other:?}"),
        }
        assert_eq!(client.revision(), Some(7));
        assert!(!client.needs_snapshot());
        let model = client.model();
        assert_eq!(model.windows().len(), 2);
        assert_eq!(model.selected(), Some(7));
        assert_eq!(model.workspaces(), &[1, 2]);
        assert!(!model.is_overview_open(), "snapshot keeps UI flag");
    }

    #[test]
    fn overview_intent_sets_flag_without_touching_revision() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));
        server_write(&mut peer, &Message::Overview { open: true });
        match client.poll().expect("poll overview") {
            Handled::Overview { open } => assert!(open),
            other => panic!("expected Overview, got {other:?}"),
        }
        let model = client.model();
        assert!(model.is_overview_open());
        assert_eq!(model.selected(), Some(7), "intent keeps selection");
        assert_eq!(model.windows().len(), 2, "intent keeps the list");
        assert_eq!(client.revision(), Some(7), "intent is not model truth");
        assert!(!client.needs_snapshot());

        server_write(&mut peer, &Message::Overview { open: false });
        assert!(matches!(
            client.poll().expect("poll overview close"),
            Handled::Overview { open: false }
        ));
        assert!(!client.model().is_overview_open());
        assert_eq!(client.revision(), Some(7));
    }

    #[test]
    fn changes_apply_deltas_and_bump_revision() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));
        server_write(
            &mut peer,
            &Message::Changes {
                from_revision: 7,
                to_revision: 9,
                ops: vec![
                    roost_shell_control::StateOp::WindowOpened(window(9, "Editor", 2, false)),
                    roost_shell_control::StateOp::WindowFocused { id: 9 },
                    roost_shell_control::StateOp::WindowClosed { id: 7 },
                ],
            },
        );
        match client.poll().expect("poll changes") {
            Handled::Changes { to_revision } => assert_eq!(to_revision, 9),
            other => panic!("expected Changes, got {other:?}"),
        }
        assert_eq!(client.revision(), Some(9));
        let model = client.model();
        assert_eq!(model.selected(), Some(9), "focus delta moves selection");
        assert_eq!(
            model.windows().iter().map(|w| w.id).collect::<Vec<_>>(),
            vec![8, 9],
            "closed window leaves the list, opened one joins"
        );
    }

    #[test]
    fn gap_triggers_resnapshot_request() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));

        // Stale base: the delta is dropped, state is untouched, and the
        // client flags itself for a fresh snapshot.
        server_write(
            &mut peer,
            &Message::Changes {
                from_revision: 3,
                to_revision: 4,
                ops: vec![roost_shell_control::StateOp::WindowClosed { id: 8 }],
            },
        );
        match client.poll().expect("poll gap") {
            Handled::Gap { have } => assert_eq!(have, Some(7)),
            other => panic!("expected Gap, got {other:?}"),
        }
        assert!(client.needs_snapshot());
        assert_eq!(client.revision(), Some(7), "gap keeps old revision");
        assert_eq!(client.model().windows().len(), 2, "gap applies nothing");

        // Resnapshot: re-hello goes out on the wire; the answering full
        // snapshot clears the flag.
        client.request_snapshot().expect("re-hello");
        assert_eq!(
            server_read(&mut peer),
            Message::Hello {
                version: CURRENT_VERSION
            }
        );
        server_write(
            &mut peer,
            &Message::Snapshot {
                revision: 10,
                windows: vec![window(8, "Browser", 1, true)],
                workspaces: vec![workspace(1, true)],
            },
        );
        assert!(matches!(
            client.poll().expect("resnapshot"),
            Handled::Snapshot { revision: 10 }
        ));
        assert!(!client.needs_snapshot());
        assert_eq!(client.model().selected(), Some(8));
    }

    #[test]
    fn compositor_gap_signal_triggers_resnapshot() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));
        server_write(
            &mut peer,
            &Message::Error {
                kind: ErrorKind::RevisionGap,
                message: "have 7, need 12; resnapshot".to_owned(),
            },
        );
        match client.poll().expect("poll gap signal") {
            Handled::Gap { have } => assert_eq!(have, Some(7)),
            other => panic!("expected Gap, got {other:?}"),
        }
        assert!(client.needs_snapshot());
    }

    #[test]
    fn activation_returns_id_correlated_with_result() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));

        let token = ActivationToken::new("opaque-token-123".to_owned());
        let id = client.send_activation(token.clone()).expect("activate");
        assert_eq!(id, INITIAL_REQUEST_ID);
        match server_read(&mut peer) {
            Message::Command {
                id: wire_id,
                kind:
                    CommandKind::ActivateWindow {
                        window,
                        token: wire_token,
                    },
            } => {
                assert_eq!(wire_id, id, "wire id matches returned id");
                assert_eq!(window, 7, "selected window is the target");
                assert_eq!(wire_token, token);
            }
            other => panic!("expected ActivateWindow command, got {other:?}"),
        }

        // A second activation takes the next id; results correlate back
        // even when they arrive out of order with other traffic.
        let id2 = client
            .send_activation(ActivationToken::new("second-token".to_owned()))
            .expect("second activate");
        assert_ne!(id, id2);
        let _ = server_read(&mut peer);
        server_write(
            &mut peer,
            &Message::CommandResult {
                id,
                status: CommandStatus::Applied,
            },
        );
        match client.poll().expect("poll result") {
            Handled::CommandResult { id: got, status } => {
                assert_eq!(got, id, "result echoes the request id");
                assert_eq!(status, CommandStatus::Applied);
            }
            other => panic!("expected CommandResult, got {other:?}"),
        }
    }

    #[test]
    fn close_window_sends_id_command_with_fresh_id() {
        let (mut client, mut peer) = handshook();
        let id = client.close_window(9).expect("close");
        assert_eq!(id, INITIAL_REQUEST_ID);
        match server_read(&mut peer) {
            Message::Command {
                id: wire_id,
                kind: CommandKind::CloseWindow { window },
            } => {
                assert_eq!(wire_id, id, "wire id matches returned id");
                assert_eq!(window, 9, "close names the dock's window");
            }
            other => panic!("expected CloseWindow command, got {other:?}"),
        }
        // Close needs no selection: it works on an empty model too.
        assert_eq!(client.model().selected(), None);
    }

    #[test]
    fn snapshot_carries_app_id_into_entries() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));
        let entry = client
            .model()
            .windows()
            .iter()
            .find(|w| w.id == 7)
            .expect("window 7 in snapshot");
        assert_eq!(entry.app_id.as_deref(), Some("org.example.App"));
    }

    #[test]
    fn toggle_overview_sends_unit_command_with_fresh_id() {
        let (mut client, mut peer) = handshook();
        let id = client.toggle_overview().expect("toggle");
        assert_eq!(id, INITIAL_REQUEST_ID);
        match server_read(&mut peer) {
            Message::Command {
                id: wire_id,
                kind: CommandKind::ToggleOverview,
            } => assert_eq!(wire_id, id, "wire id matches returned id"),
            other => panic!("expected ToggleOverview command, got {other:?}"),
        }
        // Toggle needs no selection: it works on an empty model too.
        assert_eq!(client.model().selected(), None);
    }

    #[test]
    fn activate_window_targets_by_id_not_shadow_selection() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));
        // Shadow selection is window 7; activating 8 must name 8.
        let id = client.activate_window(8).expect("activate by id");
        match server_read(&mut peer) {
            Message::Command {
                id: wire_id,
                kind: CommandKind::ActivateWindow { window, .. },
            } => {
                assert_eq!(wire_id, id);
                assert_eq!(window, 8);
            }
            other => panic!("expected ActivateWindow for 8, got {other:?}"),
        }
        // Unknown ids fail before anything is sent.
        match client.activate_window(99) {
            Err(ControlError::Unexpected(_)) => {}
            other => panic!("expected Unknown-window refusal, got {other:?}"),
        }
    }

    #[test]
    fn activation_without_selection_is_refused_locally() {
        let (mut client, _peer) = handshook();
        assert_eq!(client.model().selected(), None);
        match client.send_activation(ActivationToken::new("token".to_owned())) {
            Err(ControlError::NoActiveWindow) => {}
            other => panic!("expected NoActiveWindow, got {other:?}"),
        }
    }

    #[test]
    fn idle_poll_surfaces_would_block() {
        let (mut client, _peer) = handshook();
        match client.poll() {
            Err(ControlError::Io(e)) => {
                assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
            }
            other => panic!("expected WouldBlock, got {other:?}"),
        }
    }
}
